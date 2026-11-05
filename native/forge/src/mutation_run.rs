//! One generated campaign, end to end: refuse what is out of scope, open the
//! receipt, run the engine in a disposable snapshot, verify the pinned bytes and
//! the cleanup, then score.
//!
//! The receipt is on disk from the first moment, so a run killed outright leaves
//! a RUNNING receipt rather than nothing. Every other ending writes a final
//! status: only a run that finished, scored clean and verified is PASS.

use crate::instant::{format_compact_utc, isoformat_utc, now};
use crate::json_canon::python_pretty;
use crate::mutation_bytecode::{isolated_python_env, scratch_root_for};
use crate::mutation_campaign::{drift, sha256_path, Campaign, Step, Stop, KILLED, SURVIVED};
use crate::mutation_engine_cargo as cargo;
use crate::mutation_engine_fest as fest;
use crate::mutation_engine_mull as mull;
use crate::mutation_process::{live_pgids_path, memory_cap, CommandResult, Supervisor};
use crate::mutation_run_scope::validate_run_scope;
use crate::mutation_score::{
    account, record_baseline, rows, score, status, Receipt, BASELINE_FAILED, CANCELLED, ERROR,
    RUNNING,
};
use crate::mutation_snapshot::{configured_snapshot_python, snapshot_python, Snapshot};
use crate::mutation_write::atomic_write;
use crate::process_signals::Interrupts;
use conductor_native::mutation_native_runner::{RUNNER_KIND, RUNNER_SCHEMA};
use conductor_native::project_paths::{self, absolute_resolved};
use conductor_native::receipt_slim::slim_receipt;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// The receipt schema `conductor_native::mutation_receipt` validates.
const RECEIPT_SCHEMA: &str = "llm.mutation-testing.receipt.v3";
const INTERPRETER_FAILURES: [&str; 3] = [
    "python3: not found",
    "ModuleNotFoundError",
    "No module named",
];
const SNAPSHOT_PYTHON_ENV: &str = "CONDUCTOR_SNAPSHOT_PYTHON";

/// What the CLI knows about the binary that is running.
#[derive(Clone, Copy, Debug)]
pub struct BuildStamp {
    pub forge_version: &'static str,
    pub source_sha256: &'static str,
    pub git_commit: &'static str,
    pub source_matches_git: bool,
}

/// One `forge mutation run` invocation, owner already resolved.
pub struct RunRequest {
    pub manifest: PathBuf,
    pub repo_root: PathBuf,
    pub owner: String,
    pub base: String,
    pub only: Vec<String>,
    pub allow_mutations: bool,
    pub receipt: Option<PathBuf>,
    pub stamp: BuildStamp,
}

/// The last `count` characters of `text`.
pub fn last_chars(text: &str, count: usize) -> String {
    let skip = text.chars().count().saturating_sub(count);
    text.chars().skip(skip).collect()
}

/// What an engine adapter needs while it runs inside a snapshot.
pub struct EngineRun<'a> {
    pub campaign: &'a mut Campaign,
    pub receipt: &'a mut Receipt,
    pub output_path: &'a Path,
    pub repo_root: &'a Path,
    pub worktree: &'a Path,
    pub interpreter: Option<&'a str>,
    pub supervisor: &'a Supervisor<'a>,
    /// The engine's own mutant count, set as soon as a report states one.
    pub planned: Option<u64>,
}

impl<'a> EngineRun<'a> {
    /// One child, bound to the private bytecode cache of the directory it runs in.
    pub fn run(
        &self,
        argv: &[String],
        cwd: &Path,
        timeout_seconds: i64,
        environment: &BTreeMap<String, String>,
        mutated_paths: &[PathBuf],
    ) -> Step<(CommandResult, String)> {
        let bound = self.guest_environment(environment);
        let isolated = isolated_python_env(&bound, &scratch_root_for(cwd), mutated_paths)?;
        self.supervisor.run(argv, cwd, timeout_seconds, &isolated)
    }

    pub fn write_receipt(&self) -> Result<(), String> {
        write_receipt(self.output_path, self.receipt)
    }

    /// Record the baseline; a red one refuses the run, a green one sets the bound.
    pub fn note_baseline(&mut self, baseline: &CommandResult, argv: &[String]) -> Step<()> {
        self.receipt.insert("baseline_argv".into(), json!(argv));
        self.receipt.insert("baseline".into(), baseline.as_value());
        if !baseline.succeeded() {
            self.receipt.insert("status".into(), json!(BASELINE_FAILED));
            self.write_receipt()?;
            let detail = format!(
                "unmutated baseline failed; receipt={}",
                self.output_path.display()
            );
            return Err(Stop::Refused(match interpreter_hint(baseline) {
                Some(hint) => format!("{detail}; {hint}"),
                None => detail,
            }));
        }
        let bound = self
            .campaign
            .resolve_mutant_timeout(baseline.duration_seconds);
        self.receipt
            .insert("mutant_timeout_seconds".into(), json!(bound));
        Ok(())
    }

    /// Refuse a run that scored nothing, however green it looks.
    pub fn require_executed(&self, generated: u64, tested: u64) -> Step<()> {
        if generated == 0 {
            return Err(Stop::Refused(format!(
                "the engine generated no mutants; source globs matched nothing: {}",
                crate::mutation_campaign::py_list(&self.campaign.source)
            )));
        }
        if tested == 0 {
            return Err(Stop::Refused(format!(
                "no mutant was executed: all {generated} were reported as unreached \
                 or unbuildable, so no test ever ran against a mutation"
            )));
        }
        Ok(())
    }

    /// `require_executed`, counted from the receipt's rows.
    pub fn require_scored_rows(&self) -> Step<()> {
        let rows = rows(self.receipt);
        let tested = rows
            .iter()
            .filter(|row| matches!(row["outcome"].as_str(), Some(KILLED | SURVIVED)))
            .count();
        self.require_executed(rows.len() as u64, tested as u64)
    }

    /// Python-only adapters must never use an absent guest as an empty command.
    pub fn require_python(&self) -> Result<&'a str, String> {
        self.interpreter
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| "this mutation operation requires a Python guest".into())
    }

    /// The selected guest owns this key, overriding contradictory campaign values.
    pub(crate) fn guest_environment(
        &self,
        environment: &BTreeMap<String, String>,
    ) -> BTreeMap<String, String> {
        let mut bound = environment.clone();
        match self.interpreter {
            Some(interpreter) => {
                bound.insert(SNAPSHOT_PYTHON_ENV.into(), interpreter.into());
            }
            None => {
                bound.remove(SNAPSHOT_PYTHON_ENV);
            }
        }
        bound
    }

    /// Prove the pinned bytes held, in the snapshot and in the live repository.
    fn verify_identities(&mut self) -> Step<()> {
        let snapshot = drift(self.campaign, self.worktree)?;
        let repository = drift(self.campaign, self.repo_root)?;
        self.receipt.insert(
            "identity_check".into(),
            json!({"snapshot_drift": &snapshot, "repository_drift": &repository}),
        );
        if !snapshot.is_empty() {
            return Err(Stop::Refused(format!(
                "pinned bytes changed inside the snapshot during the run: {snapshot:?}"
            )));
        }
        if !repository.is_empty() {
            return Err(Stop::Refused(format!(
                "pinned bytes changed in the repository during the run: {repository:?}"
            )));
        }
        Ok(())
    }
}

fn interpreter_hint(baseline: &CommandResult) -> Option<&'static str> {
    let text = format!("{} {}", baseline.stdout_tail, baseline.stderr_tail);
    INTERPRETER_FAILURES
        .iter()
        .any(|marker| text.contains(marker))
        .then_some(
            "the baseline could not drive Python inside the snapshot: export \
             CONDUCTOR_SNAPSHOT_PYTHON (the engine adapters do this automatically; \
             [tool.conductor].snapshot_python overrides which interpreter)",
        )
}

/// Publish the receipt in its slim form; `forge receipt show` expands it.
pub fn write_receipt(path: &Path, receipt: &Receipt) -> Result<(), String> {
    let slim = slim_receipt(&Value::Object(receipt.clone()));
    let body = python_pretty(slim).map_err(|error| error.to_string())?;
    atomic_write(path, &body)
}

/// The compiled runner's identity, in place of the Python runner's file hashes.
fn native_runner(stamp: &BuildStamp) -> Result<Value, String> {
    Ok(json!({
        "schema_version": RUNNER_SCHEMA,
        "forge_version": stamp.forge_version,
        // This inode, even if the installed path was replaced since exec.
        "executable_sha256": sha256_path(Path::new("/proc/self/exe"))?,
        "compiled_source_sha256": stamp.source_sha256,
        "compiled_git_commit": stamp.git_commit,
        "source_matches_git": stamp.source_matches_git,
    }))
}

fn relative_to_repo(path: &Path, repo_root: &Path) -> Option<String> {
    path.strip_prefix(repo_root)
        .ok()
        .map(|relative| relative.to_string_lossy().into_owned())
}

fn open_receipt(
    campaign: &Campaign,
    repo_root: &Path,
    interpreter: Option<&str>,
    stamp: &BuildStamp,
    opened: f64,
) -> Result<Receipt, String> {
    let manifest = absolute_resolved(&campaign.manifest_path)?;
    let manifest = relative_to_repo(&manifest, repo_root)
        .ok_or_else(|| "campaign manifest must be inside the repository".to_string())?;
    let fields = json!({
        "schema_version": RECEIPT_SCHEMA,
        "campaign_id": campaign.campaign_id,
        "manifest": manifest,
        "manifest_sha256": campaign.manifest_sha256,
        "runner_kind": RUNNER_KIND,
        "native_runner": native_runner(stamp)?,
        "language": campaign.language,
        "mutation_engine": campaign.mutation_engine,
        "mutants_are_generated": true,
        "generated_at": isoformat_utc(opened),
        "status": RUNNING,
        "source_sha256": campaign.source_sha256,
        "test_sha256": campaign.test_sha256,
        "test_argv": campaign.test_argv,
        "interpreter": interpreter,
        "seed": campaign.seed,
        "baseline": null,
        "mutants": [],
        "mutation_score": null,
        "survivors": [],
        "survivor_baseline": campaign.survivor_baseline,
        "new_survivors": [],
        "resolved_survivors": [],
        "no_coverage": null,
        "test_value": null,
        "killer_enforcement": null,
    });
    match fields {
        Value::Object(receipt) => Ok(receipt),
        _ => Err("the receipt envelope is not an object".into()),
    }
}

/// Where the receipt goes and its repo-relative name; outside the repo refuses.
fn resolve_receipt_path(
    campaign: &Campaign,
    explicit: Option<&Path>,
    repo_root: &Path,
    opened: f64,
) -> Result<(PathBuf, String), String> {
    crate::mutation_campaign::validate_campaign_id(&campaign.campaign_id)?;
    let selected = match explicit {
        Some(path) => path.to_path_buf(),
        None => {
            let root = project_paths::resolve_key(repo_root, "mutation_receipt_root")?.0;
            let directory = repo_root.join(root);
            let stamp = format_compact_utc(opened);
            directory.join(format!("{}_{stamp}Z.json", campaign.campaign_id))
        }
    };
    let output = crate::mutation_write::confined_output(repo_root, &selected, "receipt path")?;
    let relative = relative_to_repo(&output, repo_root)
        .ok_or_else(|| "receipt path must be inside the repository".to_string())?;
    if explicit.is_none() {
        let directory = output.parent().ok_or("receipt path has no parent")?;
        fs::create_dir_all(directory).map_err(|error| {
            format!(
                "cannot create mutation receipt directory {}: {error}",
                directory.display()
            )
        })?;
    }
    Ok((output, relative))
}

/// The engine binary this campaign would run, or why it cannot.
fn selected_interpreter(campaign: &Campaign, repo_root: &Path) -> Result<Option<String>, String> {
    match campaign.mutation_engine.as_str() {
        "fest" => snapshot_python(repo_root).map(Some),
        "cargo-mutants" | "mull" => configured_snapshot_python(repo_root),
        other => Err(format!("{other} has no native adapter")),
    }
}

fn engine_binary(campaign: &Campaign, interpreter: Option<&str>) -> Result<String, String> {
    match campaign.mutation_engine.as_str() {
        "fest" => fest::binary(interpreter.ok_or("fest requires a Python guest")?),
        "cargo-mutants" => cargo::binary(),
        "mull" => mull::binary(),
        other => Err(format!("{other} has no native adapter")),
    }
}

fn execute(run: &mut EngineRun, binary: &str) -> Step<()> {
    match run.campaign.mutation_engine.clone().as_str() {
        "fest" => fest::execute(run, binary),
        "cargo-mutants" => cargo::execute(run, binary),
        "mull" => mull::execute(run, binary),
        other => Err(Stop::Refused(format!("{other} has no native adapter"))),
    }
}

/// The parts of one run that outlive its snapshot.
struct Execution<'a> {
    campaign: &'a mut Campaign,
    receipt: &'a mut Receipt,
    output_path: &'a Path,
    repo_root: &'a Path,
    interpreter: Option<&'a str>,
    binary: &'a str,
    supervisor: &'a Supervisor<'a>,
    planned: &'a mut Option<u64>,
}

/// Engine, then identity checks, then cleanup: the snapshot never outlives this.
fn in_snapshot(parts: Execution) -> Step<()> {
    let snapshot = Snapshot::create(parts.repo_root)?;
    parts.receipt.insert(
        "snapshot".into(),
        json!({
            "root": snapshot.root.to_string_lossy(),
            "commit": snapshot.commit,
            "included_untracked": snapshot.included_untracked,
            "removed": false,
        }),
    );
    let mut run = EngineRun {
        campaign: parts.campaign,
        receipt: parts.receipt,
        output_path: parts.output_path,
        repo_root: parts.repo_root,
        worktree: &snapshot.worktree,
        interpreter: parts.interpreter,
        supervisor: parts.supervisor,
        planned: None,
    };
    let finished = execute(&mut run, parts.binary).and_then(|()| {
        if let Some(signal) = interrupted(parts.supervisor)? {
            return Err(Stop::Cancelled(signal));
        }
        crate::mutation_source_coverage::require(run.campaign, run.worktree, run.receipt)?;
        run.verify_identities()
    });
    *parts.planned = run.planned;
    let removal = snapshot.remove();
    parts.receipt["snapshot"]["removed"] = json!(removal.is_ok());
    if let Err(error) = &removal {
        parts.receipt["snapshot"]["removal_error"] = json!(error);
    }
    finished?;
    Ok(removal?)
}

fn interrupted(supervisor: &Supervisor) -> Result<Option<i32>, String> {
    supervisor
        .interrupts
        .signal()
        .map_err(|error| format!("cannot read interrupts: {error}"))
}

/// Close a run that did not score: the receipt says why, and never PASS.
fn close_stopped(
    receipt: &mut Receipt,
    output: &Path,
    planned: Option<u64>,
    stop: Stop,
) -> Step<()> {
    let accounted = account(receipt, planned).map(|_| ());
    match stop {
        Stop::Cancelled(signal) => {
            receipt.insert("status".into(), json!(CANCELLED));
            receipt.insert("cancelled_by_signal".into(), json!(signal));
            if let Err(error) = accounted {
                receipt.insert("error".into(), json!(error));
            }
            write_receipt(output, receipt)?;
            Ok(())
        }
        Stop::Refused(message) => {
            let message = match accounted {
                Ok(()) => message,
                Err(error) => format!("{message}; {error}"),
            };
            if status(receipt) == RUNNING {
                receipt.insert("status".into(), json!(ERROR));
            }
            receipt.insert("error".into(), json!(message));
            write_receipt(output, receipt)?;
            Err(Stop::Refused(message))
        }
    }
}

/// Score a run whose engine, identity checks and cleanup all finished.
fn close_scored(
    campaign: &mut Campaign,
    receipt: &mut Receipt,
    planned: Option<u64>,
    relative: &str,
) -> Result<(), String> {
    score(campaign, receipt, planned)?;
    record_baseline(campaign, receipt, planned)?;
    receipt.insert("receipt_path".into(), json!(relative));
    Ok(())
}

/// Generate, run and score a campaign's mutants inside a disposable snapshot.
///
/// `Ok` is every run that reached a verdict, cancelled included; the caller maps
/// the status to an exit code. A refusal has already been written to the receipt
/// when one was open.
pub fn run_campaign(request: &RunRequest) -> Step<Receipt> {
    if !request.allow_mutations {
        return Err("refusing mutation run without --allow-mutations".into());
    }
    let repo_root = absolute_resolved(&request.repo_root)?;
    let mut campaign = Campaign::load(&request.manifest)?;
    validate_run_scope(
        &campaign,
        &repo_root,
        &request.owner,
        &request.base,
        &request.only,
    )?;
    let interpreter = selected_interpreter(&campaign, &repo_root)?;
    let binary = engine_binary(&campaign, interpreter.as_deref())?;
    let opened = now();
    let mut receipt = open_receipt(
        &campaign,
        &repo_root,
        interpreter.as_deref(),
        &request.stamp,
        opened,
    )?;
    let cap = memory_cap()?;
    receipt.insert("memory_cap_bytes".into(), json!(cap.map(|cap| cap.limit)));
    let (output, relative) =
        resolve_receipt_path(&campaign, request.receipt.as_deref(), &repo_root, opened)?;
    let interrupts =
        Interrupts::install().map_err(|error| format!("cannot own signals: {error}"))?;
    write_receipt(&output, &receipt)?;
    let supervisor = Supervisor {
        interrupts: &interrupts,
        registry: live_pgids_path(&repo_root)?,
        memory_cap: cap,
    };
    let mut planned = None;
    let executed = in_snapshot(Execution {
        campaign: &mut campaign,
        receipt: &mut receipt,
        output_path: &output,
        repo_root: &repo_root,
        interpreter: interpreter.as_deref(),
        binary: &binary,
        supervisor: &supervisor,
        planned: &mut planned,
    })
    .and_then(|()| {
        close_scored(&mut campaign, &mut receipt, planned, &relative).map_err(Stop::Refused)
    });
    match executed {
        Ok(()) => write_receipt(&output, &receipt)?,
        Err(stop) => close_stopped(&mut receipt, &output, planned, stop)?,
    }
    Ok(receipt)
}

/// Whether the campaign could run now, without running it.
pub fn inspect(campaign: &Campaign, repo_root: &Path) -> Result<Map<String, Value>, String> {
    let mut reasons = Vec::new();
    let interpreter = selected_interpreter(campaign, repo_root);
    let binary = interpreter
        .and_then(|interpreter| engine_binary(campaign, interpreter.as_deref()))
        .map_err(|error| reasons.push(error))
        .ok();
    for (relative, expected) in &campaign.source_sha256 {
        let target = repo_root.join(relative);
        if !target.is_file() {
            reasons.push(format!("pinned source is absent: {relative}"));
        } else if &sha256_path(&target)? != expected {
            reasons.push(format!("pinned source drifted: {relative}"));
        }
    }
    let status = if reasons.is_empty() {
        "READY"
    } else {
        "NOT_READY"
    };
    let report = json!({
        "campaign_id": campaign.campaign_id,
        "mutation_engine": campaign.mutation_engine,
        "engine_binary": binary,
        "source": campaign.source,
        "test_argv": campaign.test_argv,
        "survivor_baseline": campaign.survivor_baseline,
        "status": status,
        "readiness_reasons": reasons,
    });
    match report {
        Value::Object(report) => Ok(report),
        _ => Err("the inspection report is not an object".into()),
    }
}

#[cfg(test)]
#[path = "mutation_run_tests.rs"]
mod tests;
