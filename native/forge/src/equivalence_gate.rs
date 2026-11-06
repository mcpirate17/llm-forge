//! Explicit committed-module probing through the existing confined Stage owner.
use crate::{
    candidate_equivalence_inputs::{self, Artifacts, CaptureContext},
    candidate_equivalence_stage::{self, Probe, StageContext},
    candidate_execution_workspace::{Grants, Workspace},
    candidate_snapshot::{Limits as SnapshotLimits, Snapshot},
    candidate_source::Source,
    confined_execution::Limits,
    process_signals::Interrupts,
};
use anyhow::{ensure, Context, Result};
use clap::Args;
use serde_json::{json, Value};
use slop_core::gate_types::Summary;
use std::{path::PathBuf, sync::Arc, time::Duration};

#[derive(Args)]
pub struct GateArgs {
    /// Repository containing the requested commit; live changes are not evaluated.
    #[arg(long, default_value = ".")]
    root: PathBuf,
    /// Full lowercase 40-character commit OID, never a branch or abbreviated name.
    #[arg(long)]
    commit: String,
    /// One literal relative .py path, evaluated as a whole module from the commit.
    #[arg(long)]
    module: String,
    /// Absolute operator-owned grants file outside the repository.
    #[arg(long)]
    grants: PathBuf,
    /// Explicit external Slop Core guest extension, outside repository and scratch.
    #[arg(long)]
    guest_path: PathBuf,
    #[arg(long)]
    guest_sha256: String,
    /// Separately supplied current record data; must match committed configuration.
    #[arg(long, requires = "store_sha256")]
    store: Option<PathBuf>,
    #[arg(long, requires = "store")]
    store_sha256: Option<String>,
    /// Explicit existing output directory matching committed run_artifacts.
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
        args.commit.len() == 40
            && args
                .commit
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "commit must be a full lowercase 40-character commit OID"
    );
    crate::candidate_source::safe_path(args.module.as_bytes())?;
    ensure!(
        args.module.ends_with(".py") && !args.module.chars().any(char::is_control),
        "module must be a literal relative Python source path"
    );
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
    source: &Source,
    snapshot: &Snapshot,
    grants: Arc<Grants>,
    limits: Limits,
    interrupts: &Interrupts,
) -> Result<Completed> {
    snapshot.verify_materialized(source)?;
    let mut captured = candidate_equivalence_inputs::capture_committed(
        CaptureContext {
            source,
            snapshot,
            grants: &grants,
        },
        &args.module,
        args.store.as_deref().zip(args.store_sha256.as_deref()),
        args.artifacts.as_deref(),
        args.jobs,
    )?;
    ensure!(
        captured.targets.len() == 1,
        "named gate requires exactly one target module"
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
            source_root: source.root(),
            capture_snapshot: &capture_path,
            data: json!({"scope":"committed-whole-module", "commit_oid":args.commit,
                "tree_oid":snapshot.report().tree_oid, "module":args.module,
                "grants_sha256":grants.digest(), "execution_verified":false}),
        },
    )?;
    let workspace = Workspace::new(source, &snapshot.report().tree_oid, &grants)?;
    workspace.verify_materialized(source)?;
    let prepared =
        candidate_equivalence_stage::prepare_workspace(workspace, &grants, limits, admitted)?;
    captured.verify()?;
    snapshot.verify_materialized(source)?;
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
    snapshot.verify_materialized(source)?;
    let summary: Summary = serde_json::from_slice(&bytes).context("invalid equivalence summary")?;
    Ok(Completed {
        summary,
        bytes,
        evidence: json!({"schema_version":1, "action":"equivalence_gate",
            "scope":"committed-whole-module", "commit_oid":args.commit,
            "tree_oid":snapshot.report().tree_oid, "module":args.module,
            "working_tree_evaluated":false, "original_git_history":false,
            "guest_output_authenticated":false, "approval":false,
            "inputs":captured.evidence, "stage":stage, "execution":report,
            "summary_sha256":summary_sha256}),
        artifacts: captured.artifacts.take(),
    })
}

fn finish_capture(result: Result<Completed>, cleanup: Result<()>) -> Result<Completed> {
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
    let source = Source::open(&args.root)?;
    let tree = source
        .commit_tree(&args.commit)
        .context("resolve explicit commit")?;
    let grants = Arc::new(Grants::load(&args.grants, &source)?);
    let limits = execution_limits(&args, &grants)?;
    let snapshot = Snapshot::create(
        &source,
        &tree,
        &grants
            .scratch_parent
            .join(crate::resource_id("equivalence-capture")?),
        SnapshotLimits::default(),
    )?;
    let result = evaluate(&args, &source, &snapshot, grants, limits, &interrupts);
    let mut completed = finish_capture(result, snapshot.discard())?;
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
        println!(
            "Commit {} · tree {} · whole module {}",
            args.commit, tree, args.module
        );
        print!("{}", slop_core::gate_report::render(&completed.summary)?);
    }
    Ok(u8::from(
        args.enforce && !completed.summary.blocking.is_empty(),
    ))
}

#[cfg(test)]
#[path = "equivalence_gate_tests.rs"]
mod tests;
