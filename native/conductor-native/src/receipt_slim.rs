//! Slim mutation receipts: the summary block stays plain JSON (every existing
//! reader keys off it unchanged) and the two bulky detail lists -- `mutants`
//! and `test_value`, ~24 of the ~26 MiB the tracked receipts held -- move
//! under one `detail` key whose value is either inline JSON (small campaigns)
//! or a zstd-compressed base64 blob inside the same file, so the tree stops
//! growing ~350 KB per ratchet iteration without splitting files.
//!
//! One decoder owns the format: [`expand_detail`], re-exported to Python as
//! `receipt_expand_detail_native` (the readers that need detail rows call it
//! lazily -- a summary-only reader never decompresses anything) and to the
//! `forge` binary as `forge receipt show <path>`. The engine writes slim
//! receipts through [`slim_receipt`], and [`compact_directory`] is the one
//! pass that slims every kept receipt and replaces the detail of superseded
//! ones (an older receipt of a campaign that has a newer one) with a
//! `{"encoding": "superseded", "superseded_by": <file>}` pointer, keeping the
//! summary block byte-identical.
//!
//! Canonical file shape matches `mutation_testing_support.atomic_json` exactly
//! (`json.dumps(..., indent=2, sort_keys=True) + "\n"`): serde_json's
//! `arbitrary_precision` (this crate's serde_json feature) keeps number tokens
//! verbatim, and both serializers emit two-space indent with sorted keys, so a
//! compacted-and-back receipt is byte-identical to its author's format.

use std::collections::BTreeMap;
use std::path::Path;

use pyo3::prelude::*;
use serde_json::{json, Map, Value};

/// The one key under which the detail lists live in a slim receipt.
pub const DETAIL_KEY: &str = "detail";
/// The detail lists that move under [`DETAIL_KEY`].
pub const DETAIL_KEYS: &[&str] = &["mutants", "test_value"];
/// A campaign with fewer mutants than this stays inline (the brief's small
/// campaign cut).
pub const INLINE_MUTANT_LIMIT: usize = 50;
/// ...and only when the inline payload is also under this many bytes, so a
/// small-mutant receipt with a huge `test_value` still compresses.
pub const INLINE_BYTE_LIMIT: usize = 32 * 1024;

fn canonical_json(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| String::new()) + "\n"
}

/// The detail object of a full (unslimmed) receipt: only the keys that exist.
fn detail_payload(receipt: &Value) -> Map<String, Value> {
    let mut payload = Map::new();
    if let Some(object) = receipt.as_object() {
        for key in DETAIL_KEYS {
            if let Some(value) = object.get(*key) {
                payload.insert((*key).to_string(), value.clone());
            }
        }
    }
    payload
}

/// [`slim_detail`]: inline when the payload is small, else one zstd+base64
/// blob of the payload's canonical JSON.
fn encode_blob(payload: &Map<String, Value>) -> Value {
    let text = serde_json::to_string(payload).unwrap_or_default();
    let compressed =
        zstd::stream::encode_all(text.as_bytes(), 3).unwrap_or_else(|_| text.as_bytes().to_vec());
    let blob = base64_encode(&compressed);
    json!({"encoding": "zstd+base64", "blob": blob})
}

/// [`slim_receipt`]'s one rule, on the detail object alone.
pub fn slim_detail(payload: &Map<String, Value>) -> Value {
    let mutants = payload.get("mutants").and_then(Value::as_array);
    let inline = mutants.is_some_and(|rows| rows.len() < INLINE_MUTANT_LIMIT)
        && serde_json::to_string(payload).is_ok_and(|text| text.len() < INLINE_BYTE_LIMIT);
    if inline {
        let mut detail = payload.clone();
        detail.insert("encoding".to_string(), Value::String("json".to_string()));
        Value::Object(detail)
    } else {
        encode_blob(payload)
    }
}

/// The full receipt with its detail lists replaced by one slim `detail` value.
/// Every other key is carried over untouched (the byte-identical summary).
pub fn slim_receipt(receipt: &Value) -> Value {
    let payload = detail_payload(receipt);
    if payload.is_empty() {
        return receipt.clone();
    }
    let mut out = receipt.clone();
    let object = out.as_object_mut().expect("receipts are JSON objects");
    for key in DETAIL_KEYS {
        object.remove(*key);
    }
    object.insert(DETAIL_KEY.to_string(), slim_detail(&payload));
    out
}

/// The inverse of [`slim_detail`]: the detail lists back as one object. A
/// `superseded` pointer is an error naming the receipt that replaced it; an
/// unknown encoding fails loud rather than guessing.
pub fn expand_detail(detail: &Value) -> Result<Map<String, Value>, String> {
    let object = detail
        .as_object()
        .ok_or_else(|| "detail block is not a JSON object".to_string())?;
    match object.get("encoding").and_then(Value::as_str) {
        Some("json") => Ok(object
            .iter()
            .filter(|(key, _)| key.as_str() != "encoding")
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect()),
        Some("zstd+base64") => {
            let blob = object
                .get("blob")
                .and_then(Value::as_str)
                .ok_or_else(|| "zstd+base64 detail lacks its blob".to_string())?;
            let compressed = base64_decode(blob)
                .map_err(|error| format!("detail blob is not valid base64: {error}"))?;
            let raw = zstd::stream::decode_all(compressed.as_slice())
                .map_err(|error| format!("detail blob does not decompress: {error}"))?;
            let text =
                String::from_utf8(raw).map_err(|_| "detail blob is not UTF-8 JSON".to_string())?;
            let value: Value = serde_json::from_str(&text)
                .map_err(|error| format!("detail blob is not JSON: {error}"))?;
            value
                .as_object()
                .cloned()
                .ok_or_else(|| "detail blob is not a JSON object".to_string())
        }
        Some("superseded") => {
            let newer = object
                .get("superseded_by")
                .and_then(Value::as_str)
                .unwrap_or("?");
            Err(format!("receipt superseded by {newer}"))
        }
        other => Err(format!(
            "unknown detail encoding {:?} (expected json, zstd+base64 or superseded)",
            other
        )),
    }
}

/// A receipt with its detail lists restored. A slim receipt without a
/// `detail` key (a legacy full receipt) is returned unchanged, so callers can
/// feed every receipt through this one seam.
pub fn expand_receipt(receipt: &Value) -> Result<Value, String> {
    let Some(detail) = receipt.get(DETAIL_KEY) else {
        return Ok(receipt.clone());
    };
    let payload = expand_detail(detail)?;
    let mut out = receipt.clone();
    let object = out.as_object_mut().expect("receipts are JSON objects");
    object.remove(DETAIL_KEY);
    for (key, value) in payload {
        object.insert(key, value);
    }
    Ok(out)
}

/// The superseded pointer: the summary block stays, the detail goes.
pub fn supersede_detail(newer_file: &str) -> Value {
    json!({"encoding": "superseded", "superseded_by": newer_file})
}

fn generated_at(payload: &Value) -> &str {
    payload
        .get("generated_at")
        .and_then(Value::as_str)
        .unwrap_or("")
}

/// `mutation_patch_audit`'s status gate: only these statuses can be the
/// receipt the audit reads, so only they may be supersede targets -- a crashed
/// run leaves an ERROR receipt with the newest stamp, and pointing an older
/// PASS receipt at it would destroy the only detail the audit expands.
fn status_passes(payload: &Value) -> bool {
    matches!(
        payload.get("status").and_then(Value::as_str),
        Some("PASS") | Some("RATCHET_HELD")
    )
}

/// One pass over a receipt directory (non-recursive; `.iterations` and any
/// subdirectory are not this pass's to touch): every receipt is slimmed, and
/// every receipt superseded by a kept one of the same `campaign_id` has its
/// detail replaced by a pointer to it. Which receipt is kept is the audit's
/// decision, not the clock's: `mutation_patch_audit._acceptable_receipt`
/// rejects a newer receipt whose runner components match neither this runner
/// nor any lineage entry (a parallel slice's clone, say) while accepting an
/// older sibling, so the caller passes the per-campaign keep-set it computed
/// with the audit's own predicate; `None` falls back to the (passing-status,
/// then greatest `generated_at`, first sorted filename on ties) heuristic,
/// which is right whenever no receipt was rejected for lineage.
/// Returns the before/after byte totals and per-kind counts.
pub fn compact_directory(
    root: &Path,
    keep: Option<&BTreeMap<String, String>>,
) -> Result<Value, String> {
    let mut entries: Vec<(std::path::PathBuf, Value)> = Vec::new();
    let read = std::fs::read_dir(root).map_err(|error| format!("{root:?}: {error}"))?;
    for entry in read.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let text = std::fs::read_to_string(&path)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        let payload: Value =
            serde_json::from_str(&text).map_err(|error| format!("{}: {error}", path.display()))?;
        if payload.get("campaign_id").and_then(Value::as_str).is_none() {
            return Err(format!("{}: receipt lacks campaign_id", path.display()));
        }
        entries.push((path, payload));
    }
    if let Some(map) = keep {
        let names: std::collections::BTreeSet<String> = entries
            .iter()
            .map(|(path, _)| {
                path.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        for (campaign, target) in map {
            if !names.contains(target) {
                return Err(format!(
                    "keep-set names {target:?} for {campaign:?}, which is not a receipt in {root:?}"
                ));
            }
        }
    }
    entries.sort_by(|a, b| a.0.file_name().cmp(&b.0.file_name()));
    // Heuristic keep-set: (passes-status, stamp, path, name) -- a
    // passing-status receipt always outranks a failing one however new (a
    // crashed run leaves an ERROR receipt with the newest stamp), then the
    // greater stamp wins, then the first file in sorted-filename order
    // (strictly-greater comparison keeps it).
    let mut newest: BTreeMap<String, (bool, String, std::path::PathBuf, String)> = BTreeMap::new();
    for (path, payload) in &entries {
        let campaign = payload["campaign_id"].as_str().unwrap_or("?").to_string();
        let rank = (status_passes(payload), generated_at(payload).to_string());
        let incumbent = newest.get(&campaign);
        if incumbent.is_none_or(|(passes, stamp, _, _)| rank > (*passes, stamp.clone())) {
            let name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            newest.insert(campaign, (rank.0, rank.1, path.clone(), name));
        }
    }
    let keep_name = |campaign: &str, path: &Path| -> Option<String> {
        if let Some(target) = keep.and_then(|map| map.get(campaign)) {
            let name = path.file_name()?.to_string_lossy().into_owned();
            return (name == *target).then_some(name);
        }
        newest
            .get(campaign)
            .filter(|(_, _, newest_path, _)| newest_path == path)
            .map(|(_, _, _, name)| name.clone())
    };
    let mut bytes_before = 0usize;
    let mut bytes_after = 0usize;
    let mut slimmed = 0usize;
    let mut superseded = 0usize;
    let mut unchanged = 0usize;
    for (path, payload) in &entries {
        let campaign = payload["campaign_id"].as_str().unwrap_or("?").to_string();
        let original = std::fs::read(path).unwrap_or_default();
        bytes_before += original.len();
        let rewritten = match keep_name(&campaign, path) {
            Some(_) => {
                let slimmed_payload = slim_receipt(payload);
                if slimmed_payload.get(DETAIL_KEY) == payload.get(DETAIL_KEY) {
                    unchanged += 1; // already slim (or detail-free)
                } else {
                    slimmed += 1;
                }
                slimmed_payload
            }
            None => {
                let target = keep
                    .and_then(|map| map.get(&campaign))
                    .cloned()
                    .or_else(|| newest.get(&campaign).map(|(_, _, _, name)| name.clone()));
                superseded += 1;
                let mut out = payload.clone();
                let object = out.as_object_mut().expect("receipts are JSON objects");
                for key in DETAIL_KEYS {
                    object.remove(*key);
                }
                object.insert(
                    DETAIL_KEY.to_string(),
                    supersede_detail(&target.unwrap_or_default()),
                );
                out
            }
        };
        let text = canonical_json(&rewritten);
        bytes_after += text.len();
        if text.as_bytes() != original.as_slice() {
            std::fs::write(path, text.as_bytes())
                .map_err(|error| format!("{}: {error}", path.display()))?;
        }
    }
    Ok(json!({
        "files": entries.len(),
        "slimmed": slimmed,
        "superseded": superseded,
        "unchanged": unchanged,
        "bytes_before": bytes_before,
        "bytes_after": bytes_after,
    }))
}

/// Read one receipt file and expand it (the `forge receipt show` body).
pub fn expand_file(path: &Path) -> Result<Value, String> {
    let text =
        std::fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let receipt: Value =
        serde_json::from_str(&text).map_err(|error| format!("{}: {error}", path.display()))?;
    expand_receipt(&receipt)
}

fn base64_encode(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn base64_decode(text: &str) -> Result<Vec<u8>, String> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(text)
        .map_err(|error| error.to_string())
}

fn json_round_trip(
    py: Python<'_>,
    payload_json: &str,
    native: fn(&Value) -> Result<Value, String>,
) -> PyResult<String> {
    let receipt: Value = serde_json::from_str(payload_json)
        .map_err(|error| pyo3::exceptions::PyValueError::new_err(error.to_string()))?;
    let out = py
        .detach(|| native(&receipt))
        .map_err(|error| pyo3::exceptions::PyValueError::new_err(error.to_string()))?;
    serde_json::to_string(&out)
        .map_err(|error| pyo3::exceptions::PyRuntimeError::new_err(error.to_string()))
}

#[pyfunction]
fn receipt_slim_detail_native(py: Python<'_>, receipt_json: &str) -> PyResult<String> {
    json_round_trip(py, receipt_json, |receipt| Ok(slim_receipt(receipt)))
}

#[pyfunction]
fn receipt_expand_detail_native(py: Python<'_>, receipt_json: &str) -> PyResult<String> {
    json_round_trip(py, receipt_json, expand_receipt)
}

#[pyfunction]
fn receipt_compact_directory_native(
    py: Python<'_>,
    root: &str,
    keep_json: Option<String>,
) -> PyResult<String> {
    let keep: Option<BTreeMap<String, String>> = keep_json
        .map(|text| {
            serde_json::from_str(&text)
                .map_err(|error| pyo3::exceptions::PyValueError::new_err(error.to_string()))
        })
        .transpose()?;
    let out = py.detach(|| compact_directory(Path::new(root), keep.as_ref()));
    match out {
        Ok(value) => serde_json::to_string(&value)
            .map_err(|error| pyo3::exceptions::PyRuntimeError::new_err(error.to_string())),
        Err(error) => Err(pyo3::exceptions::PyValueError::new_err(error)),
    }
}

pub(crate) fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(receipt_slim_detail_native, module)?)?;
    module.add_function(wrap_pyfunction!(receipt_expand_detail_native, module)?)?;
    module.add_function(wrap_pyfunction!(receipt_compact_directory_native, module)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn full_receipt(mutants: usize) -> Value {
        let rows: Vec<Value> = (0..mutants)
            .map(|n| json!({"id": format!("m{n}"), "outcome": "KILLED"}))
            .collect();
        json!({
            "campaign_id": "c1", "status": "RATCHET_HELD", "generated_at": "t",
            "mutation_score": 0.9459459459459459, "mutants": rows,
        })
    }

    #[test]
    fn small_campaigns_stay_inline_and_round_trip() {
        let receipt = full_receipt(5);
        let slim = slim_receipt(&receipt);
        let detail = slim[DETAIL_KEY].as_object().unwrap();
        assert_eq!(detail["encoding"], "json");
        assert_eq!(detail["mutants"].as_array().unwrap().len(), 5);
        // The summary keys are carried untouched.
        assert_eq!(slim["campaign_id"], "c1");
        assert_eq!(slim["mutation_score"], receipt["mutation_score"]);
        assert!(slim.get("mutants").is_none());
        let expanded = expand_receipt(&slim).unwrap();
        assert_eq!(expanded, receipt);
    }

    #[test]
    fn big_campaigns_become_one_zstd_blob() {
        let receipt = full_receipt(200);
        let slim = slim_receipt(&receipt);
        assert_eq!(slim[DETAIL_KEY]["encoding"], "zstd+base64");
        assert!(slim[DETAIL_KEY].get("blob").is_some());
        let expanded = expand_receipt(&slim).unwrap();
        assert_eq!(expanded, receipt);
        // A float token parsed from file text survives compress-and-back
        // verbatim (arbitrary_precision keeps the original digits).
        let fussy: Value = serde_json::from_str(
            r#"{"campaign_id": "c", "mutants": [{"timing_ms": 0.9459459459459459}]}"#,
        )
        .unwrap();
        let round = expand_receipt(&slim_receipt(&fussy)).unwrap();
        assert_eq!(
            serde_json::to_string(&round["mutants"]).unwrap(),
            r#"[{"timing_ms":0.9459459459459459}]"#
        );
    }

    #[test]
    fn superseded_pointers_fail_loud_naming_the_replacement() {
        let detail = supersede_detail("newer.json");
        assert_eq!(
            expand_detail(&detail).unwrap_err(),
            "receipt superseded by newer.json"
        );
    }

    #[test]
    fn legacy_receipts_pass_through_expansion_unchanged() {
        let receipt = full_receipt(3);
        assert_eq!(expand_receipt(&receipt).unwrap(), receipt);
    }

    #[test]
    fn a_newest_error_receipt_never_becomes_the_supersede_target() {
        let dir = std::env::temp_dir().join(format!("receipt-slim-err-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut pass = full_receipt(80);
        pass["generated_at"] = Value::String("2026-01-01T00:00:00Z".to_string());
        let mut error = full_receipt(80);
        error["status"] = Value::String("ERROR".to_string());
        error["generated_at"] = Value::String("2027-01-01T00:00:00Z".to_string());
        std::fs::write(dir.join("a_pass.json"), canonical_json(&pass)).unwrap();
        std::fs::write(dir.join("z_error.json"), canonical_json(&error)).unwrap();
        let stats = compact_directory(&dir, None).unwrap();
        // The ERROR receipt is superseded by the older PASS one, not the other
        // way round: the audit reads the PASS receipt, so that one keeps detail.
        assert_eq!(stats["superseded"], 1);
        let kept: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("a_pass.json")).unwrap())
                .unwrap();
        assert_eq!(kept[DETAIL_KEY]["encoding"], "zstd+base64");
        let dead: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("z_error.json")).unwrap())
                .unwrap();
        assert_eq!(dead[DETAIL_KEY]["superseded_by"], "a_pass.json");
        assert_eq!(dead["status"], "ERROR"); // summary kept
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_callers_keep_set_overrides_the_clock() {
        let dir = std::env::temp_dir().join(format!("receipt-slim-keep-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut older = full_receipt(80);
        older["generated_at"] = Value::String("2026-01-01T00:00:00Z".to_string());
        let mut newer = full_receipt(80);
        newer["generated_at"] = Value::String("2027-01-01T00:00:00Z".to_string());
        std::fs::write(dir.join("a_old.json"), canonical_json(&older)).unwrap();
        std::fs::write(dir.join("z_new.json"), canonical_json(&newer)).unwrap();
        // The audit's own predicate accepted the OLDER receipt (the newer one
        // pinned a foreign runner) and said so through the keep-set.
        let mut keep = BTreeMap::new();
        keep.insert("c1".to_string(), "a_old.json".to_string());
        let stats = compact_directory(&dir, Some(&keep)).unwrap();
        assert_eq!(stats["superseded"], 1);
        let kept: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("a_old.json")).unwrap())
                .unwrap();
        assert_eq!(kept[DETAIL_KEY]["encoding"], "zstd+base64");
        let dead: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("z_new.json")).unwrap())
                .unwrap();
        assert_eq!(dead[DETAIL_KEY]["superseded_by"], "a_old.json");
        // A keep-set naming a file that is not there refuses rather than
        // writing a dangling pointer.
        let mut bogus = BTreeMap::new();
        bogus.insert("c1".to_string(), "nope.json".to_string());
        assert!(compact_directory(&dir, Some(&bogus)).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn compaction_slims_kept_and_points_superseded_at_the_newest() {
        let dir = std::env::temp_dir().join(format!("receipt-slim-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("c1_old.json"), canonical_json(&full_receipt(80))).unwrap();
        let mut newer = full_receipt(80);
        newer["generated_at"] = Value::String("t2".to_string());
        std::fs::write(dir.join("c1_new.json"), canonical_json(&newer)).unwrap();
        let stats = compact_directory(&dir, None).unwrap();
        assert_eq!(stats["superseded"], 1);
        assert_eq!(stats["slimmed"], 1);
        let old: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("c1_old.json")).unwrap())
                .unwrap();
        assert_eq!(old[DETAIL_KEY]["superseded_by"], "c1_new.json");
        assert_eq!(old["status"], "RATCHET_HELD"); // summary kept
        let new: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("c1_new.json")).unwrap())
                .unwrap();
        assert_eq!(new[DETAIL_KEY]["encoding"], "zstd+base64");
        assert!(expand_receipt(&old).is_err());
        assert_eq!(expand_receipt(&new).unwrap(), newer);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
