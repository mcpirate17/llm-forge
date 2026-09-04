//! Target-tree authentication for mutation campaign receipts.
//!
//! Python keeps bounded Git process execution and CLI presentation. This module owns
//! receipt/manifest validation, byte hashing, pin comparison, inventory reproduction,
//! and the deterministic four-part verdict.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use pyo3::prelude::*;
use pyo3::types::PyBytes;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::mutation_manifest::safe_relative;

const PART1: &str = "manifest_blob_in_tree";
const PART2: &str = "manifest_sha256";
const PART3: &str = "source_pins";
const PART4: &str = "inventory_digest";

#[derive(Debug, Deserialize, Serialize)]
struct TargetTreeReceipt {
    path: String,
    schema_version: Option<String>,
    manifest: String,
    manifest_sha256: String,
    source_sha256: BTreeMap<String, String>,
}

#[derive(Debug, Serialize)]
struct PinDivergence {
    only_in_receipt: Vec<String>,
    only_in_tree_inventory: Vec<String>,
    hash_mismatch: Vec<String>,
}

struct SourceRead {
    sha256: Option<String>,
    reason: String,
}

fn value_error(message: impl Into<String>) -> PyErr {
    pyo3::exceptions::PyValueError::new_err(message.into())
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn required_object<'a>(value: &'a Value, label: &str) -> Result<&'a Map<String, Value>, String> {
    value
        .as_object()
        .ok_or_else(|| format!("{label} must be a JSON object"))
}

fn required_string(value: Option<&Value>, label: &str) -> Result<String, String> {
    value
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
        .map(str::to_owned)
        .ok_or_else(|| format!("{label} must be a non-empty string"))
}

fn required_sha256(value: Option<&Value>, label: &str) -> Result<String, String> {
    let digest = required_string(value, label)?;
    let valid = digest.len() == 64
        && digest
            .as_bytes()
            .iter()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte));
    if !valid {
        return Err(format!("{label} must be a lowercase SHA-256 digest"));
    }
    Ok(digest)
}

fn load_pin_map(value: Option<&Value>, label: &str) -> Result<BTreeMap<String, String>, String> {
    let mapping = value
        .and_then(Value::as_object)
        .ok_or_else(|| format!("{label} must be a JSON object"))?;
    if mapping.is_empty() {
        return Err(format!("{label} must not be empty"));
    }
    let mut pins = BTreeMap::new();
    for (raw_path, raw_digest) in mapping {
        let path = safe_relative(raw_path, &format!("{label} key"))?;
        if pins.contains_key(&path) {
            return Err(format!(
                "{label} contains duplicate normalized path: {path}"
            ));
        }
        let digest = required_sha256(Some(raw_digest), &format!("{label}[{path}]"))?;
        pins.insert(path, digest);
    }
    Ok(pins)
}

fn load_receipt(path: &Path) -> Result<TargetTreeReceipt, String> {
    if !path.is_file() {
        return Err(format!("receipt file is missing: {}", path.display()));
    }
    let raw = fs::read(path)
        .map_err(|error| format!("cannot read receipt file {}: {error}", path.display()))?;
    if raw.is_empty() {
        return Err(format!("receipt file is empty: {}", path.display()));
    }
    let payload: Value = serde_json::from_slice(&raw)
        .map_err(|error| format!("receipt is not valid JSON: {}: {error}", path.display()))?;
    let receipt = required_object(&payload, &format!("receipt {}", path.display()))?;
    let manifest_raw = required_string(receipt.get("manifest"), "receipt.manifest")?;
    let manifest = safe_relative(&manifest_raw, "receipt.manifest")?;
    Ok(TargetTreeReceipt {
        path: path.display().to_string(),
        schema_version: receipt
            .get("schema_version")
            .and_then(Value::as_str)
            .map(str::to_owned),
        manifest,
        manifest_sha256: required_sha256(
            receipt.get("manifest_sha256"),
            "receipt.manifest_sha256",
        )?,
        source_sha256: load_pin_map(receipt.get("source_sha256"), "receipt.source_sha256")?,
    })
}

fn manifest_pins(blob: &[u8], manifest: &str) -> Result<BTreeMap<String, String>, String> {
    let payload: Value = serde_json::from_slice(blob)
        .map_err(|error| format!("manifest blob is not valid JSON: {manifest}: {error}"))?;
    let mapping = required_object(&payload, &format!("manifest {manifest}"))?;
    load_pin_map(
        mapping.get("source_sha256"),
        &format!("manifest {manifest} source_sha256"),
    )
}

fn inventory_digest<'a>(pins: impl IntoIterator<Item = (&'a str, &'a str)>) -> String {
    let mut lines: Vec<String> = pins
        .into_iter()
        .map(|(path, digest)| format!("{digest}  {path}\n"))
        .collect();
    lines.sort_by(|left, right| {
        let left_digest = left.split_once("  ").map_or("", |row| row.0);
        let right_digest = right.split_once("  ").map_or("", |row| row.0);
        left_digest.cmp(right_digest).then_with(|| left.cmp(right))
    });
    sha256_hex(lines.concat().as_bytes())
}

fn pin_divergence(
    recorded: &BTreeMap<String, String>,
    reproduced: &BTreeMap<String, String>,
) -> PinDivergence {
    let recorded_paths: BTreeSet<&String> = recorded.keys().collect();
    let reproduced_paths: BTreeSet<&String> = reproduced.keys().collect();
    PinDivergence {
        only_in_receipt: recorded_paths
            .difference(&reproduced_paths)
            .map(|path| (*path).clone())
            .collect(),
        only_in_tree_inventory: reproduced_paths
            .difference(&recorded_paths)
            .map(|path| (*path).clone())
            .collect(),
        hash_mismatch: recorded_paths
            .intersection(&reproduced_paths)
            .filter(|path| recorded.get(**path) != reproduced.get(**path))
            .map(|path| (*path).clone())
            .collect(),
    }
}

fn verdict(
    receipt: &TargetTreeReceipt,
    repo_root: &str,
    tree_oid: &str,
    checks: BTreeMap<String, Value>,
    failures: Vec<String>,
) -> Value {
    let passed = failures.is_empty()
        && checks
            .values()
            .all(|check| check.get("status").and_then(Value::as_str) == Some("PASS"));
    json!({
        "status": if passed { "PASS" } else { "FAIL" },
        "repo_root": repo_root,
        "tree_oid": tree_oid,
        "receipt": receipt.path,
        "receipt_schema_version": receipt.schema_version,
        "manifest": receipt.manifest,
        "checks": checks,
        "failures": failures,
    })
}

fn verify(
    receipt: TargetTreeReceipt,
    repo_root: &str,
    tree_oid: &str,
    manifest_blob: Option<&[u8]>,
    manifest_reason: &str,
    source_reads: &BTreeMap<String, SourceRead>,
) -> Result<Value, String> {
    let mut checks = BTreeMap::new();
    let mut failures = Vec::new();
    let Some(blob) = manifest_blob else {
        checks.insert(
            PART1.to_owned(),
            json!({"status": "FAIL", "detail": manifest_reason}),
        );
        failures.push(format!(
            "part 1 ({PART1}): {} is not a blob in tree {tree_oid}: {manifest_reason}",
            receipt.manifest
        ));
        for name in [PART2, PART3, PART4] {
            checks.insert(
                name.to_owned(),
                json!({"status": "BLOCKED", "detail": "manifest blob unavailable"}),
            );
        }
        return Ok(verdict(&receipt, repo_root, tree_oid, checks, failures));
    };

    checks.insert(
        PART1.to_owned(),
        json!({"status": "PASS", "size_bytes": blob.len()}),
    );
    let actual_manifest_sha = sha256_hex(blob);
    if actual_manifest_sha == receipt.manifest_sha256 {
        checks.insert(
            PART2.to_owned(),
            json!({"status": "PASS", "sha256": actual_manifest_sha}),
        );
    } else {
        checks.insert(
            PART2.to_owned(),
            json!({
                "status": "FAIL",
                "recorded": receipt.manifest_sha256,
                "actual": actual_manifest_sha,
            }),
        );
        failures.push(format!(
            "part 2 ({PART2}): recorded {} != actual {actual_manifest_sha}",
            receipt.manifest_sha256
        ));
    }

    let pins = manifest_pins(blob, &receipt.manifest)?;
    let mut pin_failures = Vec::new();
    let mut actual_hashes = BTreeMap::new();
    let mut unreadable = 0usize;
    for (path, pinned) in &pins {
        match source_reads.get(path).and_then(|row| row.sha256.as_deref()) {
            Some(actual) => {
                actual_hashes.insert(path.clone(), actual.to_owned());
                if actual != pinned {
                    pin_failures.push(format!("{path}: pinned {pinned} != actual {actual}"));
                }
            }
            None => {
                unreadable += 1;
                let reason = source_reads
                    .get(path)
                    .map(|row| row.reason.as_str())
                    .filter(|reason| !reason.is_empty())
                    .unwrap_or("source blob result unavailable");
                pin_failures.push(format!(
                    "{path}: pinned {pinned}, but the path is not a blob in the target tree ({reason})"
                ));
            }
        }
    }
    if pin_failures.is_empty() {
        checks.insert(
            PART3.to_owned(),
            json!({"status": "PASS", "checked": pins.len()}),
        );
    } else {
        checks.insert(
            PART3.to_owned(),
            json!({"status": "FAIL", "checked": pins.len(), "failures": pin_failures}),
        );
        failures.extend(
            pin_failures
                .iter()
                .map(|line| format!("part 3 ({PART3}): {line}")),
        );
    }

    let recorded_digest = inventory_digest(
        receipt
            .source_sha256
            .iter()
            .map(|(path, digest)| (path.as_str(), digest.as_str())),
    );
    if unreadable > 0 {
        checks.insert(
            PART4.to_owned(),
            json!({
                "status": "BLOCKED",
                "recorded_digest": recorded_digest,
                "detail": format!("{unreadable} pinned path(s) unreadable in the target tree"),
            }),
        );
        failures.push(format!(
            "part 4 ({PART4}): blocked -- {unreadable} pinned path(s) unreadable in the target tree; reproduction impossible"
        ));
    } else {
        let reproduced_digest = inventory_digest(
            actual_hashes
                .iter()
                .map(|(path, digest)| (path.as_str(), digest.as_str())),
        );
        if recorded_digest == reproduced_digest {
            checks.insert(
                PART4.to_owned(),
                json!({"status": "PASS", "digest": recorded_digest, "pins": actual_hashes.len()}),
            );
        } else {
            let divergence = pin_divergence(&receipt.source_sha256, &actual_hashes);
            let divergence_json = serde_json::to_string(&divergence)
                .map_err(|error| format!("cannot serialize pin divergence: {error}"))?;
            checks.insert(
                PART4.to_owned(),
                json!({
                    "status": "FAIL",
                    "recorded_digest": recorded_digest,
                    "reproduced_digest": reproduced_digest,
                    "divergence": divergence,
                }),
            );
            failures.push(format!(
                "part 4 ({PART4}): recorded {recorded_digest} != reproduced {reproduced_digest} (divergence: {divergence_json})"
            ));
        }
    }
    Ok(verdict(&receipt, repo_root, tree_oid, checks, failures))
}

#[pyfunction]
pub fn load_tree_receipt_native(path: &str) -> PyResult<String> {
    let receipt = load_receipt(&PathBuf::from(path)).map_err(value_error)?;
    serde_json::to_string(&receipt).map_err(|error| value_error(error.to_string()))
}

#[pyfunction]
pub fn receipt_manifest_pins_native(blob: &[u8], manifest: &str) -> PyResult<String> {
    let pins = manifest_pins(blob, manifest).map_err(value_error)?;
    serde_json::to_string(&pins).map_err(|error| value_error(error.to_string()))
}

#[pyfunction]
pub fn receipt_inventory_digest_native(pins: Vec<(String, String)>) -> String {
    inventory_digest(
        pins.iter()
            .map(|(path, digest)| (path.as_str(), digest.as_str())),
    )
}

#[pyfunction]
pub fn receipt_sha256_native(blob: &[u8]) -> String {
    sha256_hex(blob)
}

#[pyfunction]
#[pyo3(signature = (
    receipt_json,
    repo_root,
    tree_oid,
    manifest_blob,
    manifest_reason,
    source_rows
))]
pub fn verify_tree_receipt_native(
    py: Python<'_>,
    receipt_json: &str,
    repo_root: &str,
    tree_oid: &str,
    manifest_blob: Option<Py<PyBytes>>,
    manifest_reason: &str,
    source_rows: Vec<(String, Option<String>, String)>,
) -> PyResult<String> {
    let receipt: TargetTreeReceipt = serde_json::from_str(receipt_json)
        .map_err(|error| value_error(format!("invalid loaded receipt: {error}")))?;
    let mut reads = BTreeMap::new();
    for (path, blob, reason) in source_rows {
        if reads.contains_key(&path) {
            return Err(value_error(format!("duplicate source blob result: {path}")));
        }
        reads.insert(
            path,
            SourceRead {
                sha256: blob,
                reason,
            },
        );
    }
    let manifest_bytes = manifest_blob
        .as_ref()
        .map(|value| value.bind(py).as_bytes());
    let result = verify(
        receipt,
        repo_root,
        tree_oid,
        manifest_bytes,
        manifest_reason,
        &reads,
    )
    .map_err(value_error)?;
    serde_json::to_string(&result).map_err(|error| value_error(error.to_string()))
}

pub(crate) fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(load_tree_receipt_native, module)?)?;
    module.add_function(wrap_pyfunction!(receipt_manifest_pins_native, module)?)?;
    module.add_function(wrap_pyfunction!(receipt_inventory_digest_native, module)?)?;
    module.add_function(wrap_pyfunction!(receipt_sha256_native, module)?)?;
    module.add_function(wrap_pyfunction!(verify_tree_receipt_native, module)?)?;
    Ok(())
}
