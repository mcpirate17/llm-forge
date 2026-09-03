//! Deterministic policy core for bounded A2A message retention.
//!
//! Python intentionally retains the explicit filesystem and SQLite transaction
//! boundaries.  Evidence traversal, content-drift verification, canonical
//! manifests, receipt hashes, and byte accounting live here.

use std::collections::{BTreeSet, HashSet};

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

const POLICY_VERSION: u64 = 2;
const TOMBSTONE_BODY: &str = "[compacted: resolved A2A content retained by digest]";
const MAX_EVIDENCE_FILES: usize = 512;
const MAX_EVIDENCE_FILE_BYTES: usize = 2 << 20;
const MAX_EVIDENCE_TOTAL_BYTES: usize = 32 << 20;
const MAX_EVIDENCE_NODES: usize = 500_000;
const EVIDENCE_PATTERNS: [&str; 2] = [
    "research/reports/**/*gate*.json",
    "research/reports/**/*receipt*.json",
];
const STRUCTURED_FIELDS: [&str; 9] = [
    "kind",
    "thread_id",
    "summary",
    "status",
    "requires_response",
    "supersedes",
    "gate",
    "fingerprint",
    "artifact_paths",
];

fn sha256(value: &[u8]) -> String {
    format!("{:x}", Sha256::digest(value))
}

fn canonical_bytes(value: &Value) -> Result<Vec<u8>, String> {
    serde_json::to_vec(value).map_err(|error| error.to_string())
}

fn canonical_string(value: &Value) -> Result<String, String> {
    String::from_utf8(canonical_bytes(value)?).map_err(|error| error.to_string())
}

fn required<'py, T: FromPyObjectOwned<'py>>(row: &Bound<'py, PyDict>, field: &str) -> PyResult<T> {
    row.get_item(field)?
        .ok_or_else(|| PyValueError::new_err(format!("retention row is missing field {field:?}")))?
        .extract::<T>()
        .map_err(|_| {
            PyValueError::new_err(format!("retention row field {field:?} has invalid type"))
        })
}

#[derive(Debug)]
struct RetentionRow {
    message_id: String,
    direction: String,
    sender: String,
    recipient: String,
    body: String,
    data_json: Option<String>,
    created_at: String,
    received_at: Option<String>,
    read_at: String,
    resolved_at: Option<String>,
    superseded_at: Option<String>,
    thread_id: String,
    summary: String,
    protocol_status: String,
    requires_response: bool,
    retention_class: String,
    body_sha256: String,
    body_bytes: u64,
    data_sha256: Option<String>,
    data_bytes: u64,
}

impl RetentionRow {
    fn extract(row: &Bound<'_, PyDict>) -> PyResult<Self> {
        Ok(Self {
            message_id: required(row, "message_id")?,
            direction: required(row, "direction")?,
            sender: required(row, "sender")?,
            recipient: required(row, "recipient")?,
            body: required(row, "body")?,
            data_json: required(row, "data_json")?,
            created_at: required(row, "created_at")?,
            received_at: required(row, "received_at")?,
            read_at: required(row, "read_at")?,
            resolved_at: required(row, "resolved_at")?,
            superseded_at: required(row, "superseded_at")?,
            thread_id: required(row, "thread_id")?,
            summary: required(row, "summary")?,
            protocol_status: required(row, "protocol_status")?,
            requires_response: required(row, "requires_response")?,
            retention_class: required(row, "retention_class")?,
            body_sha256: required(row, "body_sha256")?,
            body_bytes: required(row, "body_bytes")?,
            data_sha256: required(row, "data_sha256")?,
            data_bytes: required(row, "data_bytes")?,
        })
    }
}

fn structured_projection(data_json: Option<&str>) -> Result<Value, String> {
    let Some(data_json) = data_json else {
        return Ok(Value::Null);
    };
    let value = serde_json::from_str::<Value>(data_json)
        .map_err(|_| "eligible protocol message has malformed data_json".to_owned())?;
    let source = value
        .as_object()
        .ok_or_else(|| "eligible protocol message data_json is not an object".to_owned())?;
    let mut projected = Map::new();
    for field in STRUCTURED_FIELDS {
        if let Some(value) = source.get(field) {
            projected.insert(field.to_owned(), value.clone());
        }
    }
    Ok(Value::Object(projected))
}

fn manifest(row: &RetentionRow, compacted_at: &str) -> Result<Value, String> {
    let body_bytes = row.body.as_bytes();
    let data_bytes = row.data_json.as_deref().unwrap_or_default().as_bytes();
    let body_sha256 = sha256(body_bytes);
    let data_sha256 = row.data_json.as_ref().map(|_| sha256(data_bytes));
    if body_sha256 != row.body_sha256
        || body_bytes.len() as u64 != row.body_bytes
        || data_sha256 != row.data_sha256
        || data_bytes.len() as u64 != row.data_bytes
    {
        return Err(format!(
            "message {:?} content drifted from lifecycle metadata",
            row.message_id
        ));
    }

    let mut value = json!({
        "schema_version": 1,
        "policy_version": POLICY_VERSION,
        "authority": "deterministic-a2a-retention",
        "message_id": row.message_id,
        "direction": row.direction,
        "sender": row.sender,
        "recipient": row.recipient,
        "created_at": row.created_at,
        "received_at": row.received_at,
        "read_at": row.read_at,
        "resolved_at": row.resolved_at,
        "superseded_at": row.superseded_at,
        "thread_id": row.thread_id,
        "summary": row.summary,
        "protocol_status": row.protocol_status,
        "requires_response": row.requires_response,
        "retention_class": row.retention_class,
        "structured": structured_projection(row.data_json.as_deref())?,
        "body_sha256": body_sha256,
        "body_bytes": body_bytes.len(),
        "data_sha256": data_sha256,
        "data_bytes": data_bytes.len(),
        "compacted_at": compacted_at,
    });
    let digest = sha256(&canonical_bytes(&value)?);
    value
        .as_object_mut()
        .expect("json object literal")
        .insert("manifest_sha256".to_owned(), Value::String(digest));
    Ok(value)
}

fn event_sha256(message_id: &str, manifest_sha256: &str) -> String {
    sha256(format!("{POLICY_VERSION}\0inbound\0{message_id}\0{manifest_sha256}").as_bytes())
}

#[pyfunction]
fn a2a_retention_evidence_native(
    py: Python<'_>,
    candidate_ids: Vec<String>,
    files: Vec<(String, Py<PyBytes>)>,
) -> PyResult<(Vec<String>, Vec<String>, String)> {
    let candidate_ids = candidate_ids.into_iter().collect::<HashSet<_>>();
    if files.len() > MAX_EVIDENCE_FILES {
        return Err(PyValueError::new_err(format!(
            "evidence scan found {} files; maximum is {MAX_EVIDENCE_FILES}",
            files.len()
        )));
    }

    let mut protected = BTreeSet::new();
    let mut provenance = Vec::with_capacity(files.len());
    let mut total_bytes = 0usize;
    let mut total_nodes = 0usize;
    for (path, raw) in files {
        let raw = raw.bind(py).as_bytes();
        if raw.len() > MAX_EVIDENCE_FILE_BYTES {
            return Err(PyValueError::new_err(format!(
                "evidence file exceeds {MAX_EVIDENCE_FILE_BYTES} bytes: {path}"
            )));
        }
        total_bytes = total_bytes.saturating_add(raw.len());
        if total_bytes > MAX_EVIDENCE_TOTAL_BYTES {
            return Err(PyValueError::new_err(format!(
                "evidence scan exceeds {MAX_EVIDENCE_TOTAL_BYTES} total bytes"
            )));
        }
        let payload = serde_json::from_slice::<Value>(raw).map_err(|_| {
            PyValueError::new_err(format!("evidence file is malformed JSON: {path}"))
        })?;
        let mut stack = vec![&payload];
        while let Some(node) = stack.pop() {
            total_nodes += 1;
            if total_nodes > MAX_EVIDENCE_NODES {
                return Err(PyValueError::new_err(format!(
                    "evidence scan exceeds {MAX_EVIDENCE_NODES} JSON nodes"
                )));
            }
            match node {
                Value::Object(values) => {
                    for (key, value) in values {
                        if candidate_ids.contains(key) {
                            protected.insert(key.clone());
                        }
                        stack.push(value);
                    }
                }
                Value::Array(values) => stack.extend(values),
                Value::String(value) if candidate_ids.contains(value) => {
                    protected.insert(value.clone());
                }
                _ => {}
            }
        }
        provenance.push(json!({
            "path": path,
            "bytes": raw.len(),
            "sha256": sha256(raw),
        }));
    }
    let snapshot = json!({
        "policy": "bounded-a2a-gate-receipt-evidence-v1",
        "patterns": EVIDENCE_PATTERNS,
        "files": provenance,
    });
    let paths = snapshot["files"]
        .as_array()
        .expect("array")
        .iter()
        .map(|item| item["path"].as_str().expect("path string").to_owned())
        .collect();
    let snapshot_sha256 = sha256(&canonical_bytes(&snapshot).map_err(PyValueError::new_err)?);
    Ok((paths, protected.into_iter().collect(), snapshot_sha256))
}

#[pyfunction]
#[allow(clippy::type_complexity)]
fn a2a_retention_manifests_native(
    rows: Vec<Bound<'_, PyDict>>,
    compacted_at: &str,
) -> PyResult<(
    Vec<(String, String, String, String, u64, u64)>,
    u64,
    u64,
    u64,
)> {
    let row_count = rows.len();
    let mut items = Vec::with_capacity(rows.len());
    let mut original_content_bytes = 0u64;
    for row in rows {
        let row = RetentionRow::extract(&row)?;
        let manifest = manifest(&row, compacted_at).map_err(PyValueError::new_err)?;
        let manifest_sha256 = manifest["manifest_sha256"]
            .as_str()
            .expect("manifest digest string")
            .to_owned();
        let event_sha256 = event_sha256(&row.message_id, &manifest_sha256);
        original_content_bytes += row.body_bytes + row.data_bytes;
        items.push((
            row.message_id,
            canonical_string(&manifest).map_err(PyValueError::new_err)?,
            manifest_sha256,
            event_sha256,
            row.body_bytes,
            row.data_bytes,
        ));
    }
    let tombstone_bytes = (TOMBSTONE_BODY.len() * row_count) as u64;
    Ok((
        items,
        original_content_bytes,
        tombstone_bytes,
        original_content_bytes.saturating_sub(tombstone_bytes),
    ))
}

pub(crate) fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(a2a_retention_evidence_native, module)?)?;
    module.add_function(wrap_pyfunction!(a2a_retention_manifests_native, module)?)?;
    Ok(())
}
