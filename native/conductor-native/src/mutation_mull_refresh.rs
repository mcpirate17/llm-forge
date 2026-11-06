//! Pure validation and pin refresh for an existing generated standalone Mull campaign.
//! The caller captures input bytes and owns publication; this module performs no I/O.
use serde_json::Value;

fn valid_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 200
        && id.starts_with(char::is_alphanumeric)
        && id
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.'))
}
fn check_ratchet(value: &Value) -> Result<(), String> {
    let object = value.as_object().ok_or("campaign must be an object")?;
    let baseline = object
        .get("survivor_baseline")
        .and_then(Value::as_array)
        .ok_or("survivor_baseline must be a list")?;
    if let Some(recorded) = object.get("survivor_baseline_recorded") {
        let recorded = recorded
            .as_bool()
            .ok_or("survivor_baseline_recorded must be boolean")?;
        if !recorded && !baseline.is_empty() {
            return Err("unrecorded survivor baseline must be empty".into());
        }
    }
    for key in ["survivor_baseline_note", "survivor_baseline_recorded_at"] {
        if object.get(key).is_some_and(|field| !field.is_string()) {
            return Err(format!("{key} must be a string when present"));
        }
    }
    Ok(())
}

/// Validate the independently supplied manifest and return its exact source and driver.
pub fn inputs(existing: &Value) -> Result<(String, String), String> {
    crate::candidate_cargo_audit::serialized_size(existing, 2 * 1024 * 1024)
        .map_err(|error| format!("campaign exceeds refresh bound: {error}"))?;
    let payload = existing.as_object().ok_or("campaign must be an object")?;
    if !payload
        .get("generator")
        .and_then(|v| v.get("run_timeout_seconds"))
        .and_then(Value::as_i64)
        .is_some_and(|timeout| timeout > 0)
    {
        return Err("Mull run timeout must be a positive signed integer".into());
    }
    if payload.get("schema_version").and_then(Value::as_u64) != Some(1) {
        return Err("unsupported generated campaign schema".into());
    }
    let id = payload
        .get("campaign_id")
        .and_then(Value::as_str)
        .filter(|id| valid_id(id))
        .ok_or("invalid campaign ID")?;
    check_ratchet(existing)?;
    let contract = crate::mutation_manifest::generated_campaign_contract_with(
        payload,
        Vec::new(),
        "refresh.json",
        "mull",
        |_| None,
    )?;
    if contract.execution_profile.as_deref() != Some("mull-confined-v1")
        || contract.campaign_id != id
    {
        return Err("refresh requires a verified generated confined Mull profile".into());
    }
    let source = contract
        .source_sha256
        .as_object()
        .and_then(|m| m.keys().next())
        .ok_or("missing Mull source")?;
    let driver = contract
        .test_sha256
        .as_object()
        .and_then(|m| m.keys().next())
        .ok_or("missing Mull driver")?;
    Ok((source.clone(), driver.clone()))
}

/// Replace only the two independently captured content hashes; all other fields are preserved.
pub fn refresh(existing: &Value, source_sha256: &str, test_sha256: &str) -> Result<Value, String> {
    let (source, driver) = inputs(existing)?;
    if !valid_hash(source_sha256) || !valid_hash(test_sha256) {
        return Err("refreshed pins must be lowercase SHA-256 digests".into());
    }
    let mut refreshed = existing.clone();
    refreshed["source_sha256"][&source] = Value::String(source_sha256.to_owned());
    refreshed["test_sha256"][&driver] = Value::String(test_sha256.to_owned());
    Ok(refreshed)
}

#[cfg(test)]
#[path = "mutation_mull_refresh_tests.rs"]
mod tests;
