//! Bounded cooperative cargo-mutants progress; never scored evidence.
use crate::candidate_inputs::{self, BoundFile};
use crate::mutation_campaign::Campaign;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

const MAX_BYTES: usize = 16 * 1024 * 1024;
const MAX_ROWS: usize = 10_000;
const MAX_IDENTITY: usize = 16 * 1024;

fn capture(path: &Path) -> Result<(BoundFile, Vec<u8>, Value), String> {
    let (held, bytes) = candidate_inputs::repository_live(path, MAX_BYTES)
        .map_err(|e| format!("cannot capture {}: {e:#}", path.display()))?;
    let text = std::str::from_utf8(&bytes).map_err(|e| format!("progress is not UTF-8: {e}"))?;
    let value = crate::tool_schema::parse_document_with_limit(text, MAX_BYTES)
        .map_err(|e| format!("invalid progress JSON: {e:#}"))?;
    Ok((held, bytes, value))
}

fn identity(value: &Value, sources: &BTreeSet<String>, package: &str) -> Result<String, String> {
    let mut item = value
        .as_object()
        .ok_or("mutant identity is not an object")?
        .clone();
    item.remove("diff");
    let keys = [
        "name",
        "package",
        "file",
        "function",
        "span",
        "replacement",
        "genre",
    ];
    if item.len() != keys.len() || keys.iter().any(|key| !item.contains_key(*key)) {
        return Err("invalid mutant identity shape".into());
    }
    for key in ["name", "replacement", "genre"] {
        if !item.get(key).and_then(Value::as_str).is_some_and(|s| {
            s.len() <= 4096
                && !s.contains('\0')
                && (key == "replacement" || (!s.is_empty() && !s.chars().any(char::is_control)))
        }) {
            return Err(format!("invalid mutant {key}"));
        }
    }
    if !item.get("function").is_some_and(|v| {
        v.is_null()
            || v.as_object().is_some_and(|o| {
                ["function_name", "return_type"].iter().all(|key| {
                    o.get(*key)
                        .and_then(Value::as_str)
                        .is_some_and(|s| s.len() <= 4096 && !s.chars().any(char::is_control))
                }) && o.get("span").is_some_and(Value::is_object)
            })
    }) {
        return Err("invalid mutant function".into());
    }
    let span = item
        .get("span")
        .and_then(Value::as_object)
        .ok_or("mutant span missing")?;
    for key in ["start", "end"] {
        let point = span
            .get(key)
            .and_then(Value::as_object)
            .ok_or("mutant span endpoint missing")?;
        if ["line", "column"]
            .iter()
            .any(|k| !point.get(*k).and_then(Value::as_u64).is_some_and(|n| n > 0))
        {
            return Err("invalid mutant span endpoint".into());
        }
    }
    if item.get("package").and_then(Value::as_str) != Some(package) {
        return Err("mutant package outside selected campaign".into());
    }
    let file = item
        .get("file")
        .and_then(Value::as_str)
        .ok_or("mutant source missing")?;
    if !sources.contains(file) {
        return Err("mutant source outside selected campaign".into());
    }
    if conductor_native::mutation_source_coverage::normalise(file)? != file {
        return Err("mutant source path is not normalized".into());
    }
    let encoded = serde_json::to_string(&item).map_err(|e| e.to_string())?;
    if encoded.len() > MAX_IDENTITY {
        return Err("mutant identity exceeds bound".into());
    }
    Ok(encoded)
}

fn inventory(
    value: &Value,
    sources: &BTreeSet<String>,
    package: &str,
) -> Result<BTreeSet<String>, String> {
    let rows = value
        .as_array()
        .filter(|rows| rows.len() <= MAX_ROWS)
        .ok_or("invalid mutant inventory")?;
    let mut result = BTreeSet::new();
    for row in rows {
        if !result.insert(identity(row, sources, package)?) {
            return Err("duplicate mutant inventory identity".into());
        }
    }
    Ok(result)
}

fn check_phase_summary(row: &Value, summary: &str) -> Result<(), String> {
    let phases = row
        .get("phase_results")
        .and_then(Value::as_array)
        .filter(|p| !p.is_empty() && p.len() <= 3)
        .ok_or("invalid progress phase list")?;
    let mut build_failure = false;
    let mut timeout = false;
    for phase in phases {
        let name = phase
            .get("phase")
            .and_then(Value::as_str)
            .ok_or("progress phase missing")?;
        if !matches!(name, "Check" | "Build" | "Test") {
            return Err("unknown progress phase".into());
        }
        let status = phase
            .get("process_status")
            .ok_or("progress phase status missing")?;
        let failed = status.as_object().is_some_and(|o| {
            o.len() == 1
                && o.get("Failure")
                    .and_then(Value::as_i64)
                    .is_some_and(|n| n != 0)
        });
        let signal = status.as_object().is_some_and(|o| {
            o.len() == 1
                && o.get("Signalled")
                    .and_then(Value::as_i64)
                    .is_some_and(|n| n > 0)
        });
        if !failed && !signal && !matches!(status.as_str(), Some("Success" | "Timeout" | "Other")) {
            return Err("invalid progress phase status".into());
        }
        build_failure |= name != "Test" && failed;
        timeout |= status == "Timeout";
    }
    let last = phases.last().ok_or("progress phase missing")?;
    let status = &last["process_status"];
    let failed = status
        .as_object()
        .is_some_and(|o| o.contains_key("Failure"));
    let actual = if build_failure {
        "Unviable"
    } else if timeout {
        "Timeout"
    } else if last["phase"] == "Test" && failed {
        "CaughtMutant"
    } else if last["phase"] == "Test" && status == "Success" {
        "MissedMutant"
    } else {
        "Failure"
    };
    if summary != actual {
        return Err("progress phase outcome differs from summary".into());
    }
    Ok(())
}

fn outcomes(
    value: &Value,
    inventory: &BTreeSet<String>,
    sources: &BTreeSet<String>,
    package: &str,
) -> Result<(BTreeMap<&'static str, u64>, String), String> {
    if value.get("cargo_mutants_version").and_then(Value::as_str) != Some("27.1.0") {
        return Err("unsupported cargo-mutants progress version".into());
    }
    let rows = value
        .get("outcomes")
        .and_then(Value::as_array)
        .filter(|rows| rows.len() <= MAX_ROWS + 1)
        .ok_or("invalid progress outcomes")?;
    let mut counts = BTreeMap::from([
        ("KILLED", 0),
        ("SURVIVED", 0),
        ("TIMED_OUT", 0),
        ("UNVIABLE", 0),
        ("ERROR", 0),
    ]);
    let mut seen = BTreeSet::new();
    let mut baseline: Option<&str> = None;
    for row in rows {
        let scenario = row.get("scenario").ok_or("progress scenario missing")?;
        if scenario == "Baseline" {
            if baseline.is_some() || !seen.is_empty() {
                return Err("duplicate progress baseline".into());
            }
            let summary = row.get("summary").and_then(Value::as_str);
            if !matches!(summary, Some("Success" | "Failure" | "Timeout")) {
                return Err("unknown progress baseline summary".into());
            }
            baseline = summary;
            continue;
        }
        if baseline != Some("Success") {
            return Err("progress mutant requires a successful prior baseline".into());
        }
        let object = scenario
            .as_object()
            .filter(|o| o.len() == 1)
            .ok_or("invalid progress scenario")?;
        let key = identity(
            object.get("Mutant").ok_or("unknown progress scenario")?,
            sources,
            package,
        )?;
        if !inventory.contains(&key) || !seen.insert(key) {
            return Err("unmatched or duplicate progress mutant".into());
        }
        let summary = row
            .get("summary")
            .and_then(Value::as_str)
            .ok_or("progress summary missing")?;
        check_phase_summary(row, summary)?;
        let outcome = super::outcome(summary).ok_or("unknown progress outcome")?;
        *counts.get_mut(outcome).ok_or("invalid progress outcome")? += 1;
    }
    if value.get("total_mutants").and_then(Value::as_u64) != Some(seen.len() as u64) {
        return Err("progress total differs from observed scenarios".into());
    }
    for (field, key) in [
        ("caught", "KILLED"),
        ("missed", "SURVIVED"),
        ("timeout", "TIMED_OUT"),
        ("unviable", "UNVIABLE"),
    ] {
        if value.get(field).and_then(Value::as_u64) != counts.get(key).copied() {
            return Err(format!("progress {field} count differs from outcomes"));
        }
    }
    if value.get("success").and_then(Value::as_u64) != Some(0) {
        return Err("unsupported progress success count".into());
    }
    Ok((counts, identity_digest(seen.into_iter())))
}

fn identity_digest(items: impl Iterator<Item = String>) -> String {
    let mut hash = Sha256::new();
    for item in items {
        hash.update((item.len() as u64).to_be_bytes());
        hash.update(item.as_bytes());
    }
    format!("{:x}", hash.finalize())
}

pub(super) fn collect(output: &Path, campaign: &Campaign, reason: &str) -> Result<Value, String> {
    let package = campaign
        .option_text("package")
        .ok_or("selected package unavailable")?;
    let root = output.join("mutants.out");
    let (inventory_held, inventory_bytes, inventory_value) = capture(&root.join("mutants.json"))?;
    let (outcome_held, outcome_bytes, outcome_value) = capture(&root.join("outcomes.json"))?;
    let sources: BTreeSet<String> = campaign.source.iter().cloned().collect();
    if sources.is_empty() || sources.len() > MAX_ROWS || sources.len() != campaign.source.len() {
        return Err("invalid selected source set".into());
    }
    let inventory = inventory(&inventory_value, &sources, &package)?;
    let (counts, observed_digest) = outcomes(&outcome_value, &inventory, &sources, &package)?;
    inventory_held
        .verify()
        .map_err(|e| format!("inventory changed: {e:#}"))?;
    outcome_held
        .verify()
        .map_err(|e| format!("outcomes changed: {e:#}"))?;
    let observed: u64 = counts.values().sum();
    let progress = json!({"schema_version":1,"complete":false,"reason":reason,
        "inventory":{"sha256":format!("{:x}",Sha256::digest(&inventory_bytes)),"bytes":inventory_bytes.len(),"discovered":inventory.len(),"identities_sha256":identity_digest(inventory.into_iter())},
        "outcomes":{"sha256":format!("{:x}",Sha256::digest(&outcome_bytes)),"bytes":outcome_bytes.len(),"observed":observed,"identities_sha256":observed_digest,"counts":counts},
        "remaining_or_interrupted":null,"scope":"partial automatic engine observations; not scored coverage or execution attestation"});
    if serde_json::to_vec(&progress)
        .map_err(|e| e.to_string())?
        .len()
        > 64 * 1024
    {
        return Err("partial progress exceeds receipt bound".into());
    }
    Ok(progress)
}

#[cfg(test)]
#[path = "mutation_cargo_partial_tests.rs"]
mod tests;
