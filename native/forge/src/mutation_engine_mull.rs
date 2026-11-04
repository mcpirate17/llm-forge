//! Mull, the C/C++ engine. A clang pass plugin builds the mutants into the test
//! binary; `mull-runner` then activates them one at a time and writes an
//! Elements report per executable. A suite with several executables reports the
//! same mutant more than once, so the reports merge on the best verdict.

use crate::mutation_campaign::{
    identify, py_repr, require_no_drift, Campaign, Step, ERROR, KILLED, NO_COVERAGE, SURVIVED,
    TIMED_OUT, UNVIABLE,
};
use crate::mutation_process::CommandResult;
use crate::mutation_run::{last_chars, EngineRun};
use crate::mutation_run_scope::mull_scope_config;
use crate::mutation_score::BASELINE_FAILED;
use crate::mutation_snapshot::{is_executable, on_path};
use conductor_native::project_paths::absolute_resolved;
use regex::Regex;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

const FLAGS: [&str; 4] = [
    "-g",
    "-grecord-command-line",
    "-fprofile-instr-generate",
    "-fcoverage-mapping",
];
const RANK: [&str; 6] = [
    "Killed",
    "Timeout",
    "Survived",
    "CompileError",
    "Ignored",
    "NoCoverage",
];

fn outcome(status: &str) -> Option<&'static str> {
    Some(match status {
        "Killed" => KILLED,
        "Survived" => SURVIVED,
        "NoCoverage" => NO_COVERAGE,
        "Timeout" => TIMED_OUT,
        "CompileError" | "Ignored" => UNVIABLE,
        "RuntimeError" => ERROR,
        _ => return None,
    })
}

fn llvm_version(campaign: &Campaign) -> Result<String, String> {
    campaign.option_text("llvm_version").ok_or_else(|| {
        "generator.options.llvm_version must name the LLVM release the Mull pass \
         plugin was built against (e.g. 18)"
            .to_string()
    })
}

fn tool(name: &str, version: &str, hint: &str) -> Result<String, String> {
    on_path(&format!("{name}-{version}"))
        .map(|found| found.to_string_lossy().into_owned())
        .ok_or_else(|| format!("{name}-{version} is not installed; {hint}"))
}

/// The newest `mull-runner-<llvm>` on PATH.
pub fn binary() -> Result<String, String> {
    let mut found: Vec<(u64, String)> = Vec::new();
    let path = env::var("PATH").unwrap_or_default();
    for directory in path.split(':').filter(|directory| !directory.is_empty()) {
        let Ok(entries) = fs::read_dir(directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let version = name
                .strip_prefix("mull-runner-")
                .filter(|digits| !digits.is_empty())
                .and_then(|digits| digits.parse::<u64>().ok());
            if let Some(version) = version.filter(|_| is_executable(&entry.path())) {
                found.push((version, entry.path().to_string_lossy().into_owned()));
            }
        }
    }
    found.sort();
    found.pop().map(|(_, path)| path).ok_or_else(|| {
        "no mull-runner-<llvm> is installed; install the Mull release whose asset name \
         carries the same LLVM version as the clang it will build with (it is a \
         developer tool, not a shipped dependency, so it belongs in the toolchain \
         rather than in pyproject)"
            .to_string()
    })
}

fn plugin(version: &str) -> String {
    format!("/usr/lib/mull-ir-frontend-{version}")
}

fn require_plugin(version: &str) -> Result<(), String> {
    let path = plugin(version);
    if Path::new(&path).is_file() {
        return Ok(());
    }
    Err(format!(
        "the Mull pass plugin is not at {path}; without it the build carries no \
         mutants and every campaign would report an empty corpus"
    ))
}

fn scalar_text(value: &Value) -> String {
    value
        .as_str()
        .map_or_else(|| value.to_string(), str::to_string)
}

pub(crate) fn executables(campaign: &Campaign) -> Result<Vec<String>, String> {
    let named = campaign
        .options
        .get("executables")
        .and_then(Value::as_array)
        .filter(|named| !named.is_empty());
    named
        .map(|named| named.iter().map(scalar_text).collect())
        .ok_or_else(|| {
            "generator.options.executables must be a non-empty list of test binaries, \
             relative to the build directory"
                .to_string()
        })
}

pub(crate) fn configure_argv(
    campaign: &Campaign,
    worktree: &Path,
    build: &Path,
    version: &str,
) -> Result<Vec<String>, String> {
    let source = campaign.option_text("cmake_source_dir").ok_or_else(|| {
        "generator.options.cmake_source_dir must name the directory holding the \
         CMakeLists.txt for the suite under test"
            .to_string()
    })?;
    let flags = format!("-fpass-plugin={} {}", plugin(version), FLAGS.join(" "));
    let mut argv = vec![
        "cmake".to_string(),
        "-S".to_string(),
        worktree.join(source).to_string_lossy().into_owned(),
        "-B".to_string(),
        build.to_string_lossy().into_owned(),
        "-G".to_string(),
        "Ninja".to_string(),
        format!("-DCMAKE_C_COMPILER=clang-{version}"),
        format!("-DCMAKE_CXX_COMPILER=clang++-{version}"),
        format!("-DCMAKE_C_FLAGS={flags}"),
        format!("-DCMAKE_CXX_FLAGS={flags}"),
        "-DCMAKE_EXE_LINKER_FLAGS=-fprofile-instr-generate".to_string(),
    ];
    if let Some(defines) = campaign.options.get("cmake_args").and_then(Value::as_array) {
        argv.extend(defines.iter().map(scalar_text));
    }
    Ok(argv)
}

/// One executable's `mull-runner` command line.
struct RunnerCall<'a> {
    binary: &'a str,
    executable: &'a Path,
    profdata: &'a Path,
    report_dir: &'a Path,
    report_name: &'a str,
}

fn engine_argv(campaign: &Campaign, call: &RunnerCall) -> Result<Vec<String>, String> {
    let milliseconds = (campaign.mutant_timeout()? * 1000).to_string();
    let mut argv = vec![
        call.binary.to_string(),
        call.executable.to_string_lossy().into_owned(),
        "--coverage-info".to_string(),
        call.profdata.to_string_lossy().into_owned(),
        "--workers".to_string(),
        campaign.jobs.to_string(),
        "--timeout".to_string(),
        milliseconds.clone(),
        "--minimum-timeout".to_string(),
        milliseconds,
        "--reporters".to_string(),
        "Elements".to_string(),
        "--report-dir".to_string(),
        call.report_dir.to_string_lossy().into_owned(),
        "--report-name".to_string(),
        call.report_name.to_string(),
        // Survivors are the survivor set's to judge, and in a multi-executable
        // suite one binary missing a mutant another kills is not a failure. With
        // this flag a non-zero exit means the runner itself failed.
        "--allow-surviving".to_string(),
    ];
    for mutator in &campaign.exclude {
        argv.extend(["--ignore-mutators".to_string(), mutator.clone()]);
    }
    Ok(argv)
}

/// `str.splitlines`: the boundaries Python counts, form feed included.
fn split_lines(text: &str) -> Vec<Vec<char>> {
    let boundary = |ch: char| {
        matches!(
            ch,
            '\n' | '\r'
                | '\u{b}'
                | '\u{c}'
                | '\u{1c}'
                | '\u{1d}'
                | '\u{1e}'
                | '\u{85}'
                | '\u{2028}'
                | '\u{2029}'
        )
    };
    let mut lines = Vec::new();
    let mut line = Vec::new();
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if !boundary(ch) {
            line.push(ch);
            continue;
        }
        if ch == '\r' && chars.peek() == Some(&'\n') {
            chars.next();
        }
        lines.push(std::mem::take(&mut line));
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

fn clipped(line: &[char], from: usize, to: usize) -> String {
    let to = to.min(line.len());
    let from = from.min(to);
    line[from..to].iter().collect()
}

fn coordinate(location: &Value, end: &str, key: &str) -> Result<usize, String> {
    location[end][key]
        .as_u64()
        .map(|value| value as usize)
        .ok_or_else(|| format!("Mull reported a mutant with no {end}.{key}"))
}

/// The source text a mutant's location covers; columns are one-based.
pub(crate) fn slice(source: &str, location: &Value) -> Result<String, String> {
    let lines = split_lines(source);
    let first = coordinate(location, "start", "line")?;
    let last = coordinate(location, "end", "line")?;
    let start = coordinate(location, "start", "column")?.saturating_sub(1);
    let stop = coordinate(location, "end", "column")?.saturating_sub(1);
    if first < 1 || first > lines.len() {
        return Ok(String::new());
    }
    let head = &lines[first - 1];
    if first == last {
        return Ok(clipped(head, start, stop));
    }
    let mut body = vec![clipped(head, start, head.len())];
    let middle_end = last.saturating_sub(1).min(lines.len());
    if first < middle_end {
        body.extend(
            lines[first..middle_end]
                .iter()
                .map(|line| line.iter().collect()),
        );
    }
    if last <= lines.len() {
        body.push(clipped(&lines[last - 1], 0, stop));
    }
    Ok(body.join("\n"))
}

fn matcher(pattern: &str) -> Result<Regex, String> {
    let mut expression = String::from("^");
    let mut rest = pattern;
    while !rest.is_empty() {
        if let Some(tail) = rest.strip_prefix("**") {
            expression.push_str(".*");
            rest = tail;
        } else if let Some(tail) = rest.strip_prefix('*') {
            expression.push_str("[^/]*");
            rest = tail;
        } else if let Some(tail) = rest.strip_prefix('?') {
            expression.push_str("[^/]");
            rest = tail;
        } else {
            let literal = rest.find(['*', '?']).unwrap_or(rest.len());
            expression.push_str(&regex::escape(&rest[..literal]));
            rest = &rest[literal..];
        }
    }
    expression.push('$');
    Regex::new(&expression).map_err(|error| format!("bad source glob {pattern}: {error}"))
}

pub(crate) fn in_scope(relative: &str, patterns: &[String]) -> Result<bool, String> {
    for pattern in patterns {
        if matcher(pattern)?.is_match(relative) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn better(candidate: &str, incumbent: &str) -> bool {
    let rank = |status: &str| {
        RANK.iter()
            .position(|known| *known == status)
            .unwrap_or(RANK.len())
    };
    rank(candidate) < rank(incumbent)
}

fn status_of(mutant: &Value) -> &str {
    mutant["status"].as_str().unwrap_or_default()
}

/// One file's mutants across every report, first-seen order, best verdict.
#[derive(Default)]
struct MergedFile {
    source: String,
    positions: BTreeMap<String, usize>,
    mutants: Vec<Value>,
}

impl MergedFile {
    fn absorb(&mut self, mutant: &Value) {
        let key = scalar_text(&mutant["id"]);
        match self.positions.get(&key) {
            None => {
                self.positions.insert(key, self.mutants.len());
                self.mutants.push(mutant.clone());
            }
            Some(&position) => {
                if better(status_of(mutant), status_of(&self.mutants[position])) {
                    self.mutants[position] = mutant.clone();
                }
            }
        }
    }
}

/// Merge per-executable reports: a mutant any executable killed is killed.
pub(crate) fn merge(reports: &[Value]) -> BTreeMap<String, (String, Vec<Value>)> {
    let mut merged: BTreeMap<String, MergedFile> = BTreeMap::new();
    for report in reports {
        let Some(files) = report.get("files").and_then(Value::as_object) else {
            continue;
        };
        for (path, entry) in files {
            let into = merged.entry(path.clone()).or_insert_with(|| MergedFile {
                source: entry["source"].as_str().unwrap_or_default().to_string(),
                ..MergedFile::default()
            });
            for mutant in entry["mutants"].as_array().into_iter().flatten() {
                into.absorb(mutant);
            }
        }
    }
    merged
        .into_iter()
        .map(|(path, file)| (path, (file.source, file.mutants)))
        .collect()
}

type Found<'a> = (String, (usize, usize, String), &'a str, &'a Value);

fn located<'a>(
    merged: &'a BTreeMap<String, (String, Vec<Value>)>,
    worktree: &Path,
) -> Result<Vec<Found<'a>>, String> {
    let root = absolute_resolved(worktree)?;
    let mut found = Vec::new();
    for (path, (source, mutants)) in merged {
        let resolved = absolute_resolved(Path::new(path))?;
        let relative = resolved
            .strip_prefix(&root)
            .map_err(|_| format!("Mull reported a mutant outside the worktree: {path}"))?
            .to_string_lossy()
            .into_owned();
        for mutant in mutants {
            let location = &mutant["location"];
            let key = (
                coordinate(location, "start", "line")?,
                coordinate(location, "start", "column")?,
                scalar_text(&mutant["mutatorName"]),
            );
            found.push((relative.clone(), key, source.as_str(), mutant));
        }
    }
    found.sort_by(|left, right| (&left.0, &left.1).cmp(&(&right.0, &right.1)));
    Ok(found)
}

/// Name and record every merged mutant, deterministically.
pub(crate) fn rows(
    merged: &BTreeMap<String, (String, Vec<Value>)>,
    worktree: &Path,
) -> Result<Vec<Value>, String> {
    let found = located(merged, worktree)?;
    let mut entries = Vec::with_capacity(found.len());
    for (relative, (_, _, mutator), source, mutant) in &found {
        entries.push([
            relative.clone(),
            mutator.clone(),
            slice(source, &mutant["location"])?,
            scalar_text(&mutant["replacement"]),
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
    let mut rows = Vec::with_capacity(found.len());
    for (((_, (line, _, _), _, mutant), entry), name) in found.iter().zip(&entries).zip(&names) {
        let status = status_of(mutant);
        let outcome = outcome(status)
            .ok_or_else(|| format!("Mull reported unknown status {}", py_repr(status)))?;
        rows.push(json!({
            "id": name,
            "outcome": outcome,
            "path": entry[0],
            "line": line,
            "operator": entry[1],
            "original_text": entry[2],
            "mutated_text": entry[3],
        }));
    }
    Ok(rows)
}

fn receipt_list(run: &mut EngineRun, key: &str, item: Value) {
    let entry = run.receipt.entry(key).or_insert_with(|| json!([]));
    if let Some(list) = entry.as_array_mut() {
        list.push(item);
    }
}

/// Run one executable clean for its coverage profile, then merge the profile.
fn profile(
    run: &mut EngineRun,
    executable: &Path,
    build: &Path,
    name: &str,
    profdata_tool: &str,
    bound: &BTreeMap<String, String>,
) -> Step<PathBuf> {
    let timeout = run.campaign.run_timeout_seconds;
    let raw = build.join(format!("{name}.profraw"));
    if raw.exists() {
        fs::remove_file(&raw)
            .map_err(|error| format!("cannot clear {}: {error}", raw.display()))?;
    }
    let mut environment = bound.clone();
    environment.insert(
        "LLVM_PROFILE_FILE".into(),
        raw.to_string_lossy().into_owned(),
    );
    let argv = [executable.to_string_lossy().into_owned()];
    let (result, _) = run.run(&argv, build, timeout, &environment, &[])?;
    let baselines = run.receipt.entry("baselines").or_insert_with(|| json!({}));
    baselines[name] = result.as_value();
    if !result.succeeded() {
        return Err(format!(
            "unmutated baseline {name} failed (rc={}); every mutant after an already-red \
             suite reads as caught",
            result.rc_text()
        )
        .into());
    }
    if !raw.is_file() {
        return Err(format!(
            "{name} produced no profile at {}: without coverage data Mull reports \
             unreached code as survived",
            raw.display()
        )
        .into());
    }
    let profdata = build.join(format!("{name}.profdata"));
    let merge_argv = [
        profdata_tool.to_string(),
        "merge".to_string(),
        "-sparse".to_string(),
        raw.to_string_lossy().into_owned(),
        "-o".to_string(),
        profdata.to_string_lossy().into_owned(),
    ];
    let (merged, _) = run.run(&merge_argv, build, timeout, bound, &[])?;
    if merged.returncode != Some(0) || !profdata.is_file() {
        return Err(format!(
            "llvm-profdata could not merge {}: {}",
            raw.display(),
            merged.stderr_tail
        )
        .into());
    }
    Ok(profdata)
}

/// Configure and build the instrumented suite; a failure is a failed baseline.
fn build(
    run: &mut EngineRun,
    build: &Path,
    version: &str,
    bound: &BTreeMap<String, String>,
) -> Step<()> {
    require_plugin(version)?;
    let worktree = run.worktree.to_path_buf();
    let timeout = run.campaign.run_timeout_seconds;
    let steps = [
        configure_argv(run.campaign, &worktree, build, version)?,
        vec![
            "cmake".to_string(),
            "--build".to_string(),
            build.to_string_lossy().into_owned(),
        ],
    ];
    for argv in &steps {
        let (result, _) = run.run(argv, &worktree, timeout, bound, &[])?;
        receipt_list(run, "build", result.as_value());
        if !result.succeeded() {
            run.receipt.insert("status".into(), json!(BASELINE_FAILED));
            run.write_receipt()?;
            return Err(format!(
                "the instrumented build failed: {}",
                last_chars(&result.stderr_tail, 800)
            )
            .into());
        }
    }
    Ok(())
}

fn report_at(path: &Path, result: &CommandResult) -> Result<Value, String> {
    if !path.is_file() {
        return Err(format!(
            "mull-runner wrote no Elements report at {} (rc={}): {}",
            path.display(),
            result.rc_text(),
            last_chars(&result.stderr_tail, 800)
        ));
    }
    let text = fs::read_to_string(path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    serde_json::from_str(&text)
        .map_err(|error| format!("Mull's report at {} is not JSON: {error}", path.display()))
}

/// The worst per-executable result, in the single shape the validator reads.
fn engine_result(results: &[CommandResult]) -> Value {
    let failed = results.iter().find(|result| !result.succeeded());
    let returncode = failed.map_or(Some(0), |result| result.returncode);
    json!({
        "returncode": returncode,
        "timed_out": results.iter().any(|result| result.timed_out),
        "executables": results.len(),
    })
}

fn base_name(name: &str) -> String {
    Path::new(name)
        .file_name()
        .map(|base| base.to_string_lossy().into_owned())
        .unwrap_or_else(|| name.to_string())
}

/// Under --allow-surviving only a runner failure exits non-zero, and what a failed
/// runner left behind is not a measurement.
fn require_runner_success(result: &CommandResult, name: &str) -> Result<(), String> {
    if result.succeeded() {
        return Ok(());
    }
    Err(format!(
        "mull-runner failed on {name} (rc={}): {}",
        result.rc_text(),
        last_chars(&result.stderr_tail, 800)
    ))
}

/// Profile and mutate each executable; the reports come back in manifest order.
fn engine_reports(
    run: &mut EngineRun,
    binary: &str,
    build: &Path,
    profdata_tool: &str,
    bound: &BTreeMap<String, String>,
) -> Step<Vec<Value>> {
    let timeout = run.campaign.run_timeout_seconds;
    let report_dir = build.join("mull-reports");
    let mut reports = Vec::new();
    let mut results = Vec::new();
    for name in executables(run.campaign)? {
        let executable = build.join(&name);
        if !executable.is_file() {
            return Err(format!(
                "the build produced no executable at {}",
                executable.display()
            )
            .into());
        }
        let report_name = base_name(&name);
        let profdata = profile(run, &executable, build, &report_name, profdata_tool, bound)?;
        let call = RunnerCall {
            binary,
            executable: &executable,
            profdata: &profdata,
            report_dir: &report_dir,
            report_name: &report_name,
        };
        let argv = engine_argv(run.campaign, &call)?;
        let (result, _) = run.run(&argv, build, timeout, bound, &[])?;
        receipt_list(run, "engine_results", result.as_value());
        results.push(result.clone());
        run.receipt
            .insert("engine_result".into(), engine_result(&results));
        if result.timed_out {
            return Err(format!("mull-runner timed out on {name} after {timeout}s").into());
        }
        let path = report_dir.join(format!("{report_name}.json"));
        let report = report_at(&path, &result)?;
        require_runner_success(&result, &name)?;
        reports.push(report);
    }
    Ok(reports)
}

/// The toolchain the manifest pins; a runner for another LLVM release would read
/// the plugin's output as an unmutated binary.
fn pinned_tools(campaign: &Campaign, binary: &str) -> Result<(String, String), String> {
    let version = llvm_version(campaign)?;
    let wanted = tool(
        "mull-runner",
        &version,
        &format!("the manifest pins llvm_version={version}"),
    )?;
    if absolute_resolved(Path::new(binary))? != absolute_resolved(Path::new(&wanted))? {
        return Err(format!(
            "the resolved runner {binary} is not the manifest's {wanted}: a runner built \
             for another LLVM release reads the pass plugin's output as an unmutated binary"
        ));
    }
    let hint = format!("install llvm-{version} beside clang-{version}");
    let profdata_tool = tool("llvm-profdata", &version, &hint)?;
    Ok((version, profdata_tool))
}

fn scoped_rows(campaign: &Campaign, every: &[Value]) -> Result<Vec<Value>, String> {
    let mut scoped = Vec::new();
    for row in every {
        if in_scope(row["path"].as_str().unwrap_or_default(), &campaign.source)? {
            scoped.push(row.clone());
        }
    }
    Ok(scoped)
}

fn summary(every: &[Value], scoped: &[Value]) -> Value {
    let mut files: Vec<&str> = every
        .iter()
        .filter_map(|row| row["path"].as_str())
        .collect();
    files.sort_unstable();
    files.dedup();
    let errors = scoped
        .iter()
        .filter(|row| row["outcome"].as_str() == Some(ERROR))
        .count();
    json!({
        "mutants_reported": every.len(),
        "mutants_in_scope": scoped.len(),
        "mutants_generated": scoped.len(),
        "errors": errors,
        "files_mutated": files,
    })
}

/// Build the instrumented corpus, cover it, mutate it, fill the receipt.
pub fn execute(run: &mut EngineRun, binary: &str) -> Step<()> {
    require_no_drift(run.campaign, run.worktree)?;
    let (version, profdata_tool) = pinned_tools(run.campaign, binary)?;
    let worktree = run.worktree.to_path_buf();
    let mut bound = run.campaign.environment.clone();
    bound.extend([
        run.snapshot_python_entry(),
        ("PATH".to_string(), env::var("PATH").unwrap_or_default()),
        (
            "MULL_CONFIG".to_string(),
            mull_scope_config(run.campaign, &worktree)?
                .to_string_lossy()
                .into_owned(),
        ),
    ]);
    let directory = run
        .campaign
        .option_text("build_dir")
        .unwrap_or_else(|| ".mull-build".to_string());
    let build_dir = worktree.join(directory);
    build(run, &build_dir, &version, &bound)?;
    // The declared suite gate, before any mutation work is paid for: every mutant
    // after an already-red suite would read as caught.
    let baseline_argv = run.campaign.test_argv.clone();
    let timeout = run.campaign.run_timeout_seconds;
    let (baseline, _) = run.run(&baseline_argv, &build_dir, timeout, &bound, &[])?;
    run.note_baseline(&baseline, &baseline_argv)?;
    let reports = engine_reports(run, binary, &build_dir, &profdata_tool, &bound)?;
    let version = reports[0]["config"]["mullVersion"].clone();
    run.receipt.insert("engine_version".into(), version);
    let every = rows(&merge(&reports), &worktree)?;
    let scoped = scoped_rows(run.campaign, &every)?;
    if scoped.is_empty() {
        return Err(format!(
            "none of the {} mutants Mull produced fall under generator.source {}; the \
             campaign would score an empty corpus",
            every.len(),
            crate::mutation_campaign::py_list(&run.campaign.source)
        )
        .into());
    }
    run.planned = Some(scoped.len() as u64);
    run.receipt
        .insert("engine_summary".into(), summary(&every, &scoped));
    run.receipt.insert("mutants".into(), Value::Array(scoped));
    run.require_scored_rows()
}

#[cfg(test)]
#[path = "mutation_engine_mull_tests.rs"]
mod tests;
