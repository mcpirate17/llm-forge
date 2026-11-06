//! Replay cooperative single-executable Mull observations against verified campaign pins.
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::path::{Component, Path};

fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn sha(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
/// Hash a recursively key-sorted report after enforcing the serialization bound.
pub fn report_sha256(report: &Value) -> Result<String, String> {
    crate::candidate_cargo_audit::serialized_size(report, 4 * 1024 * 1024)
        .map_err(|error| format!("Mull report exceeds bound: {error}"))?;
    let mut canonical = report.clone();
    canonical.sort_all_objects();
    let bytes = serde_json::to_vec(&canonical).map_err(|error| error.to_string())?;
    Ok(sha(&bytes))
}

fn absolute(value: &str) -> bool {
    let path = Path::new(value);
    value.len() <= 4096
        && !value.bytes().any(|b| b.is_ascii_control())
        && path.is_absolute()
        && path
            .components()
            .all(|part| matches!(part, Component::RootDir | Component::Normal(_)))
}

/// Returns the first refusal; no recorded path is opened or treated as authority.
pub fn evidence_error(
    payload: &Map<String, Value>,
    campaign_id: &str,
    source_pins: &Value,
    test_pins: &Value,
) -> Option<String> {
    validate(payload, campaign_id, source_pins, test_pins).err()
}

fn validate_definition(payload: &Map<String, Value>) -> Result<(), String> {
    let definition = payload
        .get("mull_definition")
        .and_then(Value::as_object)
        .ok_or("Mull definition evidence missing")?;
    if definition.get("schema_version").and_then(Value::as_u64) != Some(1)
        || definition.get("read_only") != Some(&Value::Bool(true))
        || definition.get("removed") != Some(&Value::Bool(true))
        || !definition
            .get("path")
            .and_then(Value::as_str)
            .is_some_and(absolute)
    {
        return Err("Mull definition isolation or cleanup incomplete".into());
    }
    Ok(())
}

fn observation<'a>(
    payload: &'a Map<String, Value>,
    campaign_id: &str,
    source_pins: &Value,
    test_pins: &Value,
) -> Result<&'a Map<String, Value>, String> {
    let obs = payload
        .get("mull_observation")
        .and_then(Value::as_object)
        .ok_or("Mull observation missing")?;
    if obs.get("schema_version").and_then(Value::as_u64) != Some(1)
        || obs.get("campaign_id").and_then(Value::as_str) != Some(campaign_id)
        || obs.get("scope").and_then(Value::as_str) != Some("single-standalone-executable")
        || obs.get("source_sha256") != Some(source_pins)
        || obs.get("test_sha256") != Some(test_pins)
        || payload.get("source_sha256") != Some(source_pins)
        || payload.get("test_sha256") != Some(test_pins)
    {
        return Err("Mull observation differs from verified campaign".into());
    }
    for key in ["report_sha256", "raw_report_sha256", "executable_sha256"] {
        if !obs.get(key).and_then(Value::as_str).is_some_and(digest) {
            return Err(format!("invalid Mull {key}"));
        }
    }
    let baselines = obs
        .get("baselines")
        .and_then(Value::as_array)
        .ok_or("Mull baselines missing")?;
    if baselines.len() != 2
        || baselines.iter().any(|row| {
            row.get("returncode").and_then(Value::as_i64) != Some(0)
                || row.get("timed_out") != Some(&Value::Bool(false))
        })
    {
        return Err("Mull baselines are not both successful".into());
    }
    Ok(obs)
}

fn validate(
    payload: &Map<String, Value>,
    campaign_id: &str,
    source_pins: &Value,
    test_pins: &Value,
) -> Result<(), String> {
    validate_definition(payload)?;
    let obs = observation(payload, campaign_id, source_pins, test_pins)?;
    let sources = source_pins.as_object().ok_or("Mull source pins missing")?;
    let tests = test_pins.as_object().ok_or("Mull test pins missing")?;
    if sources.len() != 1 || tests.len() != 1 {
        return Err("Mull requires one source and one driver".into());
    }
    let relative = obs
        .get("source_path")
        .and_then(Value::as_str)
        .ok_or("Mull source path missing")?;
    crate::mutation_snapshot_inputs::validate_path(relative)?;
    let expected = sources
        .get(relative)
        .and_then(Value::as_str)
        .filter(|v| digest(v))
        .ok_or("Mull source is not pinned")?;
    let worktree = obs
        .get("worktree")
        .and_then(Value::as_str)
        .filter(|v| absolute(v))
        .ok_or("invalid Mull worktree diagnostic")?;
    let source_path = Path::new(worktree).join(relative);
    let report = obs.get("report").ok_or("Mull report missing")?;
    let expected_report_hash = report_sha256(report)?;
    if obs.get("report_sha256").and_then(Value::as_str) != Some(expected_report_hash.as_str()) {
        return Err("Mull report canonical hash or size mismatch".into());
    }
    let source = report
        .get("files")
        .and_then(Value::as_object)
        .and_then(|files| files.get(source_path.to_str()?))
        .and_then(|file| file.get("source"))
        .and_then(Value::as_str)
        .ok_or("Mull report source missing")?;
    if sha(source.as_bytes()) != expected {
        return Err("Mull source content differs from campaign pin".into());
    }
    let rows =
        crate::mutation_mull_report::rows(report, Path::new(worktree), &source_path, source)?;
    if payload.get("mutants") != Some(&Value::Array(rows)) {
        return Err("Mull mutant rows differ from retained report".into());
    }
    Ok(())
}

#[cfg(test)]
#[path = "mutation_mull_receipt_tests.rs"]
pub(crate) mod tests;
