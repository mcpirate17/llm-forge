//! cargo-mutants, the Rust engine. Its report is `mutants.out/outcomes.json`:
//! one baseline scenario, then one scenario per mutant. Exit 2 means viable
//! mutants survived, the ordinary reading of an uncurated corpus, so the
//! survivor set decides the verdict rather than the exit code.

#[path = "mutation_cargo_execute.rs"]
mod value;

use crate::mutation_campaign::{
    identify, py_repr, require_no_drift, Campaign, Step, ERROR, KILLED, SURVIVED, TIMED_OUT,
    UNVIABLE,
};
use crate::mutation_process::{rounded, CommandResult};
use crate::mutation_run::EngineRun;
use crate::mutation_snapshot::on_path;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::Path;

const SUMMARY_KEYS: [&str; 5] = ["total_mutants", "caught", "missed", "timeout", "unviable"];
/// Clean, some mutant missed, some mutant timed out. Every other exit is a usage
/// error, a red baseline or an internal failure, none of which measured anything.
const VERDICT_EXITS: [i32; 3] = [0, 2, 3];

fn outcome(summary: &str) -> Option<&'static str> {
    Some(match summary {
        "CaughtMutant" => KILLED,
        "MissedMutant" => SURVIVED,
        "Timeout" => TIMED_OUT,
        "Unviable" => UNVIABLE,
        "Failure" => ERROR,
        _ => return None,
    })
}

/// The cargo-mutants executable, which ships as a cargo subcommand.
pub fn binary() -> Result<String, String> {
    on_path("cargo-mutants")
        .map(|found| found.to_string_lossy().into_owned())
        .ok_or_else(|| {
            "cargo-mutants is not installed; `cargo install cargo-mutants` (it is a \
             developer tool, not a shipped dependency, so it belongs in the toolchain \
             rather than in pyproject)"
                .to_string()
        })
}

/// The mutant a scenario describes, or None for the baseline scenario.
fn mutant_of(outcome: &Value) -> Result<Option<&Value>, String> {
    match outcome.get("scenario") {
        Some(Value::String(_)) => Ok(None),
        Some(scenario) if scenario.get("Mutant").is_some() => Ok(scenario.get("Mutant")),
        scenario => Err(format!(
            "cargo-mutants reported an unknown scenario: {}",
            scenario.unwrap_or(&Value::Null)
        )),
    }
}

fn position(mutant: &Value) -> Result<(u64, u64), String> {
    let start = &mutant["span"]["start"];
    match (start["line"].as_u64(), start["column"].as_u64()) {
        (Some(line), Some(column)) => Ok((line, column)),
        _ => Err("cargo-mutants reported a mutant with no span".to_string()),
    }
}

fn text(mutant: &Value, key: &str) -> Result<String, String> {
    let value = mutant
        .get(key)
        .ok_or_else(|| format!("cargo-mutants reported a mutant with no {key}"))?;
    Ok(value
        .as_str()
        .map_or_else(|| value.to_string(), str::to_string))
}

/// What the mutant replaced, named by function rather than by text: cargo-mutants
/// publishes the replacement but not the text it replaced.
fn original(mutant: &Value) -> String {
    let function = mutant.get("function");
    let field = |key: &str| {
        function
            .and_then(|value| value.get(key))
            .and_then(Value::as_str)
            .unwrap_or_default()
    };
    let name = field("function_name");
    let name = if name.is_empty() { "<file>" } else { name };
    format!("{name}{}", field("return_type"))
}

fn duration(outcome: &Value) -> f64 {
    outcome
        .get("phase_results")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|phase| phase.get("duration").and_then(Value::as_f64))
        .sum()
}

type Located<'a> = (String, (u64, u64), &'a Value, &'a Value);

fn located(report: &Value) -> Result<Vec<Located<'_>>, String> {
    let mut mutants = Vec::new();
    for outcome in outcomes(report) {
        if let Some(mutant) = mutant_of(outcome)? {
            mutants.push((text(mutant, "file")?, position(mutant)?, mutant, outcome));
        }
    }
    mutants.sort_by(|left, right| (&left.0, left.1).cmp(&(&right.0, right.1)));
    Ok(mutants)
}

fn outcomes(report: &Value) -> impl Iterator<Item = &Value> {
    report
        .get("outcomes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
}

/// Name and record every mutant the run produced, deterministically.
pub(crate) fn rows(report: &Value, package_root: &str) -> Result<Vec<Value>, String> {
    let mutants = located(report)?;
    let mut entries = Vec::with_capacity(mutants.len());
    for (file, _, mutant, _) in &mutants {
        let joined = format!("{package_root}/{file}");
        entries.push([
            joined.trim_start_matches('/').to_string(),
            text(mutant, "genre")?,
            original(mutant),
            text(mutant, "replacement")?,
        ]);
    }
    let borrowed: Vec<[&str; 4]> = entries
        .iter()
        .map(|[path, genre, original, replacement]| {
            [
                path.as_str(),
                genre.as_str(),
                original.as_str(),
                replacement.as_str(),
            ]
        })
        .collect();
    let names = identify(&borrowed);
    let mut rows = Vec::with_capacity(mutants.len());
    for (((_, (line, _), mutant, outcome_row), entry), name) in
        mutants.iter().zip(&entries).zip(&names)
    {
        let summary = outcome_row
            .get("summary")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let outcome = outcome(summary).ok_or_else(|| {
            format!(
                "cargo-mutants reported unknown summary {}",
                py_repr(summary)
            )
        })?;
        rows.push(json!({
            "id": name,
            "outcome": outcome,
            "path": entry[0],
            "line": line,
            "operator": entry[1],
            "original_text": entry[2],
            "mutated_text": entry[3],
            "function": mutant.get("function").and_then(|f| f.get("function_name")),
            "package": mutant.get("package"),
            "duration_seconds": rounded(duration(outcome_row)),
        }));
    }
    Ok(rows)
}

/// Refuse a run whose own unmutated baseline did not pass: every mutant after it
/// would read as caught by an already-red suite.
pub(crate) fn require_baseline(report: &Value) -> Result<(), String> {
    for outcome in outcomes(report) {
        if mutant_of(outcome)?.is_none() {
            let summary = outcome.get("summary").and_then(Value::as_str);
            return match summary {
                Some("Success") => Ok(()),
                other => Err(format!(
                    "cargo-mutants' unmutated baseline failed: {}",
                    other.unwrap_or("None")
                )),
            };
        }
    }
    Err("cargo-mutants reported no baseline scenario".into())
}

/// Refuse a report from a run that ended in something other than a verdict.
fn require_verdict_exit(result: &CommandResult) -> Result<(), String> {
    if result
        .returncode
        .is_some_and(|code| VERDICT_EXITS.contains(&code))
    {
        return Ok(());
    }
    Err(format!("cargo-mutants failed (rc={})", result.rc_text()))
}

/// Refuse workers that would share one build directory: builds clobber each
/// other and the same mutant reads differently run to run, so no ratchet holds.
fn require_isolated_builds(campaign: &Campaign) -> Result<(), String> {
    if campaign.jobs > 1 && campaign.environment.contains_key("CARGO_TARGET_DIR") {
        return Err(format!(
            "environment.CARGO_TARGET_DIR is set with generator.jobs={}: parallel \
             cargo-mutants workers would share one build directory and classify the \
             same mutant differently run to run. Drop it, or set jobs to 1.",
            campaign.jobs
        ));
    }
    Ok(())
}

/// The cargo-mutants invocation, every bound pinned by the manifest.
pub(crate) fn engine_argv(
    campaign: &Campaign,
    binary: &str,
    output: &Path,
) -> Result<Vec<String>, String> {
    require_isolated_builds(campaign)?;
    let manifest = campaign.option_text("manifest_path").ok_or_else(|| {
        "generator.options.manifest_path must name the crate's Cargo.toml".to_string()
    })?;
    // cargo-mutants resolves a leaf manifest's parent as an empty path. Preserve
    // root-package manifests by spelling their current-directory parent explicitly.
    let manifest = if Path::new(&manifest)
        .parent()
        .is_some_and(|parent| parent.as_os_str().is_empty())
    {
        format!("./{manifest}")
    } else {
        manifest
    };
    let mut argv = vec![
        binary.to_string(),
        "mutants".to_string(),
        "--manifest-path".to_string(),
        manifest,
        "--jobs".to_string(),
        campaign.jobs.to_string(),
        "--timeout".to_string(),
        campaign.mutant_timeout()?.to_string(),
        "--output".to_string(),
        output.to_string_lossy().into_owned(),
    ];
    if let Some(package) = campaign.option_text("package") {
        argv.extend(["--package".to_string(), package]);
    }
    for pattern in &campaign.source {
        argv.extend(["--file".to_string(), pattern.clone()]);
    }
    for pattern in &campaign.exclude {
        argv.extend(["--exclude".to_string(), pattern.clone()]);
    }
    Ok(argv)
}

fn read_report(output: &Path) -> Result<Value, String> {
    let outcomes = output.join("mutants.out").join("outcomes.json");
    if !outcomes.is_file() {
        return Err(format!(
            "cargo-mutants wrote no outcomes.json at {}",
            outcomes.display()
        ));
    }
    let text = fs::read_to_string(&outcomes)
        .map_err(|error| format!("cannot read {}: {error}", outcomes.display()))?;
    let report: Value = serde_json::from_str(&text)
        .map_err(|error| format!("cargo-mutants outcomes.json is not JSON: {error}"))?;
    if !report.is_object() {
        return Err("cargo-mutants outcomes.json root must be a JSON object".into());
    }
    Ok(report)
}

/// What every cargo run in a snapshot inherits, and nothing it shouldn't.
fn environment(run: &EngineRun) -> BTreeMap<String, String> {
    let mut bound = run.guest_environment(&run.campaign.environment);
    bound.extend([
        ("PATH".to_string(), env::var("PATH").unwrap_or_default()),
        ("CARGO_TERM_COLOR".to_string(), "never".to_string()),
    ]);
    bound
}

/// The cargo-mutants totals, plus the two fields `completion_errors` reads.
fn summary(report: &Value, rows: &[Value]) -> Value {
    let mut picked: Map<String, Value> = SUMMARY_KEYS
        .iter()
        .filter_map(|key| {
            report
                .get(*key)
                .map(|value| ((*key).to_string(), value.clone()))
        })
        .collect();
    let errors = rows
        .iter()
        .filter(|row| row["outcome"].as_str() == Some(ERROR))
        .count();
    if let Some(total) = report.get("total_mutants") {
        picked.insert("mutants_generated".into(), total.clone());
    }
    picked.insert("errors".into(), json!(errors));
    Value::Object(picked)
}

/// Run the crate's own tests, then cargo-mutants, into the receipt.
pub fn execute(run: &mut EngineRun, binary: &str) -> Step<()> {
    if run.campaign.generated_value.is_some() {
        return value::execute(run, binary);
    }
    require_no_drift(run.campaign, run.worktree)?;
    let worktree = run.worktree.to_path_buf();
    let timeout = run.campaign.run_timeout_seconds;
    let bound = environment(run);
    let baseline_argv = run.campaign.test_argv.clone();
    let (baseline, _) = run.run(&baseline_argv, &worktree, timeout, &bound, &[])?;
    run.note_baseline(&baseline, &baseline_argv)?;
    let output = worktree.join(".cargo-mutants-out");
    let argv = engine_argv(run.campaign, binary, &output)?;
    let (result, _) = run.run(&argv, &worktree, timeout, &bound, &[])?;
    run.receipt
        .insert("engine_result".into(), result.as_value());
    if result.timed_out {
        return Err(format!("cargo-mutants timed out after {timeout}s").into());
    }
    let report = read_report(&output)?;
    run.planned = report.get("total_mutants").and_then(Value::as_u64);
    require_baseline(&report)?;
    require_verdict_exit(&result)?;
    let version = report.get("cargo_mutants_version").cloned();
    run.receipt
        .insert("engine_version".into(), version.unwrap_or(Value::Null));
    let package_root = run.campaign.option_text("package_root").unwrap_or_default();
    let mutants = rows(&report, &package_root)?;
    run.receipt
        .insert("engine_summary".into(), summary(&report, &mutants));
    run.receipt.insert("mutants".into(), Value::Array(mutants));
    run.require_scored_rows()
}

#[cfg(test)]
#[path = "mutation_engine_cargo_tests.rs"]
mod tests;
