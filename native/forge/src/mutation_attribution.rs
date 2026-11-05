//! Per-test kill attribution: re-apply each killed mutant inside the snapshot,
//! run its covering tests under JUnit, and hand the matrix to the value analysis.
//!
//! fest says a mutant died, not which test killed it. This measures that, once
//! per killed mutant, while the snapshot still exists.

use crate::mutation_bytecode::{evict_mutated_caches, scratch_root_for};
use crate::mutation_campaign::{pinned, py_list, py_repr, Campaign, Step, Stop, KILLED};
use crate::mutation_process::CommandResult;
use crate::mutation_run::EngineRun;
use crate::mutation_score::rows;
use conductor_native::mutation_value::analyze_reports;
use conductor_native::mutation_value_inputs::{parse_junit_file, JunitAdapter};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

const ATTRIBUTION_SCHEMA: &str = "llm.mutation-testing.attribution.v1";
const ADAPTER: &str = "pytest-junit";
/// The validator wants 2..=5; two is the floor that can catch a flickering test.
const BASELINE_REPETITIONS: usize = 2;
/// A re-run selects a handful of tests, so it gets a bound from what they cost
/// rather than the whole suite's: a deadlocked mutant otherwise burns 30 minutes.
const RERUN_TIMEOUT_MULTIPLIER: f64 = 10.0;
/// A sub-second baseline must not turn scheduler noise into a false hang.
const RERUN_TIMEOUT_FLOOR_SECONDS: i64 = 60;
/// A generated campaign has no author to grade it, so it claims the lower grade.
const CRITICALITY: &str = "high";
const TRUNCATING: [&str; 2] = ["-x", "--exitfirst"];
const DESELECTING: [&str; 2] = ["-k", "-m"];

/// The function-level nodeid a JUnit report can be matched back to.
///
/// JUnit aggregates a function's parameter cases, so cutting at the first `[` is
/// what makes the two ends agree. None for a nodeid with no `.py` module.
fn function(nodeid: &str) -> Option<String> {
    let (module, rest) = nodeid.split_once("::")?;
    if !module.ends_with(".py") {
        return None;
    }
    let name = rest.split('[').next().unwrap_or(rest);
    Some(format!("{module}::{name}"))
}

fn tests_run(row: &Value) -> impl Iterator<Item = &str> {
    row.get("tests_run")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
}

/// Every nodeid the coverage map names, and the ones it cannot use.
pub(crate) fn ranked(rows: &[Value]) -> (Vec<String>, Vec<String>) {
    let mut usable = BTreeSet::new();
    let mut unusable = BTreeSet::new();
    for nodeid in rows.iter().flat_map(tests_run) {
        match function(nodeid) {
            Some(name) => usable.insert(name),
            None => unusable.insert(nodeid.to_string()),
        };
    }
    (usable.into_iter().collect(), unusable.into_iter().collect())
}

/// The campaign's pytest flags, with its own selection and early exits cut.
pub(crate) fn narrowed(argv: &[String], worktree: &Path) -> Result<Vec<String>, String> {
    let start = argv
        .iter()
        .position(|arg| arg.rsplit('/').next() == Some("pytest"))
        .ok_or_else(|| {
            format!(
                "attribution needs a pytest test command; campaign runs {}",
                py_list(argv)
            )
        })?
        + 1;
    let mut kept = argv[..start].to_vec();
    let mut skip_value = false;
    for arg in &argv[start..] {
        if skip_value {
            skip_value = false;
        } else if TRUNCATING.contains(&arg.as_str()) || arg.starts_with("--maxfail") {
            skip_value = arg == "--maxfail";
        } else if DESELECTING.contains(&arg.as_str()) {
            skip_value = true;
        } else if arg.starts_with('-') || (!arg.contains("::") && !worktree.join(arg).exists()) {
            kept.push(arg.clone());
        }
    }
    Ok(kept)
}

/// The campaign's command, narrowed to exactly these tests, reporting JUnit.
pub(crate) fn selection(
    argv: &[String],
    worktree: &Path,
    nodeids: &[String],
    junit: &Path,
) -> Result<Vec<String>, String> {
    let mut selected = narrowed(argv, worktree)?;
    // A nested pytest.ini can move pytest's inferred rootdir below the snapshot
    // root, and JUnit class names then stop matching repo-relative node IDs.
    let rooted = selected
        .iter()
        .any(|arg| arg == "--rootdir" || arg.starts_with("--rootdir="));
    if !rooted {
        selected.push(format!("--rootdir={}", worktree.display()));
    }
    selected.extend(nodeids.iter().cloned());
    if selected
        .iter()
        .any(|arg| arg == "--junitxml" || arg.starts_with("--junitxml="))
    {
        return Err("baseline.argv must not set --junitxml".into());
    }
    selected.push(format!("--junitxml={}", junit.display()));
    Ok(selected)
}

/// Drop the target's cached bytecode, beside the source and in the run's cache.
///
/// CPython validates a `.pyc` against size and whole-second mtime, and a
/// one-operator mutation frequently changes neither.
fn invalidate(target: &Path, worktree: &Path) -> Result<(), String> {
    let cache = target.with_file_name("__pycache__");
    let stem = target
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    if cache.is_dir() {
        let entries = fs::read_dir(&cache)
            .map_err(|error| format!("cannot read {}: {error}", cache.display()))?;
        for entry in entries {
            let entry = entry.map_err(|error| error.to_string())?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with(&format!("{stem}.")) && name.ends_with(".pyc") {
                fs::remove_file(entry.path())
                    .map_err(|error| format!("cannot evict {name}: {error}"))?;
            }
        }
    }
    evict_mutated_caches(&[target.to_path_buf()], &scratch_root_for(worktree))
}

fn row_text<'a>(row: &'a Value, key: &str) -> Result<&'a str, String> {
    row.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("mutant row has no {key}"))
}

fn row_offset(row: &Value, key: &str) -> Result<usize, String> {
    row.get(key)
        .and_then(Value::as_u64)
        .map(|value| value as usize)
        .ok_or_else(|| format!("mutant row has no {key}"))
}

/// Re-apply one mutant byte-exactly around `body`, then put the file back.
pub(crate) fn with_mutant<T>(
    worktree: &Path,
    row: &Value,
    body: impl FnOnce() -> Step<T>,
) -> Step<T> {
    let target = worktree.join(row_text(row, "path")?);
    let offset = row_offset(row, "byte_offset")?;
    let length = row_offset(row, "byte_length")?;
    let original =
        fs::read(&target).map_err(|error| format!("cannot read {}: {error}", target.display()))?;
    let expected = row_text(row, "original_text")?.as_bytes();
    let end = offset.saturating_add(length).min(original.len());
    let found = original.get(offset.min(end)..end).unwrap_or_default();
    if offset > original.len() || found != expected {
        return Err(format!(
            "mutant {} does not match {} at byte {offset}: found b{}, expected b{}",
            row_text(row, "id")?,
            row_text(row, "path")?,
            py_repr(&String::from_utf8_lossy(found)),
            py_repr(&String::from_utf8_lossy(expected)),
        )
        .into());
    }
    let mut mutated = original[..offset].to_vec();
    mutated.extend_from_slice(row_text(row, "mutated_text")?.as_bytes());
    mutated.extend_from_slice(&original[end..]);
    fs::write(&target, mutated)
        .map_err(|error| format!("cannot mutate {}: {error}", target.display()))?;
    let applied = invalidate(&target, worktree).map_err(Stop::Refused);
    let outcome = applied.and_then(|()| body());
    let restored = fs::write(&target, &original)
        .map_err(|error| format!("cannot restore {}: {error}", target.display()))
        .and_then(|()| invalidate(&target, worktree));
    let value = outcome?;
    restored?;
    Ok(value)
}

/// One campaign-wide contract, because killers are not partitioned by file: a
/// mutant in one source is routinely killed only by a test paired with another.
pub(crate) fn spec(
    campaign: &Campaign,
    ranked: &[String],
    paths: &[String],
    ids: &[String],
) -> Value {
    let contract = &campaign.campaign_id;
    let tests: Vec<Value> = ranked
        .iter()
        .map(|nodeid| {
            json!({"nodeid": nodeid, "contract_id": contract, "intentional_redundancy": false})
        })
        .collect();
    let mutation_contracts: Vec<Value> = ids.iter().map(|id| json!([id, contract])).collect();
    json!({
        "adapter": ADAPTER,
        "baseline_repetitions": BASELINE_REPETITIONS,
        "contracts": [{"id": contract, "criticality": CRITICALITY, "active_paths": paths}],
        "tests": tests,
        "mutation_contracts": mutation_contracts,
    })
}

/// The per-mutant re-run limit: never above the campaign's, never below the floor.
pub(crate) fn rerun_timeout(cap_seconds: i64, slowest_baseline_seconds: f64) -> i64 {
    let scaled = (RERUN_TIMEOUT_MULTIPLIER * slowest_baseline_seconds).ceil() as i64;
    cap_seconds.min(scaled.max(RERUN_TIMEOUT_FLOOR_SECONDS))
}

/// What one selected run told us.
enum Measured {
    TimedOut,
    /// pytest died before writing XML: a mutant that aborted the interpreter.
    NoReport,
    Report(Value),
}

/// One attribution run's fixed inputs: where, how and under what limits.
struct Session<'a> {
    argv: Vec<String>,
    environment: &'a BTreeMap<String, String>,
    reports: PathBuf,
    timeout: i64,
    worktree: PathBuf,
}

impl Session<'_> {
    /// Run exactly these tests and read their outcomes back out.
    fn measure(
        &self,
        run: &mut EngineRun,
        nodeids: &[String],
        name: &str,
    ) -> Step<(CommandResult, Measured)> {
        let junit = self.reports.join(format!("{name}.xml"));
        let argv = selection(&self.argv, &self.worktree, nodeids, &junit)?;
        let (result, _) = run.run(&argv, &self.worktree, self.timeout, self.environment, &[])?;
        if result.timed_out {
            return Ok((result, Measured::TimedOut));
        }
        if !junit.is_file() {
            return Ok((result, Measured::NoReport));
        }
        let report = parse_junit_file(&junit, JunitAdapter::Pytest, nodeids)?;
        Ok((result, Measured::Report(report)))
    }

    /// The unmutated runs every mutant is compared against; their wall time then
    /// bounds the mutant re-runs.
    fn baselines(&mut self, run: &mut EngineRun, ranked: &[String]) -> Step<Vec<Value>> {
        let mut reports = Vec::new();
        let mut slowest = 0.0_f64;
        for index in 0..BASELINE_REPETITIONS {
            let (result, measured) = self.measure(run, ranked, &format!("baseline-{index}"))?;
            match measured {
                Measured::TimedOut => {
                    return Err(format!("attribution baseline {index} timed out").into());
                }
                Measured::NoReport => {
                    return Err(format!(
                        "attribution baseline {index} exited {} without a JUnit report",
                        result.rc_text()
                    )
                    .into());
                }
                Measured::Report(report) => reports.push(report),
            }
            slowest = slowest.max(result.duration_seconds);
        }
        self.timeout = rerun_timeout(self.timeout, slowest);
        Ok(reports)
    }
}

/// A heartbeat bounded to about ten lines: a silent re-run loop looks hung, and
/// killing a run that merely looks hung loses the whole receipt.
fn progress(position: usize, total: usize) {
    let step = (total / 10).max(1);
    if position == 0 || position + 1 == total || (position + 1).is_multiple_of(step) {
        eprintln!(
            "attribution: {}/{total} killed mutants re-run",
            position + 1
        );
    }
}

fn unattributed(summary: &mut Map<String, Value>, id: &str, reason: &str) {
    if let Some(list) = summary
        .get_mut("unattributed")
        .and_then(Value::as_array_mut)
    {
        list.push(json!({"id": id, "reason": reason}));
    }
}

/// Why one measured re-run charges no test, or None when it charges one.
fn unusable_reason(measured: &Measured) -> Option<&'static str> {
    match measured {
        Measured::TimedOut => Some("covering set timed out"),
        Measured::NoReport => Some("covering set crashed without a report"),
        Measured::Report(report) if report["status"].as_str() != Some("COMPLETE") => {
            Some("report does not map to nodeids")
        }
        Measured::Report(report)
            if report["failed_nodeids"]
                .as_array()
                .is_none_or(Vec::is_empty) =>
        {
            Some("covering set passed on re-run")
        }
        Measured::Report(_) => None,
    }
}

/// One JUnit report per killed mutant, over that mutant's covering set.
fn matrix(
    session: &Session,
    run: &mut EngineRun,
    killed: &[Value],
    ranked: &[String],
    summary: &mut Map<String, Value>,
) -> Step<Map<String, Value>> {
    let known: BTreeSet<&String> = ranked.iter().collect();
    let total = killed.len();
    let mut matrix = Map::new();
    for (position, row) in killed.iter().enumerate() {
        progress(position, total);
        let id = row_text(row, "id")?;
        let covering: Vec<String> = tests_run(row)
            .filter_map(function)
            .filter(|name| known.contains(name))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        if covering.is_empty() {
            unattributed(summary, id, "no usable covering test");
            continue;
        }
        let name = format!("mutant-{position}");
        let (_, measured) = with_mutant(&session.worktree, row, || {
            session.measure(run, &covering, &name)
        })?;
        if matches!(measured, Measured::TimedOut) {
            eprintln!(
                "attribution: mutant {}/{total} ({id}) re-run timed out after {}s",
                position + 1,
                session.timeout
            );
        }
        match (unusable_reason(&measured), measured) {
            (None, Measured::Report(report)) => {
                matrix.insert(id.to_string(), report);
            }
            (reason, _) => unattributed(summary, id, reason.unwrap_or("no report")),
        }
    }
    Ok(matrix)
}

/// Baselines, matrix and analysis; `summary` keeps whatever got measured.
fn measure_kills(
    run: &mut EngineRun,
    environment: &BTreeMap<String, String>,
    ranked: &[String],
    killed: &[Value],
    summary: &mut Map<String, Value>,
) -> Step<()> {
    let reports = run.worktree.join(".attribution");
    fs::create_dir_all(&reports)
        .map_err(|error| format!("cannot create {}: {error}", reports.display()))?;
    let mut session = Session {
        argv: pinned(&run.campaign.test_argv, run.require_python()?),
        environment,
        reports,
        timeout: run.campaign.run_timeout_seconds,
        worktree: run.worktree.to_path_buf(),
    };
    let baselines = session.baselines(run, ranked)?;
    let matrix = matrix(&session, run, killed, ranked, summary)?;
    summary.insert("attributed_mutants".into(), json!(matrix.len()));
    if matrix.is_empty() {
        summary.insert("status".into(), json!("NO_ATTRIBUTION"));
        return Ok(());
    }
    let paths: Vec<String> = killed
        .iter()
        .filter(|row| row["id"].as_str().is_some_and(|id| matrix.contains_key(id)))
        .filter_map(|row| row["path"].as_str().map(str::to_string))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let mut ids: Vec<String> = matrix.keys().cloned().collect();
    ids.sort();
    let outcomes: Map<String, Value> = matrix
        .keys()
        .map(|id| (id.clone(), json!(KILLED)))
        .collect();
    let analysed = analyze_reports(
        spec(run.campaign, ranked, &paths, &ids),
        baselines,
        &Value::Object(matrix),
        &Value::Object(outcomes),
    )?;
    let value: Value =
        serde_json::from_str(&analysed).map_err(|error| format!("test value: {error}"))?;
    run.receipt.insert("test_value".into(), value);
    summary.insert("status".into(), json!("ATTRIBUTED"));
    Ok(())
}

/// Measure the kill matrix and write `test_value` into the receipt.
///
/// Must run while the snapshot exists: the mutants are re-applied inside it.
pub fn attribute(run: &mut EngineRun, environment: &BTreeMap<String, String>) -> Step<()> {
    let every = rows(run.receipt).to_vec();
    let (ranked, unusable) = ranked(&every);
    let killed: Vec<Value> = every
        .into_iter()
        .filter(|row| row["outcome"].as_str() == Some(KILLED))
        .collect();
    let mut summary = Map::new();
    summary.insert("schema_version".into(), json!(ATTRIBUTION_SCHEMA));
    summary.insert("baseline_repetitions".into(), json!(BASELINE_REPETITIONS));
    summary.insert("ranked_tests".into(), json!(ranked.len()));
    summary.insert("unusable_nodeids".into(), json!(unusable));
    summary.insert("killed_mutants".into(), json!(killed.len()));
    summary.insert("attributed_mutants".into(), json!(0));
    summary.insert("unattributed".into(), json!([]));
    summary.insert("status".into(), json!("NOT_ATTEMPTED"));
    let outcome = if ranked.is_empty() || killed.is_empty() {
        let reason = if ranked.is_empty() {
            "the coverage map names no usable pytest nodeid"
        } else {
            "no mutant was killed"
        };
        summary.insert("reason".into(), json!(reason));
        Ok(())
    } else {
        measure_kills(run, environment, &ranked, &killed, &mut summary)
    };
    run.receipt
        .insert("attribution".into(), Value::Object(summary));
    outcome
}

#[cfg(test)]
#[path = "mutation_attribution_tests.rs"]
mod tests;
