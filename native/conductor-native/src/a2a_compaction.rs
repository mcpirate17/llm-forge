//! Deterministic, bounded A2A message compaction.
//!
//! Python owns only conversion of mapping-like rows at the extension boundary.
//! Validation, normalization, hashing, deduplication, ordering, and thread
//! compaction live here so the long-lived A2A service does not use Python as a
//! systems runtime for its receipt state machine.

use std::collections::{BTreeMap, BTreeSet, HashMap};

#[cfg(feature = "python")]
use pyo3::exceptions::PyValueError;
#[cfg(feature = "python")]
use pyo3::prelude::*;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::text_normalization::{normalized_text, python_whitespace};

const SCHEMA_VERSION: u64 = 1;
const AUTHORITY: &str = "deterministic-a2a-compaction";
const MAX_PROTOCOL_SUMMARY_BYTES: usize = 1_024;
const MAX_COMPACT_SUMMARY_BYTES: usize = 320;
const MAX_IDENTIFIER_CHARS: usize = 128;
const MAX_SUPERSEDES: usize = 32;
const MAX_INPUT_MESSAGES: usize = 256;
const MAX_THREADS: u64 = 64;
const MAX_DETAILED_MESSAGES: u64 = 32;
const MAX_RAW_FIELD_BYTES: usize = 1 << 20;
const MAX_METADATA_BYTES: usize = 512;
const MAX_DATA_KIND_BYTES: usize = 64;

const COORDINATION_V2_FIELDS: [&str; 6] = [
    "kind",
    "thread_id",
    "summary",
    "status",
    "requires_response",
    "supersedes",
];
const COORDINATION_V2_OPTIONAL_FIELDS: [&str; 5] = [
    "thread_id",
    "summary",
    "status",
    "requires_response",
    "supersedes",
];
const COORDINATION_STATUSES: [&str; 6] = [
    "open",
    "in_progress",
    "blocked",
    "resolved",
    "superseded",
    "informational",
];

#[derive(Debug)]
struct Coordination {
    thread_id: Option<String>,
    summary: Option<String>,
    status: Option<String>,
    requires_response: Option<bool>,
    supersedes: Vec<String>,
}

#[derive(Debug)]
struct Row {
    message_id: String,
    direction: String,
    sender: String,
    recipient: String,
    body: String,
    data_json: Option<String>,
    created_at: String,
    received_at: Option<String>,
    delivery_status: String,
    status_reason: Option<String>,
    read_at: Option<String>,
}

#[cfg(feature = "python")]
fn parse_json(source: &str) -> Result<Value, String> {
    serde_json::from_str(source).map_err(|error| format!("invalid native JSON input: {error}"))
}

fn object(value: &Value) -> Result<&Map<String, Value>, String> {
    value
        .as_object()
        .ok_or_else(|| "coordination-v2 payload must be a JSON object".to_owned())
}

fn sha256(value: &[u8]) -> String {
    format!("{:x}", Sha256::digest(value))
}

fn canonical_bytes(value: &Value) -> Result<Vec<u8>, String> {
    serde_json::to_vec(value).map_err(|error| error.to_string())
}

fn canonical_sha256(value: &Value) -> Result<String, String> {
    Ok(sha256(&canonical_bytes(value)?))
}

fn fit_utf8(value: &str, max_bytes: usize) -> Result<String, String> {
    const MARKER: &str = "…";
    let marker_bytes = MARKER.len();
    if max_bytes < marker_bytes {
        return Err("max_bytes is too small for the truncation marker".to_owned());
    }
    if value.len() <= max_bytes {
        return Ok(value.to_owned());
    }

    let characters = value.chars().collect::<Vec<_>>();
    let mut low = 0usize;
    let mut high = characters.len();
    let mut best = String::new();
    while low <= high {
        let midpoint = (low + high) / 2;
        let candidate = characters[..midpoint]
            .iter()
            .collect::<String>()
            .trim_end_matches(python_whitespace)
            .to_owned();
        if candidate.len() + marker_bytes <= max_bytes {
            best = candidate;
            low = midpoint + 1;
        } else if midpoint == 0 {
            break;
        } else {
            high = midpoint - 1;
        }
    }
    best.push_str(MARKER);
    Ok(best)
}

fn valid_identifier(value: &str) -> bool {
    let mut characters = value.chars();
    let Some(first) = characters.next() else {
        return false;
    };
    if !first.is_ascii_alphanumeric() {
        return false;
    }
    let mut count = 1usize;
    for character in characters {
        count += 1;
        if count > MAX_IDENTIFIER_CHARS
            || !(character.is_ascii_alphanumeric() || "._:/-".contains(character))
        {
            return false;
        }
    }
    true
}

fn validated_identifier(value: Option<&Value>, field: &str) -> Result<String, String> {
    let Some(value) = value.and_then(Value::as_str) else {
        return Err(format!(
            "{field} must be a non-empty ASCII identifier of at most {MAX_IDENTIFIER_CHARS} characters"
        ));
    };
    if !valid_identifier(value) {
        return Err(format!(
            "{field} must be a non-empty ASCII identifier of at most {MAX_IDENTIFIER_CHARS} characters"
        ));
    }
    Ok(value.to_owned())
}

fn required_string(
    row: &Map<String, Value>,
    field: &str,
    max_bytes: usize,
) -> Result<String, String> {
    let Some(value) = row.get(field).and_then(Value::as_str) else {
        return Err(format!(
            "A2A row field '{field}' must be a non-empty string"
        ));
    };
    if value.is_empty() {
        return Err(format!(
            "A2A row field '{field}' must be a non-empty string"
        ));
    }
    if value.len() > max_bytes {
        return Err(format!("A2A row field '{field}' exceeds {max_bytes} bytes"));
    }
    Ok(value.to_owned())
}

fn optional_string(
    row: &Map<String, Value>,
    field: &str,
    max_bytes: usize,
) -> Result<Option<String>, String> {
    let Some(value) = row.get(field) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let Some(value) = value.as_str() else {
        return Err(format!("A2A row field '{field}' must be a string or null"));
    };
    if value.len() > max_bytes {
        return Err(format!("A2A row field '{field}' exceeds {max_bytes} bytes"));
    }
    Ok(Some(value.to_owned()))
}

fn validated_row(value: &Value) -> Result<Row, String> {
    let raw = value
        .as_object()
        .ok_or_else(|| "A2A row must be a mapping or sqlite3.Row-like object".to_owned())?;
    let message_id = validated_identifier(raw.get("message_id"), "message_id")?;
    let direction = raw.get("direction").and_then(Value::as_str);
    if !matches!(direction, Some("inbound" | "outbound")) {
        return Err("A2A row direction must be 'inbound' or 'outbound'".to_owned());
    }
    let Some(body) = raw.get("body").and_then(Value::as_str) else {
        return Err("A2A row body must be a string".to_owned());
    };
    if body.len() > MAX_RAW_FIELD_BYTES {
        return Err(format!("A2A row body exceeds {MAX_RAW_FIELD_BYTES} bytes"));
    }
    Ok(Row {
        message_id,
        direction: direction.expect("direction matched above").to_owned(),
        sender: required_string(raw, "sender", MAX_METADATA_BYTES)?,
        recipient: required_string(raw, "recipient", MAX_METADATA_BYTES)?,
        body: body.to_owned(),
        data_json: optional_string(raw, "data_json", MAX_RAW_FIELD_BYTES)?,
        created_at: required_string(raw, "created_at", MAX_METADATA_BYTES)?,
        received_at: optional_string(raw, "received_at", MAX_METADATA_BYTES)?,
        delivery_status: required_string(raw, "delivery_status", MAX_METADATA_BYTES)?,
        status_reason: optional_string(raw, "status_reason", MAX_METADATA_BYTES)?,
        read_at: optional_string(raw, "read_at", MAX_METADATA_BYTES)?,
    })
}

fn validate_coordination(value: &Value) -> Result<Coordination, String> {
    let payload = object(value)?;
    let unexpected = payload
        .keys()
        .filter(|key| !COORDINATION_V2_FIELDS.contains(&key.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    if !unexpected.is_empty() {
        let rendered = unexpected
            .iter()
            .map(|key| format!("'{key}'"))
            .collect::<Vec<_>>()
            .join(", ");
        return Err(format!(
            "coordination-v2 payload has unexpected fields: [{rendered}]"
        ));
    }
    if payload.get("kind").and_then(Value::as_str) != Some("coordination-v2") {
        return Err("coordination-v2 payload requires kind='coordination-v2'".to_owned());
    }

    let thread_id = payload
        .get("thread_id")
        .map(|value| validated_identifier(Some(value), "thread_id"))
        .transpose()?;
    let summary = payload
        .get("summary")
        .map(|value| {
            let Some(value) = value.as_str() else {
                return Err("coordination-v2 summary must be a string".to_owned());
            };
            let normalized = normalized_text(value);
            if normalized.is_empty() {
                return Err("coordination-v2 summary must not be empty".to_owned());
            }
            if normalized.len() > MAX_PROTOCOL_SUMMARY_BYTES {
                return Err(format!(
                    "coordination-v2 summary exceeds {MAX_PROTOCOL_SUMMARY_BYTES} UTF-8 bytes"
                ));
            }
            Ok(normalized)
        })
        .transpose()?;
    let status = payload
        .get("status")
        .map(|value| {
            let valid = value
                .as_str()
                .filter(|status| COORDINATION_STATUSES.contains(status));
            valid.map(str::to_owned).ok_or_else(|| {
                "coordination-v2 status must be one of ['blocked', 'in_progress', 'informational', 'open', 'resolved', 'superseded']".to_owned()
            })
        })
        .transpose()?;
    let requires_response = payload
        .get("requires_response")
        .map(|value| {
            value
                .as_bool()
                .ok_or_else(|| "coordination-v2 requires_response must be a boolean".to_owned())
        })
        .transpose()?;
    let supersedes = if let Some(value) = payload.get("supersedes") {
        let Some(values) = value.as_array() else {
            return Err("coordination-v2 supersedes must be a list".to_owned());
        };
        if values.len() > MAX_SUPERSEDES {
            return Err(format!(
                "coordination-v2 supersedes accepts at most {MAX_SUPERSEDES} IDs"
            ));
        }
        let mut result = Vec::with_capacity(values.len());
        let mut unique = BTreeSet::new();
        for (index, message_id) in values.iter().enumerate() {
            let identifier =
                validated_identifier(Some(message_id), &format!("supersedes[{index}]"))?;
            if !unique.insert(identifier.clone()) {
                return Err("coordination-v2 supersedes contains duplicate IDs".to_owned());
            }
            result.push(identifier);
        }
        result
    } else {
        Vec::new()
    };
    Ok(Coordination {
        thread_id,
        summary,
        status,
        requires_response,
        supersedes,
    })
}

fn coordination_value(coordination: &Coordination) -> Value {
    json!({
        "kind": "coordination-v2",
        "thread_id": coordination.thread_id,
        "summary": coordination.summary,
        "status": coordination.status,
        "requires_response": coordination.requires_response,
        "supersedes": coordination.supersedes,
    })
}

/// Validate the transport's coordination-v2 shape through the same pure core
/// used by the Python extension. Native senders call this before any writes.
pub fn validate_coordination_v2_value(value: &Value) -> Result<Value, String> {
    validate_coordination(value).map(|coordination| coordination_value(&coordination))
}

fn data_kind(data: Option<&Value>) -> Result<Option<String>, String> {
    let Some(kind) = data
        .and_then(Value::as_object)
        .and_then(|value| value.get("kind"))
        .and_then(Value::as_str)
    else {
        return Ok(None);
    };
    let normalized = normalized_text(kind);
    if normalized.is_empty() {
        Ok(None)
    } else {
        fit_utf8(&normalized, MAX_DATA_KIND_BYTES).map(Some)
    }
}

fn actionable(status: Option<&str>, requires_response: Option<bool>) -> bool {
    requires_response == Some(true)
        || status.is_none()
        || matches!(status, Some("open" | "in_progress" | "blocked"))
}

fn legacy_thread_id(direction: &str, message_id: &str) -> String {
    let identity = format!("{direction}\0{message_id}");
    format!("legacy-{}", &sha256(identity.as_bytes())[..24])
}

pub fn compact_message_value(value: &Value) -> Result<Value, String> {
    let row = validated_row(value)?;
    let (data, data_json_valid) = match row.data_json.as_deref() {
        None => (None, true),
        Some(source) => match serde_json::from_str::<Value>(source) {
            Ok(value) => (Some(value), true),
            Err(_) => (None, false),
        },
    };

    let mut protocol = "legacy";
    let coordination = if data
        .as_ref()
        .and_then(Value::as_object)
        .and_then(|payload| payload.get("kind"))
        .and_then(Value::as_str)
        == Some("coordination-v2")
    {
        protocol = "coordination-v2";
        Some(validate_coordination(
            data.as_ref().expect("coordination data exists"),
        )?)
    } else if data
        .as_ref()
        .and_then(Value::as_object)
        .is_some_and(|payload| {
            COORDINATION_V2_OPTIONAL_FIELDS
                .iter()
                .any(|field| payload.contains_key(*field))
        })
    {
        return Err("coordination-v2 fields require an explicit kind='coordination-v2'".to_owned());
    } else {
        None
    };

    if coordination
        .as_ref()
        .is_some_and(|payload| payload.supersedes.contains(&row.message_id))
    {
        return Err("a coordination-v2 message cannot supersede itself".to_owned());
    }

    let body_summary = normalized_text(&row.body);
    let (summary, summary_source) = if let Some(summary) = coordination
        .as_ref()
        .and_then(|payload| payload.summary.as_ref())
    {
        (summary.clone(), "coordination-v2.summary")
    } else if !body_summary.is_empty() {
        (body_summary, "body-fallback")
    } else {
        let label = data_kind(data.as_ref())?.unwrap_or_else(|| "legacy".to_owned());
        (
            format!("{label} message {}", row.message_id),
            "deterministic-label",
        )
    };
    let summary = fit_utf8(&summary, MAX_COMPACT_SUMMARY_BYTES)?;
    let body_bytes = row.body.as_bytes();
    let data_bytes = row.data_json.as_deref().unwrap_or_default().as_bytes();
    let source_payload = json!({
        "message_id": row.message_id,
        "direction": row.direction,
        "sender": row.sender,
        "recipient": row.recipient,
        "body": row.body,
        "data_json": row.data_json,
        "created_at": row.created_at,
        "received_at": row.received_at,
        "delivery_status": row.delivery_status,
        "status_reason": row.status_reason,
        "read_at": row.read_at,
    });
    let source_sha256 = canonical_sha256(&source_payload)?;
    let mut content = Vec::with_capacity(body_bytes.len() + data_bytes.len() + 16);
    content.extend_from_slice(b"body\0");
    content.extend_from_slice(body_bytes);
    if row.data_json.is_some() {
        content.extend_from_slice(b"\0data:");
        content.extend_from_slice(data_bytes);
    } else {
        content.extend_from_slice(b"\0data:null");
    }
    let status = coordination
        .as_ref()
        .and_then(|payload| payload.status.clone());
    let requires_response = coordination
        .as_ref()
        .and_then(|payload| payload.requires_response);
    let thread_id = coordination
        .as_ref()
        .and_then(|payload| payload.thread_id.clone())
        .unwrap_or_else(|| legacy_thread_id(&row.direction, &row.message_id));
    let supersedes = coordination
        .as_ref()
        .map(|payload| payload.supersedes.clone())
        .unwrap_or_default();
    let mut receipt = json!({
        "schema_version": SCHEMA_VERSION,
        "authority": AUTHORITY,
        "message_id": row.message_id,
        "direction": row.direction,
        "sender": row.sender,
        "recipient": row.recipient,
        "created_at": row.created_at,
        "received_at": row.received_at,
        "read_at": row.read_at,
        "delivery_status": row.delivery_status,
        "protocol": protocol,
        "data_kind": data_kind(data.as_ref())?,
        "data_json_valid": data_json_valid,
        "thread_id": thread_id,
        "summary": summary,
        "summary_source": summary_source,
        "status": status,
        "requires_response": requires_response,
        "actionable": actionable(status.as_deref(), requires_response),
        "supersedes": supersedes,
        "raw_body_bytes": body_bytes.len(),
        "raw_data_bytes": data_bytes.len(),
        "omitted_raw_bytes": body_bytes.len() + data_bytes.len(),
        "body_sha256": sha256(body_bytes),
        "data_sha256": row.data_json.as_ref().map(|_| sha256(data_bytes)),
        "content_sha256": sha256(&content),
        "source_sha256": source_sha256,
    });
    let receipt_sha256 = canonical_sha256(&receipt)?;
    receipt
        .as_object_mut()
        .expect("receipt is an object")
        .insert("receipt_sha256".to_owned(), Value::String(receipt_sha256));
    Ok(receipt)
}

fn string_at<'a>(value: &'a Value, field: &str) -> &'a str {
    value[field].as_str().expect("validated receipt string")
}

fn bool_at(value: &Value, field: &str) -> bool {
    value[field].as_bool().expect("validated receipt boolean")
}

fn u64_at(value: &Value, field: &str) -> u64 {
    value[field].as_u64().expect("validated receipt integer")
}

fn positive_bound(value: Option<&Value>, name: &str, maximum: u64) -> Result<u64, String> {
    let Some(value) = value.and_then(Value::as_u64) else {
        return Err(format!("{name} must be an integer"));
    };
    if value < 1 || value > maximum {
        return Err(format!("{name} must be between 1 and {maximum}"));
    }
    Ok(value)
}

fn unique_receipts(rows: &[Value]) -> Result<Vec<Value>, String> {
    let mut receipts = Vec::with_capacity(rows.len().min(MAX_INPUT_MESSAGES));
    let mut identities = HashMap::<(String, String), String>::new();
    for row in rows {
        if receipts.len() >= MAX_INPUT_MESSAGES {
            return Err(format!(
                "compaction accepts at most {MAX_INPUT_MESSAGES} distinct messages"
            ));
        }
        let receipt = compact_message_value(row)?;
        let identity = (
            string_at(&receipt, "direction").to_owned(),
            string_at(&receipt, "message_id").to_owned(),
        );
        let source_sha256 = string_at(&receipt, "source_sha256");
        if let Some(previous) = identities.get(&identity) {
            if previous != source_sha256 {
                return Err(format!(
                    "conflicting duplicate A2A row for {}:{}",
                    identity.0, identity.1
                ));
            }
            continue;
        }
        identities.insert(identity, source_sha256.to_owned());
        receipts.push(receipt);
    }
    Ok(receipts)
}

fn thread_digest(
    thread_id: &str,
    mut receipts: Vec<Value>,
    max_messages_per_thread: usize,
) -> Result<Value, String> {
    receipts.sort_by(|left, right| {
        (
            string_at(left, "created_at"),
            string_at(left, "message_id"),
            string_at(left, "direction"),
            string_at(left, "source_sha256"),
        )
            .cmp(&(
                string_at(right, "created_at"),
                string_at(right, "message_id"),
                string_at(right, "direction"),
                string_at(right, "source_sha256"),
            ))
    });
    let actionable_receipts = receipts
        .iter()
        .filter(|receipt| bool_at(receipt, "actionable"))
        .collect::<Vec<_>>();
    if actionable_receipts.len() > max_messages_per_thread {
        return Err(format!(
            "thread '{thread_id}' has {} actionable messages; detail cap is {max_messages_per_thread}",
            actionable_receipts.len()
        ));
    }
    let actionable_hashes = actionable_receipts
        .iter()
        .map(|receipt| string_at(receipt, "receipt_sha256"))
        .collect::<BTreeSet<_>>();
    let remaining_slots = max_messages_per_thread - actionable_receipts.len();
    let non_actionable = receipts
        .iter()
        .filter(|receipt| !actionable_hashes.contains(string_at(receipt, "receipt_sha256")))
        .collect::<Vec<_>>();
    let selected_non_actionable = non_actionable
        .iter()
        .skip(non_actionable.len().saturating_sub(remaining_slots));
    let mut selected_hashes = actionable_hashes;
    selected_hashes
        .extend(selected_non_actionable.map(|receipt| string_at(receipt, "receipt_sha256")));
    let selected = receipts
        .iter()
        .filter(|receipt| selected_hashes.contains(string_at(receipt, "receipt_sha256")))
        .cloned()
        .collect::<Vec<_>>();
    let supersession_edges = receipts
        .iter()
        .flat_map(|receipt| {
            receipt["supersedes"]
                .as_array()
                .expect("validated supersedes array")
                .iter()
                .map(|target| {
                    json!({
                        "message_id": string_at(receipt, "message_id"),
                        "supersedes": target,
                    })
                })
        })
        .collect::<Vec<_>>();
    let provenance = receipts
        .iter()
        .map(|receipt| {
            json!({
                "message_id": string_at(receipt, "message_id"),
                "direction": string_at(receipt, "direction"),
                "source_sha256": string_at(receipt, "source_sha256"),
                "receipt_sha256": string_at(receipt, "receipt_sha256"),
            })
        })
        .collect::<Vec<_>>();
    let digest_payload = json!({
        "thread_id": thread_id,
        "provenance": provenance,
        "supersession_edges": supersession_edges,
    });
    Ok(json!({
        "thread_id": thread_id,
        "message_count": receipts.len(),
        "message_ids": receipts.iter().map(|receipt| string_at(receipt, "message_id")).collect::<Vec<_>>(),
        "actionable_count": actionable_receipts.len(),
        "actionable_message_ids": actionable_receipts.iter().map(|receipt| string_at(receipt, "message_id")).collect::<Vec<_>>(),
        "omitted_raw_bytes": receipts.iter().map(|receipt| u64_at(receipt, "omitted_raw_bytes")).sum::<u64>(),
        "provenance": provenance,
        "supersession_edges": supersession_edges,
        "messages": selected,
        "omitted_message_details": receipts.len() - selected_hashes.len(),
        "thread_sha256": canonical_sha256(&digest_payload)?,
    }))
}

/// Compact bounded A2A thread receipts with the same pure algorithm exported
/// through the Python extension when that feature is enabled.
pub fn compact_threads_value(value: &Value) -> Result<Value, String> {
    let request = value
        .as_object()
        .ok_or_else(|| "native compaction request must be a JSON object".to_owned())?;
    let max_threads = positive_bound(request.get("max_threads"), "max_threads", MAX_THREADS)?;
    let max_messages_per_thread = positive_bound(
        request.get("max_messages_per_thread"),
        "max_messages_per_thread",
        MAX_DETAILED_MESSAGES,
    )?;
    let rows = request
        .get("rows")
        .and_then(Value::as_array)
        .ok_or_else(|| "native compaction rows must be a JSON list".to_owned())?;
    let receipts = unique_receipts(rows)?;
    let mut groups = BTreeMap::<String, Vec<Value>>::new();
    for receipt in receipts.iter().cloned() {
        groups
            .entry(string_at(&receipt, "thread_id").to_owned())
            .or_default()
            .push(receipt);
    }
    if groups.len() > max_threads as usize {
        return Err(format!(
            "compaction found {} threads, exceeding max_threads={max_threads}",
            groups.len()
        ));
    }
    let threads = groups
        .into_iter()
        .map(|(thread_id, receipts)| {
            thread_digest(&thread_id, receipts, max_messages_per_thread as usize)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut result = json!({
        "schema_version": SCHEMA_VERSION,
        "authority": AUTHORITY,
        "thread_count": threads.len(),
        "message_count": receipts.len(),
        "actionable_count": threads.iter().map(|thread| u64_at(thread, "actionable_count")).sum::<u64>(),
        "omitted_raw_bytes": threads.iter().map(|thread| u64_at(thread, "omitted_raw_bytes")).sum::<u64>(),
        "threads": threads,
    });
    let compaction_sha256 = canonical_sha256(&result)?;
    result
        .as_object_mut()
        .expect("compaction result is an object")
        .insert(
            "compaction_sha256".to_owned(),
            Value::String(compaction_sha256),
        );
    Ok(result)
}

#[cfg(feature = "python")]
fn run_native(
    py: Python<'_>,
    source: &str,
    operation: fn(&Value) -> Result<Value, String>,
) -> PyResult<String> {
    let source = source.to_owned();
    py.detach(move || {
        let value = parse_json(&source).and_then(|value| operation(&value));
        value
            .and_then(|result| serde_json::to_string(&result).map_err(|error| error.to_string()))
            .map_err(PyValueError::new_err)
    })
}

#[cfg(feature = "python")]
#[pyfunction]
fn a2a_validate_coordination_v2_native(py: Python<'_>, source: &str) -> PyResult<String> {
    run_native(py, source, validate_coordination_v2_value)
}

#[cfg(feature = "python")]
#[pyfunction]
fn a2a_compact_message_native(py: Python<'_>, source: &str) -> PyResult<String> {
    run_native(py, source, compact_message_value)
}

#[cfg(feature = "python")]
#[pyfunction]
fn a2a_compact_threads_native(py: Python<'_>, source: &str) -> PyResult<String> {
    run_native(py, source, compact_threads_value)
}

#[cfg(feature = "python")]
pub(crate) fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(
        a2a_validate_coordination_v2_native,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(a2a_compact_message_native, module)?)?;
    module.add_function(wrap_pyfunction!(a2a_compact_threads_native, module)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn coordination() -> Value {
        json!({
            "kind": "coordination-v2",
            "thread_id": "thread-1",
            "summary": "Compact status",
            "status": "open",
            "requires_response": false,
            "supersedes": [],
        })
    }

    fn row(body: &str, data: Option<Value>) -> Value {
        json!({
            "message_id": "m1",
            "direction": "inbound",
            "sender": "alice",
            "recipient": "bob",
            "body": body,
            "data_json": data.map(|value| serde_json::to_string(&value).unwrap()),
            "created_at": "2026-08-30T12:00:00+00:00",
            "received_at": "2026-08-30T12:00:01+00:00",
            "delivery_status": "delivered",
            "status_reason": null,
            "read_at": null,
        })
    }

    #[test]
    fn coordination_rejects_invalid_protocol_shapes() {
        let invalid = [
            Value::Null,
            json!([]),
            json!({}),
            json!({"kind": "coordination"}),
            json!({"kind": "coordination-v2", "unknown": 1}),
            json!({"kind": "coordination-v2", "thread_id": "bad id"}),
            json!({"kind": "coordination-v2", "summary": "   "}),
            json!({"kind": "coordination-v2", "status": "done"}),
            json!({"kind": "coordination-v2", "requires_response": 1}),
            json!({"kind": "coordination-v2", "supersedes": "m1"}),
            json!({"kind": "coordination-v2", "supersedes": ["bad id"]}),
            json!({"kind": "coordination-v2", "supersedes": ["m1", "m1"]}),
        ];
        for payload in invalid {
            assert!(
                validate_coordination_v2_value(&payload).is_err(),
                "accepted {payload}"
            );
        }
    }

    #[test]
    fn coordination_normalizes_summary_and_preserves_supersedes_order() {
        let mut payload = coordination();
        payload["summary"] = json!("  one\n  two  ");
        payload["supersedes"] = json!(["m-old", "m-older"]);
        let result = validate_coordination_v2_value(&payload).unwrap();
        assert_eq!(result["summary"], "one two");
        assert_eq!(result["supersedes"], json!(["m-old", "m-older"]));
        assert_eq!(result["thread_id"], "thread-1");
        assert_eq!(result["status"], "open");
        assert_eq!(result["requires_response"], false);
    }

    #[test]
    fn coordination_bounds_use_utf8_bytes_and_supersedes_count() {
        let mut payload = coordination();
        payload["summary"] = json!("é".repeat(MAX_PROTOCOL_SUMMARY_BYTES));
        assert!(validate_coordination_v2_value(&payload)
            .unwrap_err()
            .contains("UTF-8 bytes"));
        payload["summary"] = json!("short");
        payload["supersedes"] = json!((0..=MAX_SUPERSEDES)
            .map(|index| format!("m-{index}"))
            .collect::<Vec<_>>());
        assert!(validate_coordination_v2_value(&payload)
            .unwrap_err()
            .contains("at most"));
    }

    #[test]
    fn compact_message_is_deterministic_and_omits_raw_payload() {
        let mut data = coordination();
        data["summary"] = json!("Review the bounded evidence");
        let source = row("Please inspect the attached result.", Some(data));
        let receipt = compact_message_value(&source).unwrap();
        assert_eq!(receipt, compact_message_value(&source).unwrap());
        assert_eq!(receipt["authority"], AUTHORITY);
        assert_eq!(receipt["summary"], "Review the bounded evidence");
        let rendered = receipt.to_string();
        assert!(!rendered.contains("Please inspect the attached result."));
        assert!(!rendered.contains("Compact status"));
        assert_eq!(receipt["source_sha256"].as_str().unwrap().len(), 64);
        assert_eq!(receipt["receipt_sha256"].as_str().unwrap().len(), 64);
    }

    #[test]
    fn legacy_fallback_is_bounded_and_empty_body_has_a_label() {
        let long = row(
            &"  legacy\nmessage  ".repeat(100),
            Some(json!({"kind": "coordination"})),
        );
        let receipt = compact_message_value(&long).unwrap();
        assert_eq!(receipt["protocol"], "legacy");
        assert_eq!(receipt["summary_source"], "body-fallback");
        assert!(receipt["summary"].as_str().unwrap().len() <= MAX_COMPACT_SUMMARY_BYTES);
        assert!(receipt["summary"].as_str().unwrap().ends_with('…'));
        assert_eq!(receipt["status"], Value::Null);
        assert_eq!(receipt["requires_response"], Value::Null);
        assert_eq!(receipt["actionable"], true);

        let empty = row("", Some(json!({"kind": "gate-review-request"})));
        let labelled = compact_message_value(&empty).unwrap();
        assert_eq!(labelled["summary"], "gate-review-request message m1");
        assert_eq!(labelled["summary_source"], "deterministic-label");
    }

    #[test]
    fn actionability_preserves_status_and_response_independently() {
        for status in COORDINATION_STATUSES {
            for requires_response in [false, true] {
                let mut data = coordination();
                data["status"] = json!(status);
                data["requires_response"] = json!(requires_response);
                let receipt = compact_message_value(&row("body", Some(data))).unwrap();
                assert_eq!(receipt["status"], status);
                assert_eq!(receipt["requires_response"], requires_response);
                assert_eq!(
                    receipt["actionable"],
                    requires_response || matches!(status, "open" | "in_progress" | "blocked"),
                );
            }
        }
        let receipt = compact_message_value(&row(
            "body",
            Some(json!({"kind": "coordination-v2", "requires_response": false})),
        ))
        .unwrap();
        assert_eq!(receipt["status"], Value::Null);
        assert_eq!(receipt["actionable"], true);
    }

    #[test]
    fn malformed_v2_and_self_supersession_fail_closed() {
        let implicit = row("body", Some(json!({"summary": "looks like v2"})));
        assert!(compact_message_value(&implicit)
            .unwrap_err()
            .contains("explicit kind"));
        let mut data = coordination();
        data["supersedes"] = json!(["m1"]);
        assert!(compact_message_value(&row("body", Some(data)))
            .unwrap_err()
            .contains("supersede itself"));
    }

    #[test]
    fn malformed_json_and_unicode_have_exact_byte_accounting() {
        let mut invalid = row("body", None);
        invalid["data_json"] = json!("{not-json");
        let receipt = compact_message_value(&invalid).unwrap();
        assert_eq!(receipt["protocol"], "legacy");
        assert_eq!(receipt["data_json_valid"], false);
        assert_eq!(receipt["raw_data_bytes"], 9);
        assert_eq!(receipt["data_sha256"].as_str().unwrap().len(), 64);

        let unicode = row("é🙂", Some(json!({"kind": "coordination", "note": "雪"})));
        let receipt = compact_message_value(&unicode).unwrap();
        let data_len = unicode["data_json"].as_str().unwrap().len();
        assert_eq!(receipt["raw_body_bytes"], "é🙂".len());
        assert_eq!(receipt["raw_data_bytes"], data_len);
        assert_eq!(receipt["omitted_raw_bytes"], "é🙂".len() + data_len);
    }

    #[test]
    fn prose_fields_truncate_on_utf8_boundaries_and_metadata_fails_loud() {
        let receipt = compact_message_value(&row(
            &"雪".repeat(1_000),
            Some(json!({"kind": "分類".repeat(1_000)})),
        ))
        .unwrap();
        for (field, max) in [
            ("summary", MAX_COMPACT_SUMMARY_BYTES),
            ("data_kind", MAX_DATA_KIND_BYTES),
        ] {
            let value = receipt[field].as_str().unwrap();
            assert!(value.len() <= max);
            assert!(value.ends_with('…'));
        }
        let mut too_large = row("body", None);
        too_large["sender"] = json!("é".repeat(MAX_METADATA_BYTES));
        assert!(compact_message_value(&too_large)
            .unwrap_err()
            .contains("sender"));
    }

    #[test]
    fn threads_deduplicate_identical_rows_and_sort_receipts() {
        let mut earlier = row("first", Some(coordination()));
        earlier["message_id"] = json!("m0");
        earlier["created_at"] = json!("2026-08-30T11:00:00+00:00");
        let later = row("second", Some(coordination()));
        let input = json!({
            "rows": [later, earlier, earlier],
            "max_threads": 4,
            "max_messages_per_thread": 4,
        });
        let result = compact_threads_value(&input).unwrap();
        assert_eq!(result["thread_count"], 1);
        assert_eq!(result["message_count"], 2);
        assert_eq!(result["threads"][0]["message_ids"], json!(["m0", "m1"]));
        assert_eq!(
            result["threads"][0]["actionable_message_ids"],
            json!(["m0", "m1"])
        );
        assert_eq!(
            result["threads"][0]["messages"].as_array().unwrap().len(),
            2
        );
        assert_eq!(result["compaction_sha256"].as_str().unwrap().len(), 64);
    }

    #[test]
    fn conflicting_duplicates_fail_before_thread_output() {
        let first = row("first", Some(coordination()));
        let second = row("changed", Some(coordination()));
        let input = json!({
            "rows": [first, second],
            "max_threads": 4,
            "max_messages_per_thread": 4,
        });
        assert!(compact_threads_value(&input)
            .unwrap_err()
            .contains("conflicting duplicate"));
    }

    #[test]
    fn detail_cap_preserves_actionable_messages_and_newest_resolved_message() {
        let mut rows = Vec::new();
        for (id, status) in [
            ("m0", "resolved"),
            ("m1", "open"),
            ("m2", "informational"),
            ("m3", "resolved"),
        ] {
            let mut data = coordination();
            data["status"] = json!(status);
            let mut message = row(id, Some(data));
            message["message_id"] = json!(id);
            message["created_at"] = json!(format!("2026-08-30T12:00:0{}+00:00", &id[1..]));
            rows.push(message);
        }
        let result = compact_threads_value(&json!({
            "rows": rows,
            "max_threads": 4,
            "max_messages_per_thread": 2,
        }))
        .unwrap();
        let thread = &result["threads"][0];
        assert_eq!(thread["message_count"], 4);
        assert_eq!(thread["actionable_count"], 1);
        assert_eq!(thread["actionable_message_ids"], json!(["m1"]));
        assert_eq!(thread["omitted_message_details"], 2);
        let ids = thread["messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|message| message["message_id"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(ids, ["m1", "m3"]);
        assert_eq!(thread["provenance"].as_array().unwrap().len(), 4);
    }

    #[test]
    fn detail_cap_refuses_to_drop_actionable_messages() {
        let first = row("first", Some(coordination()));
        let mut second = row("second", Some(coordination()));
        second["message_id"] = json!("m2");
        let request = json!({
            "rows": [first, second],
            "max_threads": 4,
            "max_messages_per_thread": 1,
        });
        assert!(compact_threads_value(&request)
            .unwrap_err()
            .contains("2 actionable messages"));
    }

    #[test]
    fn threads_reject_invalid_bounds_and_distinct_message_limit() {
        for request in [
            json!({"rows": [], "max_threads": 0, "max_messages_per_thread": 1}),
            json!({"rows": [], "max_threads": 1, "max_messages_per_thread": 0}),
            json!({"rows": [], "max_threads": MAX_THREADS + 1, "max_messages_per_thread": 1}),
            json!({"rows": [], "max_threads": 1, "max_messages_per_thread": MAX_DETAILED_MESSAGES + 1}),
        ] {
            assert!(compact_threads_value(&request).is_err());
        }
        let rows = (0..=MAX_INPUT_MESSAGES)
            .map(|index| {
                let mut message = row("body", None);
                message["message_id"] = json!(format!("m{index}"));
                message
            })
            .collect::<Vec<_>>();
        let request = json!({
            "rows": rows,
            "max_threads": MAX_THREADS,
            "max_messages_per_thread": MAX_DETAILED_MESSAGES,
        });
        assert!(compact_threads_value(&request)
            .unwrap_err()
            .contains("at most 256 distinct messages"));
    }
}
