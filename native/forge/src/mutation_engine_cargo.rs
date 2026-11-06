//! cargo-mutants, the Rust engine. Its report is `mutants.out/outcomes.json`:
//! one baseline scenario, then one scenario per mutant. Exit 2 means viable
//! mutants survived, the ordinary reading of an uncurated corpus, so the
//! survivor set decides the verdict rather than the exit code.

#[path = "mutation_cargo_partial.rs"]
mod partial;
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
    ];
    if conductor_native::mutation_workspace_selector::is_workspace(
        campaign.option_text("execution_profile").as_deref(),
    ) {
        if campaign.jobs != 1 {
            return Err("in-place workspace mutation requires exactly one worker".into());
        }
        // cargo-mutants 27.1 rejects --jobs with --in-place; its omitted-jobs
        // default is one worker. The admitted workspace already requires jobs=1.
        argv.push("--in-place".into());
    } else {
        argv.extend(["--jobs".into(), campaign.jobs.to_string()]);
    }
    argv.extend([
        "--timeout".to_string(),
        campaign.mutant_timeout()?.to_string(),
        "--output".to_string(),
        output.to_string_lossy().into_owned(),
    ]);
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

// Outcomes are payload-controlled; cap the complete JSON before parsing and hold
// the no-follow path/bytes until all receipt observations have been derived.
const MAX_OUTCOMES_BYTES: usize = 16 * 1024 * 1024;
fn read_report(output: &Path) -> Result<(crate::candidate_inputs::BoundFile, Value), String> {
    let outcomes = output.join("mutants.out").join("outcomes.json");
    let (held, bytes) = crate::candidate_inputs::repository_live(&outcomes, MAX_OUTCOMES_BYTES)
        .map_err(|error| format!("cannot capture {}: {error:#}", outcomes.display()))?;
    let report: Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("cargo-mutants outcomes.json is not JSON: {error}"))?;
    if !report.is_object() {
        return Err("cargo-mutants outcomes.json root must be a JSON object".into());
    }
    Ok((held, report))
}

fn record_partial(
    run: &mut EngineRun<'_>,
    output: &Path,
    selected: &Campaign,
    argv: &[String],
    reason: &str,
) {
    let cleanup = run
        .receipt
        .get("process_cleanup")
        .and_then(Value::as_object);
    let last = run
        .receipt
        .get("process_commands")
        .and_then(Value::as_array)
        .and_then(|rows| rows.last());
    let safe = cleanup.and_then(|v| v.get("complete")) == Some(&Value::Bool(true))
        && last.and_then(|v| v.get("state")).and_then(Value::as_str) == Some("observed")
        && last.and_then(|v| v.get("argv")) == Some(&json!(argv))
        && last
            .and_then(|v| v.get("confinement"))
            .and_then(|v| v.get("cleanup_complete"))
            == Some(&Value::Bool(true));
    let result = if safe {
        partial::collect(output, selected, reason)
    } else {
        Err("owned engine cleanup is not verified; progress was not read".into())
    };
    match result {
        Ok(value) => {
            run.receipt.insert("engine_partial_progress".into(), value);
        }
        Err(error) => {
            let bounded: String = error.chars().take(1024).collect();
            let suffix = if bounded.len() < error.len() {
                " [truncated]"
            } else {
                ""
            };
            run.receipt.insert(
                "engine_partial_progress_error".into(),
                json!(format!("{bounded}{suffix}")),
            );
        }
    }
}

fn require_package(rows: &[Value], expected: Option<&str>) -> Result<(), String> {
    if let Some(expected) = expected {
        if rows
            .iter()
            .any(|row| row["package"].as_str() != Some(expected))
        {
            return Err("cargo-mutants package differs from admitted workspace package".into());
        }
    }
    Ok(())
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
    let (baseline_argv, test_arguments) = run
        .runtime
        .ok_or("mutation payload requires an admitted confined execution session")?
        .test_arguments(run.campaign)
        .map_err(|e| e.to_string())?;
    let (baseline, baseline_stdout) = run.run(&baseline_argv, &worktree, timeout, &bound, &[])?;
    run.note_baseline(&baseline, &baseline_argv)?;
    run.runtime
        .ok_or("confined runtime missing")?
        .observe_baseline(run, &baseline_stdout)?;
    let output = run
        .runtime
        .ok_or("mutation payload requires an admitted confined execution session")?
        .output();
    let runtime = run.runtime.ok_or("confined runtime missing")?;
    let selected = runtime
        .engine_campaign(run.campaign)
        .map_err(|e| e.to_string())?;
    let mut argv = engine_argv(&selected, binary, &output)?;
    argv.extend([
        "--no-config".into(),
        "--cargo-arg=--locked".into(),
        "--cargo-arg=--offline".into(),
    ]);
    argv.extend(test_arguments);
    let (result, _) = match run.run(&argv, &worktree, timeout, &bound, &[]) {
        Ok(result) => result,
        Err(stop) => {
            record_partial(run, &output, &selected, &argv, "actor_error");
            return Err(stop);
        }
    };
    run.receipt
        .insert("engine_result".into(), result.as_value());
    if result.timed_out {
        record_partial(run, &output, &selected, &argv, "timeout");
        return Err(format!("cargo-mutants timed out after {timeout}s").into());
    }
    if !result
        .returncode
        .is_some_and(|code| VERDICT_EXITS.contains(&code))
    {
        record_partial(run, &output, &selected, &argv, "nonverdict_exit");
    }
    let (held_report, report) = read_report(&output)?;
    run.planned = report.get("total_mutants").and_then(Value::as_u64);
    require_baseline(&report)?;
    require_verdict_exit(&result)?;
    runtime.observe_engine_baseline(run, &report)?;
    let version = report.get("cargo_mutants_version").cloned();
    run.receipt
        .insert("engine_version".into(), version.unwrap_or(Value::Null));
    let package_root = runtime
        .report_root(run.campaign)
        .map_err(|e| e.to_string())?;
    let mutants = rows(&report, &package_root)?;
    require_package(
        &mutants,
        runtime.report_package().map_err(|e| e.to_string())?,
    )?;
    held_report
        .verify()
        .map_err(|error| format!("engine outcomes changed during observation: {error:#}"))?;
    run.receipt
        .insert("engine_summary".into(), summary(&report, &mutants));
    run.receipt.insert("mutants".into(), Value::Array(mutants));
    run.require_scored_rows()
}

#[cfg(test)]
#[path = "mutation_engine_cargo_tests.rs"]
mod tests;
