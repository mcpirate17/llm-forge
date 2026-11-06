//! One generated campaign, end to end: refuse what is out of scope, open the
//! receipt, run the engine in a disposable snapshot, verify the pinned bytes and
//! the cleanup, then score.
//!
//! The receipt is on disk from the first moment, so a run killed outright leaves
//! a RUNNING receipt rather than nothing. Every other ending writes a final
//! status: only a run that finished, scored clean and verified is PASS.

use crate::instant::{format_compact_utc, isoformat_utc, now};
use crate::json_canon::python_pretty;
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
    pub execution_grants: Option<PathBuf>,
    pub execution_grants_sha256: Option<String>,
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
    pub(crate) runtime: Option<&'a crate::mutation_execution_legacy::Runtime<'a>>,
    #[cfg(test)]
    pub(crate) test_session: Option<&'a crate::mutation_execution::CommandSession<'a>>,
    /// The engine's own mutant count, set as soon as a report states one.
    pub planned: Option<u64>,
}

impl<'a> EngineRun<'a> {
    /// One child, bound to the private bytecode cache of the directory it runs in.
    pub fn run(
        &mut self,
        argv: &[String],
        cwd: &Path,
        timeout_seconds: i64,
        _environment: &BTreeMap<String, String>,
        _mutated_paths: &[PathBuf],
    ) -> Step<(CommandResult, String)> {
        #[cfg(test)]
        if self.runtime.is_none() {
            if let Some(session) = self.test_session {
                return session
                    .command(
                        self,
                        argv,
                        cwd,
                        timeout_seconds,
                        crate::mutation_execution::Capture::Complete,
                    )
                    .map(|(report, stdout, _)| (report, stdout));
            }
        }
        let runtime = self
            .runtime
            .ok_or("mutation payload requires an admitted confined execution session")?;
        runtime.command(self, argv, cwd, timeout_seconds)
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
    grants: Option<&'a crate::mutation_execution_grants::Grants>,
    inputs: Option<&'a crate::mutation_execution_legacy::Inputs>,
}

/// Engine, then identity checks and cleanup. Opt-in Cargo snapshots are retained
/// whenever the confined execution owner cannot establish process-tree cleanup.
fn in_snapshot(parts: Execution) -> Step<()> {
    if parts.campaign.generated_value.is_none() {
        return in_legacy_snapshot(parts);
    }
    let mut snapshot = Snapshot::create(parts.repo_root)?;
    let requires_tree_cleanup = parts.campaign.generated_value.is_some();
    if requires_tree_cleanup {
        snapshot.preserve();
    }
    parts.receipt.insert(
        "snapshot".into(),
        json!({
            "root": snapshot.root.to_string_lossy(),
            "commit": snapshot.commit,
            "included_untracked": snapshot.included_untracked,
            "removed": false,
        }),
    );
    if requires_tree_cleanup {
        parts.receipt.insert("cargo_value_process_cleanup".into(),
            json!({"complete":false,"commands_observed":0,"scope":"awaiting confined execution owner"}));
        // Identify retained recovery paths durably before a child can be launched.
        write_receipt(parts.output_path, parts.receipt)?;
    }
    let mut run = EngineRun {
        campaign: parts.campaign,
        receipt: parts.receipt,
        output_path: parts.output_path,
        repo_root: parts.repo_root,
        worktree: &snapshot.worktree,
        interpreter: parts.interpreter,
        supervisor: parts.supervisor,
        runtime: None,
        #[cfg(test)]
        test_session: None,
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
    let removal = dispose_snapshot(snapshot, parts.receipt, requires_tree_cleanup);
    finished?;
    removal
}

fn ledger_verified(receipt: &Receipt) -> bool {
    let cleanup = &receipt["process_cleanup"];
    let Some(commands) = receipt.get("process_commands").and_then(Value::as_array) else {
        return false;
    };
    !commands.is_empty()
        && cleanup["schema_version"] == 1
        && cleanup["complete"] == true
        && cleanup["commands_observed"].as_u64() == Some(commands.len() as u64)
        && commands.iter().all(|command| {
            command["state"] == "observed" && command["confinement"]["cleanup_complete"] == true
        })
}
fn dispose_legacy_snapshot(
    mut snapshot: Snapshot,
    receipt: &mut Receipt,
    verified: bool,
) -> Step<()> {
    if !verified || !ledger_verified(receipt) {
        snapshot.preserve();
        receipt["snapshot"]["removed"] = json!(false);
        receipt["snapshot"]["retained_reason"] =
            json!("process cleanup or execution input verification is incomplete");
        return Err(Stop::Refused(format!(
            "mutation cleanup or input verification is incomplete; snapshot retained at {}",
            snapshot.root.display()
        )));
    }
    let removal = snapshot.remove();
    receipt["snapshot"]["removed"] = json!(removal.is_ok());
    if let Err(error) = &removal {
        receipt["snapshot"]["removal_error"] = json!(error);
    }
    Ok(removal?)
}

fn finish_legacy_snapshot(
    runtime: &crate::mutation_execution_legacy::Runtime<'_>,
    snapshot: Snapshot,
    receipt: &mut Receipt,
) -> Step<()> {
    let checked = runtime.verify(Some(&snapshot.worktree));
    if let Err(error) = &checked {
        receipt.insert(
            "execution_verification_error".into(),
            json!(format!("{error:#}")),
        );
    }
    let mut verified = checked.is_ok();
    let cache = if verified && ledger_verified(receipt) {
        runtime.finish_cache(&snapshot.worktree)
    } else {
        Ok(())
    };
    if let Err(error) = &cache {
        verified = false;
        receipt["fest_cache"]["removal_error"] = json!(format!("{error:#}"));
    } else if verified && ledger_verified(receipt) && receipt.contains_key("fest_cache") {
        receipt["fest_cache"]["removed"] = json!(true);
    }
    let removal = dispose_legacy_snapshot(snapshot, receipt, verified);
    checked.map_err(|error| Stop::Refused(format!("execution input verification: {error:#}")))?;
    cache.map_err(|error| Stop::Refused(format!("Fest cache cleanup: {error:#}")))?;
    removal
}

fn in_legacy_snapshot(parts: Execution<'_>) -> Step<()> {
    let grants = parts
        .grants
        .ok_or("ordinary mutation execution requires operator grants")?;
    let inputs = parts
        .inputs
        .ok_or("ordinary mutation admission is missing")?;
    let mut snapshot = Snapshot::allocate(&grants.scratch_parent)?;
    snapshot.preserve();
    parts.receipt.insert(
        "snapshot".into(),
        json!({"root":snapshot.root,"removed":false}),
    );
    parts.receipt.insert("process_commands".into(), json!([]));
    parts.receipt.insert("process_cleanup".into(), json!({"schema_version":1,"complete":false,"commands_observed":0,"scope":"awaiting confined execution owner"}));
    parts
        .receipt
        .insert("execution_grants_sha256".into(), json!(grants.digest()));
    write_receipt(parts.output_path, parts.receipt)?;
    let runtime = crate::mutation_execution_legacy::Runtime::prepare(&snapshot, grants, inputs)
        .map_err(|e| format!("execution admission: {e:#}"))?;
    runtime
        .record_admission(parts.receipt)
        .map_err(|e| format!("execution cache admission: {e:#}"))?;
    write_receipt(parts.output_path, parts.receipt)?;
    inputs
        .populate(&mut snapshot, parts.repo_root)
        .map_err(|e| format!("snapshot population: {e:#}"))?;
    snapshot.remove_git_metadata()?;
    inputs
        .verify_snapshot(&snapshot.worktree)
        .map_err(|e| format!("mutation snapshot admission: {e:#}"))?;
    runtime
        .verify(Some(&snapshot.worktree))
        .map_err(|e| format!("execution input verification: {e:#}"))?;
    parts.receipt.insert(
        "supporting_inputs".into(),
        json!(inputs.supporting_identity()),
    );
    parts.receipt["snapshot"]["commit"] = json!(snapshot.commit);
    parts.receipt["snapshot"]["included_untracked"] = json!(snapshot.included_untracked);
    parts.receipt["snapshot"]["git_history"] =
        json!("private Git metadata removed; history unavailable");
    write_receipt(parts.output_path, parts.receipt)?;
    let mut run = EngineRun {
        campaign: parts.campaign,
        receipt: parts.receipt,
        output_path: parts.output_path,
        repo_root: parts.repo_root,
        worktree: &snapshot.worktree,
        interpreter: parts.interpreter,
        supervisor: parts.supervisor,
        runtime: Some(&runtime),
        #[cfg(test)]
        test_session: None,
        planned: None,
    };
    let finished = runtime
        .admit(&mut run)
        .and_then(|()| execute(&mut run, parts.binary))
        .and_then(|()| {
            if let Some(signal) = interrupted(parts.supervisor)? {
                return Err(Stop::Cancelled(signal));
            }
            crate::mutation_source_coverage::require(run.campaign, run.worktree, run.receipt)?;
            run.verify_identities()
        });
    *parts.planned = run.planned;
    let removal = finish_legacy_snapshot(&runtime, snapshot, parts.receipt);
    finished?;
    removal
}
fn prepare_legacy(
    request: &RunRequest,
    campaign: &Campaign,
    root: &Path,
) -> Step<
    Option<(
        crate::mutation_execution_grants::Grants,
        crate::mutation_execution_legacy::Inputs,
    )>,
> {
    if campaign.generated_value.is_some() {
        if request.execution_grants.is_some() || request.execution_grants_sha256.is_some() {
            return Err("opt-in Cargo value execution uses its existing operator interface".into());
        }
        return Ok(None);
    }
    let fest = campaign.language == "python"
        && campaign.mutation_engine == "fest"
        && campaign.option_text("execution_profile").as_deref() == Some("fest-confined-v1");
    if !fest && (campaign.language != "rust" || campaign.mutation_engine != "cargo-mutants") {
        return Err("ordinary mutation profile is not yet supported; explicit confined Cargo or generated Fest grants are required".into());
    }
    let path = request.execution_grants.as_deref().ok_or(
        "ordinary mutation execution requires --execution-grants and --execution-grants-sha256",
    )?;
    let sha = request.execution_grants_sha256.as_deref().ok_or(
        "ordinary mutation execution requires --execution-grants and --execution-grants-sha256",
    )?;
    let source = crate::candidate_source::Source::open(root)
        .map_err(|e| format!("source repository: {e:#}"))?;
    let git_dir = source
        .repository()
        .path()
        .canonicalize()
        .map_err(|e| format!("Git directory: {e}"))?;
    let grants = crate::mutation_execution_grants::Grants::load(
        path,
        sha,
        source.root(),
        source.common_dir(),
        &git_dir,
    )
    .map_err(|e| format!("execution grants: {e:#}"))?;
    let inputs = crate::mutation_execution_legacy::Inputs::capture(campaign, root, &grants)
        .map_err(|e| format!("ordinary mutation admission: {e:#}"))?;
    Ok(Some((grants, inputs)))
}

/// Confined guest selection belongs to held operator grants, never ambient PATH.
fn selected_tools(
    campaign: &Campaign,
    root: &Path,
    grants: Option<&crate::mutation_execution_grants::Grants>,
) -> Result<(Option<String>, String), String> {
    let Some(grants) = grants else {
        let interpreter = selected_interpreter(campaign, root)?;
        let binary = engine_binary(campaign, interpreter.as_deref())?;
        return Ok((interpreter, binary));
    };
    if configured_snapshot_python(root)?.is_some() {
        return Err("confined mutation profiles refuse a project-configured Python guest; use operator execution grants".into());
    }
    let tool = |alias| {
        grants
            .program(alias)
            .map(|path| path.to_string_lossy().into_owned())
            .map_err(|e| format!("execution grants: {e:#}"))
    };
    if campaign.mutation_engine == "fest" {
        Ok((Some(tool("python")?), tool("fest")?))
    } else {
        Ok((None, tool("cargo-mutants")?))
    }
}
fn dispose_snapshot(
    mut snapshot: Snapshot,
    receipt: &mut Receipt,
    requires_tree_cleanup: bool,
) -> Step<()> {
    if requires_tree_cleanup
        && receipt
            .get("cargo_value_process_cleanup")
            .and_then(|value| value.get("complete"))
            != Some(&Value::Bool(true))
    {
        snapshot.preserve();
        receipt["snapshot"]["removed"] = json!(false);
        receipt["snapshot"]["retained_reason"] = json!("process-tree cleanup is unverified");
        return Err(Stop::Refused(format!(
            "process-tree cleanup is unverified; snapshot retained at {}",
            snapshot.root.display()
        )));
    }
    let removal = snapshot.remove();
    receipt["snapshot"]["removed"] = json!(removal.is_ok());
    if let Err(error) = &removal {
        receipt["snapshot"]["removal_error"] = json!(error);
    }
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
    if campaign.option_text("execution_profile").as_deref() == Some("fest-confined-v1") {
        if let Some(error) = conductor_native::mutation_fest_receipt::cleanup_error(receipt) {
            return Err(error);
        }
        if let Some(error) = conductor_native::mutation_fest_receipt::attribution_error(
            receipt,
            &campaign.campaign_id,
            &json!(campaign.source_sha256),
            &json!(campaign.test_sha256),
        ) {
            return Err(error);
        }
    }
    if campaign.generated_value.is_none()
        && (!ledger_verified(receipt) || receipt["snapshot"]["removed"] != true)
    {
        return Err("owned process or snapshot cleanup is incomplete".into());
    }
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
    let legacy = prepare_legacy(request, &campaign, &repo_root)?;
    let (interpreter, binary) = selected_tools(
        &campaign,
        &repo_root,
        legacy.as_ref().map(|(grants, _)| grants),
    )?;
    let opened = now();
    let mut receipt = open_receipt(
        &campaign,
        &repo_root,
        interpreter.as_deref(),
        &request.stamp,
        opened,
    )?;
    let cap = memory_cap()?;
    if let Some((grants, _)) = &legacy {
        let enforcement = crate::mutation_execution_grants::memory_enforcement(
            &grants.resources,
            cap,
            std::env::var_os("MUTATION_MEMORY_CAP_GIB").is_some(),
        )
        .map_err(|e| e.to_string())?;
        receipt.insert("memory_enforcement".into(), enforcement);
        receipt.insert("memory_cap_bytes".into(), Value::Null);
    } else {
        receipt.insert("memory_cap_bytes".into(), json!(cap.map(|cap| cap.limit)));
    }
    let (output, relative) =
        resolve_receipt_path(&campaign, request.receipt.as_deref(), &repo_root, opened)?;
    if let Some((_, inputs)) = &legacy {
        inputs
            .receipt(&relative)
            .map_err(|e| format!("receipt admission: {e:#}"))?;
    }
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
        grants: legacy.as_ref().map(|(grants, _)| grants),
        inputs: legacy.as_ref().map(|(_, inputs)| inputs),
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
    if campaign.generated_value.is_none() {
        reasons
            .push("ordinary mutation execution requires explicit pinned execution grants".into());
    }
    let binary = if campaign.option_text("execution_profile").as_deref() == Some("fest-confined-v1")
    {
        // Inspection has no operator grant document; do not infer a guest from host state.
        reasons.push(
            "confined Fest selects its Python guest and engine only from held operator grants"
                .into(),
        );
        None
    } else {
        selected_interpreter(campaign, repo_root)
            .and_then(|interpreter| engine_binary(campaign, interpreter.as_deref()))
            .map_err(|error| reasons.push(error))
            .ok()
    };
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

#[cfg(test)]
#[path = "mutation_run_owned_tests.rs"]
mod owned_tests;
