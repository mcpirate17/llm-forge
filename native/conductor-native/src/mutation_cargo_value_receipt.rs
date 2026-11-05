//! Recompute generated Cargo test value from held contracts and retained outcomes.
//! This establishes receipt consistency, not authenticity of its producer or logs.
use crate::mutation_rust_value_contract::{self, Declaration};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet};

const SCHEMA: &str = "llm.mutation-testing.cargo-value-observations.v1";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Observations {
    schema_version: String,
    inventory_digest: String,
    baseline_reports: Vec<Value>,
    engine_baseline: Observation,
    mutants: Vec<Mutant>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Observation {
    report: Value,
    log_path: String,
    log_sha256: String,
    phases: Value,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Mutant {
    id: String,
    report: Value,
    log_path: String,
    log_sha256: String,
    phases: Value,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Report {
    status: String,
    tests: BTreeMap<String, Case>,
    missing_nodeids: Vec<Value>,
    ambiguous_nodeids: Vec<Value>,
    unmapped_cases: Vec<Value>,
    unranked_failures: Vec<Value>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    outcome: String,
    cases: u64,
}

fn require(condition: bool, error: &str) -> Result<(), String> {
    if condition {
        Ok(())
    } else {
        Err(error.into())
    }
}
fn provenance(path: &str, sha: &str, phases: &Value) -> Result<(), String> {
    require(
        path.len() <= 4096
            && !path.contains('\\')
            && crate::mutation_manifest::safe_relative(path, "scenario log")? == path,
        "invalid scenario log path",
    )?;
    require(
        sha.len() == 64
            && sha
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "invalid scenario log digest",
    )?;
    require(
        phases.as_array().is_some_and(|v| v.len() == 2),
        "expected build and test phase provenance",
    )
}
fn report(value: &Value, ranked: &BTreeSet<String>, killed: bool) -> Result<Value, String> {
    require(
        super::inputs::measure(value)? <= 1024 * 1024,
        "normalized report exceeds 1 MiB",
    )?;
    let parsed: Report = serde_json::from_value(value.clone()).map_err(|e| e.to_string())?;
    require(
        parsed.status == "COMPLETE"
            && parsed.missing_nodeids.is_empty()
            && parsed.ambiguous_nodeids.is_empty()
            && parsed.unmapped_cases.is_empty()
            && parsed.unranked_failures.is_empty(),
        "Cargo report is not complete",
    )?;
    require(
        parsed.tests.keys().eq(ranked.iter()),
        "Cargo report test inventory differs from declaration",
    )?;
    let mut failed = Vec::new();
    for (node, case) in &parsed.tests {
        require(
            case.cases == 1 && matches!(case.outcome.as_str(), "PASSED" | "FAILED"),
            "Cargo case is not one executed test",
        )?;
        if case.outcome == "FAILED" {
            failed.push(node.clone());
        }
    }
    require(
        killed != failed.is_empty(),
        "Cargo case outcomes disagree with scenario",
    )?;
    let mut result = value.clone();
    // The scorer prefers this field. Derive it from case outcomes, never the receipt.
    result["failed_nodeids"] = json!(failed);
    Ok(result)
}
fn mutant_ids(rows: &Value, sources: &[String]) -> Result<BTreeSet<String>, String> {
    let rows = rows.as_array().ok_or("Cargo mutant rows missing")?;
    require(
        !rows.is_empty() && rows.len() <= 1000,
        "Cargo mutant count must be 1..1000",
    )?;
    let mut ids = BTreeSet::new();
    let mut paths = BTreeSet::new();
    for row in rows {
        let id = row["id"].as_str().ok_or("mutant ID missing")?;
        let path = row["path"].as_str().ok_or("mutant source missing")?;
        require(
            !id.is_empty() && id.len() <= 4096 && ids.insert(id.into()),
            "duplicate or invalid Cargo mutant ID",
        )?;
        require(
            row["outcome"] == "KILLED" && sources.iter().any(|s| s == path),
            "Cargo value requires killed mutants in declared source scope",
        )?;
        paths.insert(path.to_owned());
    }
    require(
        paths == sources.iter().cloned().collect(),
        "Cargo mutation evidence does not measure every declared source",
    )?;
    Ok(ids)
}
fn spec(declaration: &Declaration, ids: &BTreeSet<String>) -> Value {
    let contract = &declaration.required_contracts()[0];
    let tests: Vec<Value> = declaration.ranked_tests().iter().map(|test|
        json!({"nodeid":test.nodeid(),"contract_id":test.contract(),"intentional_redundancy":false})).collect();
    let pairs: Vec<Value> = ids.iter().map(|id| json!([id, contract.id()])).collect();
    json!({"adapter":"cargo-libtest", "baseline_repetitions":declaration.baseline_repetitions(),
        "contracts":[{"id":contract.id(),"criticality":contract.criticality(),"active_paths":contract.active_paths()}],
        "tests":tests,"mutation_contracts":pairs})
}
fn matrix(
    observations: Vec<Mutant>,
    ranked: &BTreeSet<String>,
    ids: &BTreeSet<String>,
    baseline_log: &str,
) -> Result<Value, String> {
    require(
        observations.len() == ids.len(),
        "Cargo observation count differs from mutants",
    )?;
    let mut reports = Map::new();
    let mut logs = BTreeSet::from([baseline_log.to_owned()]);
    for row in observations {
        require(
            ids.contains(&row.id) && !reports.contains_key(&row.id),
            "missing, extra or duplicate Cargo observation",
        )?;
        provenance(&row.log_path, &row.log_sha256, &row.phases)?;
        require(
            logs.insert(row.log_path),
            "Cargo scenarios reuse a log path",
        )?;
        reports.insert(row.id, report(&row.report, ranked, true)?);
    }
    Ok(Value::Object(reports))
}

/// `metadata` and `source_sha256` must come from the independently held campaign.
/// Receipt rows supply observed mutant IDs/outcomes, never contracts or importance.
pub fn evaluate(
    metadata: &Value,
    source_sha256: &Value,
    mutants: &Value,
    observations: &Value,
) -> Result<(Value, Value), String> {
    require(
        super::inputs::measure(observations)? <= 16 * 1024 * 1024,
        "Cargo observations exceed 16 MiB",
    )?;
    let sources: Vec<String> = source_sha256
        .as_object()
        .ok_or("source bindings missing")?
        .keys()
        .cloned()
        .collect();
    let declaration = mutation_rust_value_contract::parse(
        &serde_json::to_vec(&metadata["declaration"]).map_err(|e| e.to_string())?,
        &sources,
    )?;
    let observed: Observations =
        serde_json::from_value(observations.clone()).map_err(|e| e.to_string())?;
    require(
        observed.schema_version == SCHEMA
            && metadata["inventory"]["digest"].as_str() == Some(&observed.inventory_digest),
        "Cargo observation schema or inventory binding mismatch",
    )?;
    require(
        observed.baseline_reports.len() == declaration.baseline_repetitions() as usize,
        "Cargo baseline repetition count differs from declaration",
    )?;
    let ranked: BTreeSet<String> = declaration
        .ranked_tests()
        .iter()
        .map(|t| t.nodeid().to_owned())
        .collect();
    let ids = mutant_ids(mutants, &sources)?;
    require(
        (ids.len() + observed.baseline_reports.len() + 1).saturating_mul(ranked.len()) <= 100_000,
        "Cargo observation matrix exceeds 100000 cases",
    )?;
    let baseline = &observed.engine_baseline;
    provenance(&baseline.log_path, &baseline.log_sha256, &baseline.phases)?;
    report(&baseline.report, &ranked, false)?;
    let baselines = observed
        .baseline_reports
        .iter()
        .map(|value| report(value, &ranked, false))
        .collect::<Result<Vec<_>, _>>()?;
    let reports = matrix(observed.mutants, &ranked, &ids, &baseline.log_path)?;
    let outcomes: Map<String, Value> = ids.iter().map(|id| (id.clone(), json!("KILLED"))).collect();
    let encoded = crate::mutation_value::analyze_reports(
        spec(&declaration, &ids),
        baselines,
        &reports,
        &Value::Object(outcomes),
    )?;
    let value: Value = serde_json::from_str(&encoded).map_err(|e| e.to_string())?;
    require(
        value["status"] == "PASS",
        "Cargo test value analysis did not pass",
    )?;
    let attribution = json!({"schema_version":"llm.mutation-testing.attribution.v1","status":"ATTRIBUTED",
        "baseline_repetitions":declaration.baseline_repetitions(),"ranked_tests":ranked.len(),
        "killed_mutants":ids.len(),"attributed_mutants":ids.len(),"unusable_nodeids":[],"unattributed":[]});
    Ok((value, attribution))
}

#[cfg(test)]
#[path = "mutation_cargo_value_receipt_tests.rs"]
mod tests;
