//! Confined equivalence Stage preparation. Target and configuration capture must
//! be performed by the review collector before calling this module.
//! The external binding assumes the operator-owned host parent is trusted:
//! a hostile same-UID host replacing the guest between verification and load
//! is outside the Stage filesystem boundary and is not authenticated.
use crate::{
    candidate_command::PreparedArgv,
    candidate_execution_plan::{self, Selection, Stage},
    candidate_execution_workspace::Workspace,
    candidate_inputs::{self, BoundFile},
};
use anyhow::{ensure, Context, Result};
use cap_std::fs::{OpenOptions, OpenOptionsExt};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{io::Write, os::unix::fs::MetadataExt, path::Path};

pub(crate) struct Prepared {
    pub stage: Stage,
    pub guest_binding: BoundFile,
}
pub(crate) struct Probe<'a> {
    pub guest_path: &'a Path,
    pub guest_sha256: &'a str,
    pub targets: &'a [slop_core::gate_types::Target],
    pub waivers: Option<&'a [u8]>,
    pub timeout_seconds: f64,
    pub jobs: usize,
}
fn write_new(dir: &cap_std::fs::Dir, name: &str, data: &[u8]) -> Result<()> {
    let mut file = dir.open_with(
        name,
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK),
    )?;
    file.write_all(data)?;
    file.sync_all()?;
    Ok(())
}
fn tools(execution: &Selection) -> Result<(&Path, &Path)> {
    let alias = execution
        .grants
        .python
        .as_ref()
        .context("Python interpreter grant missing")?;
    let python = execution
        .grants
        .programs
        .get(alias)
        .context("Python grant missing")?;
    let forge = execution
        .grants
        .programs
        .get("forge")
        .context("forge grant missing")?;
    let expected = std::fs::File::open(forge)?.metadata()?;
    let actual = std::fs::File::open("/proc/self/exe")?.metadata()?;
    ensure!(
        expected.is_file() && expected.dev() == actual.dev() && expected.ino() == actual.ino(),
        "forge grant differs from running executable"
    );
    Ok((python, forge))
}
/// Holds a verified external extension and stages native targets in a fresh tree.
/// `check` is the captured normalized policy for equivalence-probe.
pub(crate) fn prepare(
    execution: &Selection,
    review_snapshot: &Path,
    check: &Value,
    probe: Probe<'_>,
) -> Result<Prepared> {
    let Probe {
        guest_path,
        guest_sha256,
        targets,
        waivers,
        timeout_seconds,
        jobs,
    } = probe;
    ensure!(
        check["check_id"] == "equivalence-probe",
        "incorrect check policy"
    );
    ensure!(
        timeout_seconds.is_finite()
            && (0.001..=3600.0).contains(&timeout_seconds)
            && (1..=32).contains(&jobs),
        "invalid equivalence limits"
    );
    let (python, _) = tools(execution)?;
    ensure!(
        !guest_path.starts_with(&execution.grants.scratch_parent),
        "guest extension must be outside stage scratch"
    );
    ensure!(
        !guest_path.starts_with(execution.source()?.root()),
        "guest extension must be outside source repository"
    );
    let (guest_binding, _bytes) =
        candidate_inputs::external(guest_path, guest_sha256, 32 * 1024 * 1024, review_snapshot)?;
    let targets = serde_json::to_vec(targets)?;
    ensure!(
        targets.len() <= 4 * 1024 * 1024,
        "equivalence targets exceed 4 MiB"
    );
    let targets_sha256 = format!("{:x}", Sha256::digest(&targets));
    let source = execution.source()?;
    let workspace = Workspace::new(&source, &execution.candidate.tree_oid, &execution.grants)?;
    let root = workspace.artifact_root()?;
    write_new(&root, "equivalence-targets.json", &targets)?;
    if let Some(waivers) = waivers {
        ensure!(waivers.len() <= 1024 * 1024, "waivers exceed 1 MiB");
        write_new(&root, "equivalence-waivers.json", waivers)?;
    }
    ensure!(
        execution
            .grants
            .read_only
            .iter()
            .any(|root| guest_path.starts_with(root)),
        "guest path lacks a read-only grant"
    );
    workspace.verify_artifact_root(&root)?;
    guest_binding.verify()?;
    let runtime = workspace.runtime();
    let argv = worker_args(
        python,
        guest_path,
        &runtime,
        timeout_seconds,
        jobs,
        waivers.is_some(),
    )?;
    let plan = workspace.plan(
        &PreparedArgv {
            argv: argv.clone(),
            changed_files: None,
        },
        &execution.grants,
    )?;
    let limits = candidate_execution_plan::limits(check, &execution.grants)?;
    let stage = Stage {
        workspace,
        plan,
        limits,
        argv,
        data: json!({"scope":"equivalence-probe", "candidate_tree":execution.candidate.tree_oid,
        "policy_sha256":execution.policy.digest, "grants_sha256":execution.grants.digest(),
        "guest_sha256":guest_sha256,"targets_sha256":targets_sha256,"execution_verified":false}),
    };
    Ok(Prepared {
        stage,
        guest_binding,
    })
}

fn worker_args(
    python: &Path,
    guest_path: &Path,
    runtime: &Path,
    timeout_seconds: f64,
    jobs: usize,
    has_waivers: bool,
) -> Result<Vec<String>> {
    let mut argv = vec![
        "forge".into(),
        "equivalence-worker".into(),
        "--python".into(),
        python.to_str().context("interpreter is not UTF-8")?.into(),
        "--guest-path".into(),
        guest_path
            .to_str()
            .context("guest path is not UTF-8")?
            .into(),
        "--targets".into(),
        runtime
            .join("equivalence-targets.json")
            .to_str()
            .context("runtime is not UTF-8")?
            .into(),
        "--output".into(),
        runtime
            .join("equivalence-summary.json")
            .to_str()
            .context("runtime is not UTF-8")?
            .into(),
        "--timeout-seconds".into(),
        timeout_seconds.to_string(),
        "--jobs".into(),
        jobs.to_string(),
    ];
    if has_waivers {
        argv.push("--waivers".into());
        argv.push(
            runtime
                .join("equivalence-waivers.json")
                .to_str()
                .context("runtime is not UTF-8")?
                .into(),
        );
    }
    Ok(argv)
}

/// Run exactly one prepared Stage through the standard batch owner and capture path.
pub(crate) async fn execute(
    prepared: Prepared,
    execution: std::sync::Arc<Selection>,
    interrupts: &crate::process_signals::Interrupts,
) -> Result<(Value, crate::confined_execution::Report, Vec<u8>, String)> {
    use crate::{
        batch_execution,
        candidate_stage_artifact::{ArtifactBudget, Request},
        confined_execution::Outcome,
    };
    use std::sync::Arc;
    let Prepared {
        stage,
        guest_binding,
        ..
    } = prepared;
    let signal = interrupts.register_reactor()?;
    ensure!(
        !interrupts.interrupted()?,
        "equivalence interrupted before launch"
    );
    let mask = interrupts.child_mask();
    let budget = Arc::new(ArtifactBudget::new(4 * 1024 * 1024));
    let batch = batch_execution::start(vec![stage], 1, move |stage, cancel, owners| {
        let execution = execution.clone();
        let budget = budget.clone();
        async move {
            stage
                .execute_batch_capture(
                    &execution.grants,
                    mask,
                    cancel,
                    owners,
                    Request {
                        name: "equivalence-summary.json",
                        maximum: 4 * 1024 * 1024,
                        budget,
                    },
                )
                .await
        }
    })?;
    let cancel = batch.cancellation();
    let wait = batch.wait();
    tokio::pin!(wait);
    let outcomes = tokio::select! {
        rows = &mut wait => rows?,
        received = async {
            let mut bytes = [0u8; std::mem::size_of::<libc::signalfd_siginfo>()];
            ensure!(signal.read(&mut bytes).await? > 0, "interrupt descriptor closed");
            Ok::<_, anyhow::Error>(())
        } => { let wake = cancel.cancel(); let rows = wait.await?; received?; wake?; rows }
    };
    guest_binding.verify()?;
    ensure!(
        !cancel.is_cancelled() && !interrupts.interrupted()?,
        "equivalence interrupted after cleanup"
    );
    let (data, report, _, artifact) = outcomes
        .into_iter()
        .next()
        .context("missing Stage result")??;
    ensure!(
        report.outcome == Outcome::Exited
            && report.exit_code == Some(0)
            && report.cleanup_complete
            && report.resources.verified
            && report.errors.is_empty(),
        "equivalence Stage did not complete with verified cleanup/resources"
    );
    let artifact = artifact.context("equivalence Stage did not produce summary")?;
    Ok((data, report, artifact.bytes, artifact.sha256))
}
