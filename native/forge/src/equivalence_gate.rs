//! Explicit committed or selected live-module probing through the confined Stage owner.
#[path = "equivalence_gate_selection.rs"]
mod selection;
#[path = "equivalence_gate_source.rs"]
mod source;
use crate::{
    candidate_equivalence_inputs::{self, Artifacts, CaptureContext},
    candidate_equivalence_stage::{self, Probe, StageContext},
    candidate_execution_workspace::Grants,
    candidate_snapshot::{Limits as SnapshotLimits, Snapshot},
    candidate_source::Source,
    confined_execution::Limits,
    process_signals::Interrupts,
};
use anyhow::{ensure, Context, Result};
use clap::{ArgGroup, Args};
use selection::SelectedModules;
use serde_json::{json, Value};
use slop_core::gate_types::Summary;
use source::SelectedSource;
use std::{path::PathBuf, sync::Arc, time::Duration};

#[derive(Args)]
#[command(group(ArgGroup::new("live_inputs").args(["inputs", "tracked_only"])))]
#[command(group(ArgGroup::new("module_selection").args(["module", "changed_from", "all_tracked"]).required(true)))]
pub struct GateArgs {
    /// Repository containing the selected source and original operator authority.
    #[arg(long, default_value = ".")]
    root: PathBuf,
    /// Full lowercase 40-character commit OID, never a branch or abbreviated name.
    #[arg(long, required_unless_present = "live", conflicts_with = "live")]
    commit: Option<String>,
    /// Capture live tracked inputs and explicitly selected untracked/ignored inputs.
    #[arg(long, requires = "live_inputs", conflicts_with = "commit")]
    live: bool,
    /// Additional live input file or directory; repeat for each bounded scope.
    #[arg(long = "input", requires = "live", conflicts_with = "commit")]
    inputs: Vec<String>,
    /// Explicitly capture tracked live inputs and root configuration, with no extra roots.
    #[arg(long, requires = "live", conflicts_with = "commit")]
    tracked_only: bool,
    /// Literal relative .py path, evaluated as a whole module; repeat for multiple modules.
    #[arg(long)]
    module: Vec<String>,
    /// Select production Python modules changed from the merge base with this reference.
    #[arg(long, requires = "live", conflicts_with = "commit")]
    changed_from: Option<String>,
    /// Select existing production Python modules from the original index inventory.
    #[arg(long, requires = "live", conflicts_with = "commit")]
    all_tracked: bool,
    /// Absolute operator-owned grants file outside the repository.
    #[arg(long)]
    grants: PathBuf,
    /// Explicit external Slop Core guest extension, outside repository and scratch.
    #[arg(long)]
    guest_path: PathBuf,
    #[arg(long)]
    guest_sha256: String,
    /// Separately supplied current record data; must match captured configuration.
    #[arg(long, requires = "store_sha256")]
    store: Option<PathBuf>,
    #[arg(long, requires = "store")]
    store_sha256: Option<String>,
    /// Explicit existing output directory matching captured run_artifacts.
    #[arg(long)]
    artifacts: Option<PathBuf>,
    /// Maximum guest workers inside the single resource-limited Stage (1..32).
    #[arg(long, default_value_t = 1)]
    jobs: usize,
    /// Stage wall deadline; defaults to the lesser of 180 seconds and the grant.
    #[arg(long)]
    wall_seconds: Option<u64>,
    /// Stage CPU limit; defaults to the lesser of 180 seconds and the grant.
    #[arg(long)]
    cpu_seconds: Option<u64>,
    /// Address-space ceiling; defaults to the lesser of 8192 MiB and the grant.
    #[arg(long)]
    address_space_mib: Option<u64>,
    /// Exit 1 when the measured summary contains blocking findings.
    #[arg(long)]
    enforce: bool,
    /// Emit a versioned result with source identities, summary and execution evidence.
    #[arg(long)]
    json: bool,
}

fn validate_selection(args: &GateArgs) -> Result<()> {
    ensure!(
        args.live != args.commit.is_some(),
        "select exactly one of an explicit commit or live inputs"
    );
    if let Some(commit) = &args.commit {
        ensure!(
            commit.len() == 40
                && commit
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "commit must be a full lowercase 40-character commit OID"
        );
        ensure!(
            args.inputs.is_empty()
                && !args.tracked_only
                && args.changed_from.is_none()
                && !args.all_tracked,
            "live inputs and automatic module selection require live mode"
        );
    } else {
        ensure!(
            args.tracked_only != !args.inputs.is_empty(),
            "live mode requires either explicit input paths or tracked-only inputs"
        );
    }
    selection::validate(args)?;
    ensure!(
        (1..=32).contains(&args.jobs),
        "jobs must be between 1 and 32"
    );
    Ok(())
}

fn execution_limits(args: &GateArgs, grants: &Grants) -> Result<Limits> {
    let wall = args
        .wall_seconds
        .unwrap_or(grants.max_wall_seconds.min(180));
    let cpu = args.cpu_seconds.unwrap_or(grants.max_cpu_seconds.min(180));
    let memory = args
        .address_space_mib
        .unwrap_or(grants.max_address_space_mib.min(8192));
    ensure!(
        wall > 0 && wall <= grants.max_wall_seconds,
        "wall limit exceeds operator grant"
    );
    ensure!(
        cpu > 0 && cpu <= grants.max_cpu_seconds,
        "CPU limit exceeds operator grant"
    );
    ensure!(
        memory > 0 && memory <= grants.max_address_space_mib,
        "address-space limit exceeds operator grant"
    );
    if let Some(batch) = &grants.batch {
        batch.parallelism(1, grants.resources)?;
    }
    Ok(Limits {
        timeout: Duration::from_secs(wall),
        max_output_chars: 64 * 1024,
        cpu_seconds: Some(cpu),
        address_space_bytes: Some(
            memory
                .checked_mul(1024 * 1024)
                .context("address-space overflow")?,
        ),
    })
}

struct Completed {
    summary: Summary,
    bytes: Vec<u8>,
    evidence: Value,
    artifacts: Option<Artifacts>,
}

fn evaluate(
    args: &GateArgs,
    selected: &SelectedSource,
    modules: &SelectedModules,
    snapshot: &Snapshot,
    grants: Arc<Grants>,
    limits: Limits,
    interrupts: &Interrupts,
) -> Result<Completed> {
    modules.verify(selected)?;
    snapshot.verify_materialized(selected.tree_source())?;
    let mut captured = candidate_equivalence_inputs::capture_modules(
        CaptureContext {
            origin: selected.origin(),
            tree_source: selected.tree_source(),
            snapshot,
            grants: &grants,
        },
        modules.paths(),
        args.store.as_deref().zip(args.store_sha256.as_deref()),
        args.artifacts.as_deref(),
        args.jobs,
    )?;
    if args.live {
        captured.evidence["source_kind"] = json!("live_snapshot_tree");
    }
    ensure!(
        captured.targets.len() == modules.paths().len(),
        "whole-module target count differs from selected modules"
    );
    let capture_path = snapshot.path();
    let admitted = candidate_equivalence_stage::admit_probe(
        &grants,
        Probe {
            guest_path: &args.guest_path,
            guest_sha256: &args.guest_sha256,
            targets: &captured.targets,
            waivers: captured.waivers.as_deref(),
            timeout_seconds: captured.timeout_seconds,
            jobs: captured.jobs,
        },
        StageContext {
            source_root: selected.origin().root(),
            capture_snapshot: &capture_path,
            data: json!({"scope":selected.scope(), "commit_oid":args.commit,
                "tree_oid":snapshot.report().tree_oid, "module":modules.module(),
                "modules":modules.paths(), "selection":modules.evidence(),
                "grants_sha256":grants.digest(), "execution_verified":false}),
        },
    )?;
    let workspace = selected.workspace(&grants)?;
    workspace.verify_materialized(selected.tree_source())?;
    let prepared =
        candidate_equivalence_stage::prepare_workspace(workspace, &grants, limits, admitted)?;
    captured.verify()?;
    snapshot.verify_materialized(selected.tree_source())?;
    modules.verify(selected)?;
    ensure!(
        !interrupts.interrupted()?,
        "equivalence interrupted before execution"
    );
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(3)
        .build()?;
    let (stage, report, bytes, summary_sha256) = runtime.block_on(
        candidate_equivalence_stage::execute(prepared, grants, interrupts),
    )?;
    captured.verify()?;
    snapshot.verify_materialized(selected.tree_source())?;
    modules.verify(selected)?;
    let summary: Summary = serde_json::from_slice(&bytes).context("invalid equivalence summary")?;
    Ok(Completed {
        summary,
        bytes,
        evidence: json!({"schema_version":1, "action":"equivalence_gate",
            "scope":selected.scope(), "commit_oid":args.commit,
            "tree_oid":snapshot.report().tree_oid, "module":modules.module(),
            "modules":modules.paths(), "selection":modules.evidence(),
            "working_tree_evaluated":args.live, "original_git_history":false,
            "guest_output_authenticated":false, "approval":false,
            "inputs":captured.evidence, "stage":stage, "execution":report,
            "summary_sha256":summary_sha256}),
        artifacts: captured.artifacts.take(),
    })
}

fn finish_capture<T>(result: Result<T>, cleanup: Result<()>) -> Result<T> {
    match (result, cleanup) {
        (Ok(completed), Ok(())) => Ok(completed),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(cleanup)) => Err(cleanup).context("equivalence capture cleanup failed"),
        (Err(error), Err(cleanup)) => Err(anyhow::anyhow!(
            "equivalence failed: {error:#}; capture cleanup failed: {cleanup:#}"
        )),
    }
}

pub fn run(args: GateArgs) -> Result<u8> {
    validate_selection(&args)?;
    let interrupts = Interrupts::install()?;
    let origin = Source::open(&args.root)?;
    let grants = Arc::new(Grants::load(&args.grants, &origin)?);
    let limits = execution_limits(&args, &grants)?;
    let selected = SelectedSource::capture(&args, origin, &grants)?;
    let tree = selected.tree().to_owned();
    let live_evidence = selected.live_evidence();
    let result = evaluate_selected(&args, &selected, grants, limits, &interrupts);
    let mut completed = finish_capture(result, selected.discard())?;
    if let Some(evidence) = live_evidence {
        completed.evidence["live_inputs"] = evidence;
        completed.evidence["live_capture_cleanup_complete"] = json!(true);
        completed.evidence["execution_read_scope"] = json!({
            "policy":"operator_grants",
            "original_source_readable":true,
            "captured_inputs_are_dependency_closure":false
        });
    }
    ensure!(
        !interrupts.interrupted()?,
        "equivalence interrupted before publication"
    );
    let artifact = completed
        .artifacts
        .as_ref()
        .map(|destination| destination.publish(&completed.bytes, true))
        .transpose()?;
    completed.evidence["published_artifact"] = json!(artifact);
    completed.evidence["capture_cleanup_complete"] = json!(true);
    completed.evidence["summary"] = serde_json::to_value(&completed.summary)?;
    if args.json {
        println!("{}", serde_json::to_string(&completed.evidence)?);
    } else {
        if let Some(commit) = &args.commit {
            println!(
                "Commit {commit} · tree {tree} · {}",
                module_label(&completed.evidence)
            );
        } else {
            println!(
                "Live selected inputs · tree {tree} · {}",
                module_label(&completed.evidence)
            );
        }
        print!("{}", slop_core::gate_report::render(&completed.summary)?);
    }
    Ok(u8::from(
        args.enforce && !completed.summary.blocking.is_empty(),
    ))
}

fn module_label(evidence: &Value) -> String {
    if let Some(module) = evidence["module"].as_str() {
        format!("whole module {module}")
    } else {
        format!(
            "{} whole modules",
            evidence["modules"].as_array().map_or(0, Vec::len)
        )
    }
}

fn evaluate_selected(
    args: &GateArgs,
    selected: &SelectedSource,
    grants: Arc<Grants>,
    limits: Limits,
    interrupts: &Interrupts,
) -> Result<Completed> {
    let modules = SelectedModules::capture(args, selected)?;
    if !modules.paths().is_empty() {
        return evaluate_capture(args, selected, &modules, grants, limits, interrupts);
    }
    let artifacts = candidate_equivalence_inputs::empty_artifacts(
        selected.origin(),
        selected.tree_source(),
        selected.tree(),
        &grants,
        args.artifacts.as_deref(),
        args.jobs,
    )?;
    modules.verify(selected)?;
    let summary = Summary::default();
    let bytes = serde_json::to_vec(&summary)?;
    use sha2::{Digest, Sha256};
    Ok(Completed {
        summary,
        evidence: json!({"schema_version":1, "action":"equivalence_gate",
            "scope":selected.scope(), "commit_oid":args.commit, "tree_oid":selected.tree(),
            "module":Value::Null, "modules":[], "selection":modules.evidence(),
            "working_tree_evaluated":args.live, "original_git_history":false,
            "guest_output_authenticated":false, "approval":false,
            "execution_verified":false, "execution_skipped":"no_selected_modules",
            "summary_sha256":format!("{:x}", Sha256::digest(&bytes))}),
        bytes,
        artifacts,
    })
}

fn evaluate_capture(
    args: &GateArgs,
    selected: &SelectedSource,
    modules: &SelectedModules,
    grants: Arc<Grants>,
    limits: Limits,
    interrupts: &Interrupts,
) -> Result<Completed> {
    let snapshot = Snapshot::create(
        selected.tree_source(),
        selected.tree(),
        &grants
            .scratch_parent
            .join(crate::resource_id("equivalence-capture")?),
        SnapshotLimits::default(),
    )?;
    let result = evaluate(
        args, selected, modules, &snapshot, grants, limits, interrupts,
    );
    finish_capture(result, snapshot.discard())
}

#[cfg(test)]
#[path = "equivalence_gate_tests.rs"]
mod tests;
