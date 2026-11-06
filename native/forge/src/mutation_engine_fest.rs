//! fest, the Python engine: collect a per-test coverage map from the campaign's
//! own command, let fest generate and run the mutants, then attribute the kills.
//!
//! fest builds each mutant's pytest command itself, so nothing of ours sits
//! between a rewrite and the child that imports it. Every such child is pytest,
//! though, so `PYTEST_ADDOPTS` loads the `conductor.mutation_pycache_evict`
//! plugin before collection. That plugin runs inside the campaign's interpreter
//! and stays Python, which is why `conductor` must be importable there.

use crate::mutation_attribution::attribute;
use crate::mutation_bytecode::scratch_root_for;
use crate::mutation_campaign::{
    identify, pinned, py_repr, require_no_drift, Campaign, Step, ERROR, KILLED, NO_COVERAGE,
    SURVIVED, TIMED_OUT,
};
use crate::mutation_process::{rounded, CommandResult};
use crate::mutation_run::{last_chars, EngineRun};
use crate::mutation_snapshot::{is_executable, on_path};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

const PLUGIN_NAME: &str = "conductor.mutation_pycache_evict";
const SCRATCH_ENV: &str = "CONDUCTOR_PYCACHE_EVICT_SCRATCH";
const SOURCES_ENV: &str = "CONDUCTOR_PYCACHE_EVICT_SOURCES";
const SUMMARY_KEYS: [&str; 8] = [
    "files_scanned",
    "mutants_generated",
    "mutants_tested",
    "no_coverage",
    "killed",
    "survived",
    "timeouts",
    "errors",
];

fn outcome(status: &str) -> Option<&'static str> {
    Some(match status {
        "Killed" => KILLED,
        "Survived" => SURVIVED,
        "Timeout" => TIMED_OUT,
        "NoCoverage" => NO_COVERAGE,
        "Error" => ERROR,
        _ => return None,
    })
}

/// The fest executable beside the campaign's interpreter, or on PATH.
pub fn binary(interpreter: &str) -> Result<String, String> {
    let beside = Path::new(interpreter).with_file_name("fest");
    if beside.is_file() && is_executable(&beside) {
        return Ok(beside.to_string_lossy().into_owned());
    }
    on_path("fest")
        .map(|found| found.to_string_lossy().into_owned())
        .ok_or_else(|| {
            "fest is not installed; `uv sync` should provide fest-mutate \
             (declared in pyproject, never only in a CI job)"
                .to_string()
        })
}

fn toml_list(items: &[String]) -> Result<String, String> {
    serde_json::to_string(items).map_err(|error| error.to_string())
}

/// The fest.toml written into the snapshot, pinned to this interpreter.
fn config(campaign: &Campaign, interpreter: &str) -> Result<String, String> {
    let lines = [
        "[fest]".to_string(),
        format!("source = {}", toml_list(&campaign.source)?),
        format!("exclude = {}", toml_list(&campaign.exclude)?),
        format!("timeout = {}", campaign.mutant_timeout()?),
        format!("seed = {}", campaign.seed),
        format!("workers = {}", campaign.jobs),
        format!(
            "test_command = {}",
            toml_list(&pinned(&campaign.test_argv, interpreter))?
        ),
        "output = \"json\"".to_string(),
        "backend = \"subprocess\"".to_string(),
        String::new(),
    ];
    Ok(lines.join("\n"))
}

fn mutant_field<'a>(result: &'a Value, key: &str) -> Result<&'a Value, String> {
    result
        .get("mutant")
        .and_then(|mutant| mutant.get(key))
        .ok_or_else(|| format!("fest reported a mutant with no {key}"))
}

fn mutant_text(result: &Value, key: &str) -> Result<String, String> {
    let value = mutant_field(result, key)?;
    Ok(value
        .as_str()
        .map_or_else(|| value.to_string(), str::to_string))
}

fn seconds(duration: Option<&Value>) -> f64 {
    let part = |key: &str| {
        duration
            .and_then(|value| value.get(key))
            .and_then(Value::as_f64)
            .unwrap_or(0.0)
    };
    part("secs") + part("nanos") / 1e9
}

fn row(path: &str, result: &Value, identifier: &str) -> Result<Value, String> {
    let status = result
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let outcome = outcome(status)
        .ok_or_else(|| format!("fest reported unknown status {}", py_repr(status)))?;
    let tests_run = result
        .get("tests_run")
        .filter(|value| !value.is_null())
        .cloned()
        .unwrap_or_else(|| json!([]));
    Ok(json!({
        "id": identifier,
        "outcome": outcome,
        "path": path,
        "line": mutant_field(result, "line")?,
        "byte_offset": mutant_field(result, "byte_offset")?,
        "byte_length": mutant_field(result, "byte_length")?,
        "operator": mutant_field(result, "mutator_name")?,
        "original_text": mutant_field(result, "original_text")?,
        "mutated_text": mutant_field(result, "mutated_text")?,
        "tests_run": tests_run,
        "duration_seconds": rounded(seconds(result.get("duration"))),
    }))
}

fn relative(absolute: &Path, worktree: &Path) -> String {
    match absolute.strip_prefix(worktree) {
        Ok(path) => path.to_string_lossy().into_owned(),
        Err(_) => absolute
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default(),
    }
}

fn sort_key(result: &Value) -> Result<(String, u64), String> {
    let offset = mutant_field(result, "byte_offset")?
        .as_u64()
        .ok_or_else(|| "fest reported a non-integer byte_offset".to_string())?;
    Ok((mutant_text(result, "file_path")?, offset))
}

/// Name every generated mutant, in file order, deterministically.
pub(crate) fn rows(report: &Value, worktree: &Path) -> Result<Vec<Value>, String> {
    rows_inner(report, worktree, None)
}

fn confined_path(
    file: &str,
    worktree: &Path,
    selected: &std::collections::BTreeSet<String>,
) -> Result<String, String> {
    let root = worktree
        .canonicalize()
        .map_err(|e| format!("cannot resolve Fest snapshot: {e}"))?;
    if root != worktree {
        return Err("Fest snapshot path is aliased".into());
    }
    let path = Path::new(file);
    let relative = path
        .strip_prefix(&root)
        .map_err(|_| "Fest reported a source outside its snapshot")?;
    let relative = relative.to_str().ok_or("Fest source path is not UTF-8")?;
    conductor_native::mutation_snapshot_inputs::validate_path(relative)?;
    if !selected.contains(relative)
        || root.join(relative).as_os_str() != path.as_os_str()
        || path
            .canonicalize()
            .map_err(|e| format!("cannot resolve Fest source: {e}"))?
            .as_os_str()
            != path.as_os_str()
    {
        return Err("Fest reported an aliased or unselected source".into());
    }
    Ok(relative.to_owned())
}

pub(crate) fn rows_confined(
    report: &Value,
    worktree: &Path,
    selected: &std::collections::BTreeSet<String>,
) -> Result<Vec<Value>, String> {
    rows_inner(report, worktree, Some(selected))
}

fn rows_inner(
    report: &Value,
    worktree: &Path,
    selected: Option<&std::collections::BTreeSet<String>>,
) -> Result<Vec<Value>, String> {
    let mut results = Vec::new();
    for result in report
        .get("results")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        results.push((sort_key(result)?, result));
    }
    results.sort_by(|left, right| left.0.cmp(&right.0));
    let mut entries = Vec::with_capacity(results.len());
    for ((file, _), result) in &results {
        entries.push([
            match selected {
                Some(selected) => confined_path(file, worktree, selected)?,
                None => relative(Path::new(file), worktree),
            },
            mutant_text(result, "mutator_name")?,
            mutant_text(result, "original_text")?,
            mutant_text(result, "mutated_text")?,
        ]);
    }
    let borrowed: Vec<[&str; 4]> = entries
        .iter()
        .map(|[path, operator, original, mutated]| {
            [
                path.as_str(),
                operator.as_str(),
                original.as_str(),
                mutated.as_str(),
            ]
        })
        .collect();
    let names = identify(&borrowed);
    results
        .iter()
        .zip(entries.iter().zip(&names))
        .map(|((_, result), (entry, name))| row(&entry[0], result, name))
        .collect()
}

/// The `--cov=` roots implied by the campaign's source globs.
pub(crate) fn coverage_targets(patterns: &[String]) -> Vec<String> {
    let mut targets: Vec<String> = Vec::new();
    for pattern in patterns {
        let head = pattern
            .split('*')
            .next()
            .unwrap_or("")
            .trim_end_matches('/');
        let root = if head.ends_with(".py") {
            head.rsplit_once('/').map_or("", |(parent, _)| parent)
        } else {
            head
        };
        let root = if root.is_empty() { "." } else { root };
        if !targets.iter().any(|target| target == root) {
            targets.push(root.to_string());
        }
    }
    targets
}

/// The campaign's own test command, instrumented for per-test coverage.
pub(crate) fn coverage_argv(campaign: &Campaign, interpreter: &str) -> Vec<String> {
    let mut argv = pinned(&campaign.test_argv, interpreter);
    argv.extend(
        coverage_targets(&campaign.source)
            .iter()
            .map(|target| format!("--cov={target}")),
    );
    argv.extend(["--cov-context=test", "--cov-fail-under=0", "--cov-report="].map(str::to_string));
    argv
}

/// The snapshot's own import roots, ahead of anything site-packages injects.
///
/// An editable install's `.pth` names the original checkout; under a src layout
/// `import x` would resolve to the unmutated tree and every mutant would read
/// as unreached. `PYTHONPATH` entries precede site-packages.
fn import_roots(worktree: &Path) -> Vec<String> {
    [worktree.to_path_buf(), worktree.join("src")]
        .iter()
        .filter(|path| path.is_dir())
        .map(|path| path.to_string_lossy().into_owned())
        .collect()
}

fn parent_text(path: &Path) -> String {
    path.parent()
        .map(|parent| parent.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// The environment fest runs under, bound to this interpreter and this snapshot.
///
/// fest probes for pytest-cov with a bare `python`, so `PATH` and `VIRTUAL_ENV`
/// follow the interpreter the receipt pins.
pub(crate) fn environment(
    campaign: &Campaign,
    worktree: &Path,
    interpreter: &str,
) -> BTreeMap<String, String> {
    let declared = |key: &str| campaign.environment.get(key).cloned().unwrap_or_default();
    let mut roots = import_roots(worktree);
    roots.extend(declared("PYTHONPATH").split(':').map(str::to_string));
    roots.retain(|root| !root.is_empty());
    let interpreter_path = Path::new(interpreter);
    let bin_dir = parent_text(interpreter_path);
    let virtual_env = interpreter_path
        .parent()
        .map(parent_text)
        .unwrap_or_default();
    let host_path = env::var("PATH").unwrap_or_default();
    let declared_addopts = declared("PYTEST_ADDOPTS");
    let addopts = [declared_addopts.trim(), &format!("-p {PLUGIN_NAME}")]
        .iter()
        .filter(|part| !part.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join(" ");
    let sources = campaign
        .source_sha256
        .keys()
        .map(|relative| worktree.join(relative).to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(":");
    let mut bound = campaign.environment.clone();
    bound.extend([
        ("PYTHONPATH".to_string(), roots.join(":")),
        ("VIRTUAL_ENV".to_string(), virtual_env),
        ("PATH".to_string(), format!("{bin_dir}:{host_path}")),
        ("PYTEST_ADDOPTS".to_string(), addopts),
        (
            SCRATCH_ENV.to_string(),
            scratch_root_for(worktree).to_string_lossy().into_owned(),
        ),
        (SOURCES_ENV.to_string(), sources),
    ]);
    bound
}

/// The fest invocation: JSON on stdout, and our coverage map, not its own.
pub(crate) fn engine_argv(campaign: &Campaign, binary: &str, coverage: &Path) -> Vec<String> {
    let mut argv: Vec<String> = [binary, "run", "--progress", "quiet", "--output", "json"]
        .map(str::to_string)
        .to_vec();
    for pattern in &campaign.operators {
        argv.extend(["--filter-operators".to_string(), pattern.clone()]);
    }
    argv.extend([
        "--coverage-from".to_string(),
        coverage.to_string_lossy().into_owned(),
    ]);
    argv
}

fn summary(report: &Value) -> Value {
    let picked: Map<String, Value> = SUMMARY_KEYS
        .iter()
        .filter_map(|key| {
            report
                .get(*key)
                .map(|value| ((*key).to_string(), value.clone()))
        })
        .collect();
    Value::Object(picked)
}

fn counted(report: &Value, key: &str) -> u64 {
    report.get(key).and_then(Value::as_u64).unwrap_or(0)
}

/// Refuse a report from a run that failed. fest exits 0 with survivors, and only
/// `--fail-under`, which this runner never passes, makes a score exit non-zero.
fn require_clean_exit(result: &CommandResult) -> Result<(), String> {
    if result.succeeded() {
        return Ok(());
    }
    Err(format!(
        "fest failed (rc={}): {}",
        result.rc_text(),
        last_chars(&result.stderr_tail, 800)
    ))
}

fn ensure_last_cov_report(argv: &mut Vec<String>) -> Result<(), String> {
    if argv.pop().as_deref() != Some("--cov-report=") {
        return Err("unexpected Fest baseline coverage command".into());
    }
    Ok(())
}

/// Collect coverage, generate and run the mutants, fill in the receipt.
fn confined_engine_arguments(argv: &mut Vec<String>, campaign: &Campaign) -> Step<()> {
    argv.extend(["--source".to_owned(), campaign.source[0].clone()]);
    for exclusion in &campaign.exclude {
        argv.extend(["--exclude".to_owned(), exclusion.clone()]);
    }
    argv.extend([
        "--workers".to_owned(),
        "1".to_owned(),
        "--timeout".to_owned(),
        campaign.mutant_timeout()?.to_string(),
        "--seed".to_owned(),
        campaign.seed.to_string(),
        "--backend".to_owned(),
        "subprocess".to_owned(),
        "--no-coverage-cache".to_owned(),
    ]);
    Ok(())
}

fn record_report(
    run: &mut EngineRun,
    result: &CommandResult,
    stdout: &str,
    worktree: &Path,
) -> Step<()> {
    let report: Value = serde_json::from_str(stdout).map_err(|_| {
        format!(
            "fest produced no JSON report (rc={}): {}",
            result.rc_text(),
            last_chars(&result.stderr_tail, 800)
        )
    })?;
    run.planned = report.get("mutants_generated").and_then(Value::as_u64);
    require_clean_exit(result)?;
    run.receipt
        .insert("engine_summary".into(), summary(&report));
    let mutants =
        if run.campaign.option_text("execution_profile").as_deref() == Some("fest-confined-v1") {
            let selected = run.campaign.source_sha256.keys().cloned().collect();
            rows_confined(&report, worktree, &selected)?
        } else {
            rows(&report, worktree)?
        };
    run.receipt.insert("mutants".into(), Value::Array(mutants));
    run.require_executed(
        counted(&report, "mutants_generated"),
        counted(&report, "mutants_tested"),
    )?;
    Ok(())
}

pub fn execute(run: &mut EngineRun, binary: &str) -> Step<()> {
    let interpreter = run.require_python()?;
    require_no_drift(run.campaign, run.worktree)?;
    let worktree = run.worktree.to_path_buf();
    let timeout = run.campaign.run_timeout_seconds;
    let confined =
        run.campaign.option_text("execution_profile").as_deref() == Some("fest-confined-v1");
    let runtime = run.runtime;
    let coverage = if confined {
        runtime
            .and_then(|owner| owner.fest_output("coverage.json"))
            .ok_or("confined Fest output owner missing")?
    } else {
        worktree.join(".coverage")
    };
    let attribution_environment = if confined {
        BTreeMap::new()
    } else {
        environment(run.campaign, &worktree, interpreter)
    };
    let mut bound = attribution_environment.clone();
    bound.insert(
        "COVERAGE_FILE".into(),
        coverage.to_string_lossy().into_owned(),
    );
    let mut baseline_argv = coverage_argv(run.campaign, interpreter);
    if confined {
        ensure_last_cov_report(&mut baseline_argv)?;
        baseline_argv.push(format!("--cov-report=json:{}", coverage.display()));
    }
    let (baseline, _) = run.run(&baseline_argv, &worktree, timeout, &bound, &[])?;
    run.note_baseline(&baseline, &baseline_argv)?;
    if !coverage.is_file() {
        return Err(format!(
            "baseline produced no coverage database at {}",
            coverage.display()
        )
        .into());
    }
    let coverage_bindings = if confined {
        let projected = runtime
            .and_then(|owner| owner.fest_output("coverage-curated.json"))
            .ok_or("confined Fest coverage projection owner missing")?;
        Some((
            projected.clone(),
            crate::mutation_fest_coverage::capture(&coverage, &projected, &worktree, run.campaign)
                .map_err(|e| format!("Fest coverage admission: {e:#}"))?,
        ))
    } else {
        let toml = config(run.campaign, interpreter)?;
        fs::write(worktree.join("fest.toml"), toml)
            .map_err(|error| format!("cannot write fest.toml: {error}"))?;
        None
    };
    let mutated: Vec<PathBuf> = run
        .campaign
        .source_sha256
        .keys()
        .map(|relative| worktree.join(relative))
        .collect();
    let effective_coverage = coverage_bindings
        .as_ref()
        .map_or(&coverage, |(path, _)| path);
    let mut argv = engine_argv(run.campaign, binary, effective_coverage);
    if confined {
        confined_engine_arguments(&mut argv, run.campaign)?;
    }
    let (result, stdout) = run.run(&argv, &worktree, timeout, &bound, &mutated)?;
    if let Some((_, (raw, projected))) = &coverage_bindings {
        raw.verify()
            .map_err(|e| format!("raw coverage changed: {e:#}"))?;
        projected
            .verify()
            .map_err(|e| format!("curated coverage changed: {e:#}"))?;
    }
    run.receipt
        .insert("engine_result".into(), result.as_value());
    if result.timed_out {
        return Err(format!("fest timed out after {timeout}s").into());
    }
    record_report(run, &result, &stdout, &worktree)?;
    attribute(run, &attribution_environment)
}

#[cfg(test)]
#[path = "mutation_engine_fest_tests.rs"]
mod tests;
