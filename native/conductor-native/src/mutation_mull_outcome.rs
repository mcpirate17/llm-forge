//! Final corpus verdict checks for the confined Mull profile.
use serde_json::{Map, Value};
use std::collections::BTreeSet;

/// Call only after the retained Mull observation has been bound to its campaign.
pub fn corpus_error(payload: &Map<String, Value>) -> Option<String> {
    validate(payload).err()
}

fn validate(payload: &Map<String, Value>) -> Result<(), String> {
    let status = payload
        .get("status")
        .and_then(Value::as_str)
        .ok_or("Mull status missing")?;
    if !matches!(status, "PASS" | "RATCHET_HELD") {
        return Err("Mull final status invalid".into());
    }
    if let Some(error) = crate::mutation_native_runner::partial_progress_error(payload) {
        return Err(error.into());
    }
    crate::mutation_source_coverage::validate(payload)?;
    let rows = payload
        .get("mutants")
        .and_then(Value::as_array)
        .filter(|v| !v.is_empty())
        .ok_or("Mull mutant rows missing")?;
    let mut ids = BTreeSet::new();
    let mut survivors = BTreeSet::new();
    for row in rows {
        let id = row
            .get("id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .ok_or("Mull mutant id missing")?;
        if !ids.insert(id) {
            return Err("duplicate Mull mutant id".into());
        }
        match row.get("outcome").and_then(Value::as_str) {
            Some("KILLED") => (),
            Some("SURVIVED") => {
                survivors.insert(id);
            }
            _ => return Err("Mull has an incomplete mutant outcome".into()),
        }
    }
    let total = rows.len() as u64;
    let survived = survivors.len() as u64;
    let killed = total - survived;
    validate_counts(payload, total, killed, survived)?;
    let declared = payload
        .get("survivors")
        .and_then(Value::as_array)
        .ok_or("Mull survivors missing")?;
    let declared: Vec<&str> = declared
        .iter()
        .map(|v| v.as_str().ok_or("invalid Mull survivor id"))
        .collect::<Result<_, _>>()?;
    if declared.len() != survivors.len()
        || declared.iter().copied().collect::<BTreeSet<_>>() != survivors
    {
        return Err("Mull survivors differ from mutant rows".into());
    }
    if (status == "PASS" && survived != 0)
        || (status == "RATCHET_HELD" && survived == 0)
        || payload.get("mutation_score").and_then(Value::as_f64)
            != Some(killed as f64 / total as f64)
    {
        return Err("Mull status or score differs from mutant rows".into());
    }
    Ok(())
}

fn validate_counts(
    payload: &Map<String, Value>,
    total: u64,
    killed: u64,
    survived: u64,
) -> Result<(), String> {
    let counts = payload
        .get("outcome_counts")
        .and_then(Value::as_object)
        .ok_or("Mull outcome counts missing")?;
    if counts.len() != 6
        || [
            ("KILLED", killed),
            ("SURVIVED", survived),
            ("NO_COVERAGE", 0),
            ("UNVIABLE", 0),
            ("ERROR", 0),
            ("TIMED_OUT", 0),
        ]
        .iter()
        .any(|(key, expected)| counts.get(*key).and_then(Value::as_u64) != Some(*expected))
    {
        return Err("Mull outcome counts differ from mutant rows".into());
    }
    let accounting = payload
        .get("accounting")
        .and_then(Value::as_object)
        .ok_or("Mull accounting missing")?;
    if [
        ("planned", total),
        ("completed", total),
        ("failed", 0),
        ("censored", 0),
        ("unstarted", 0),
    ]
    .iter()
    .any(|(key, expected)| accounting.get(*key).and_then(Value::as_u64) != Some(*expected))
        || payload.get("complete_campaign") != Some(&Value::Bool(true))
    {
        return Err("Mull campaign accounting incomplete".into());
    }
    for field in ["baseline", "engine_result"] {
        let result = payload
            .get(field)
            .and_then(Value::as_object)
            .ok_or_else(|| format!("Mull {field} missing"))?;
        if result.get("returncode").and_then(Value::as_i64) != Some(0)
            || result.get("timed_out") != Some(&Value::Bool(false))
        {
            return Err(format!("Mull {field} did not succeed"));
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "mutation_mull_outcome_tests.rs"]
mod tests;
