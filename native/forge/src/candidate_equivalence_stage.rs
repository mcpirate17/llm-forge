//! Confined equivalence Stage preparation. Target and configuration capture must
//! be performed by the review collector before calling this module.
//! The external binding assumes the operator-owned host parent is trusted:
//! a hostile same-UID host replacing the guest between verification and load
//! is outside the Stage filesystem boundary and is not authenticated.
use crate::{
    candidate_command::PreparedArgv,
    candidate_execution_plan::{self, Selection, Stage},
    candidate_execution_workspace::{Grants, Workspace},
    candidate_inputs::{self, BoundFile},
    confined_execution::Limits,
};
use anyhow::{ensure, Context, Result};
use cap_std::fs::{OpenOptions, OpenOptionsExt};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    io::Write,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

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
pub(crate) struct StageContext<'a> {
    pub source_root: &'a Path,
    pub capture_snapshot: &'a Path,
    pub data: Value,
}
pub(crate) struct AdmittedProbe<'a> {
    probe: Probe<'a>,
    guest_binding: BoundFile,
    python: PathBuf,
    targets: Vec<u8>,
    data: Value,
    grants_digest: String,
}
fn validate_probe(probe: &Probe<'_>, source_root: &Path, scratch: &Path) -> Result<()> {
    ensure!(
        probe.timeout_seconds.is_finite()
            && (0.001..=3600.0).contains(&probe.timeout_seconds)
            && (1..=32).contains(&probe.jobs),
        "invalid equivalence limits"
    );
    ensure!(
        !probe.guest_path.starts_with(scratch),
        "guest extension must be outside stage scratch"
    );
    ensure!(
        !probe.guest_path.starts_with(source_root),
        "guest extension must be outside source repository"
    );
    Ok(())
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
fn tools(grants: &Grants) -> Result<(&Path, &Path)> {
    let alias = grants
        .python
        .as_ref()
        .context("Python interpreter grant missing")?;
    let python = grants.programs.get(alias).context("Python grant missing")?;
    let forge = grants
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
    ensure!(
        check["check_id"] == "equivalence-probe",
        "incorrect check policy"
    );
    let limits = candidate_execution_plan::limits(check, &execution.grants)?;
    let source = execution.source()?;
    let context = StageContext {
        source_root: source.root(),
        capture_snapshot: review_snapshot,
        data: json!({"scope":"equivalence-probe", "candidate_tree":execution.candidate.tree_oid,
            "policy_sha256":execution.policy.digest, "grants_sha256":execution.grants.digest()}),
    };
    let admitted = admit_probe(&execution.grants, probe, context)?;
    let workspace = Workspace::new(&source, &execution.candidate.tree_oid, &execution.grants)?;
    prepare_workspace(workspace, &execution.grants, limits, admitted)
}

/// Hold and validate guest and target inputs before allocating a workspace.
pub(crate) fn admit_probe<'a>(
    grants: &Grants,
    probe: Probe<'a>,
    context: StageContext<'_>,
) -> Result<AdmittedProbe<'a>> {
    validate_probe(&probe, context.source_root, &grants.scratch_parent)?;
    let (python, _) = tools(grants)?;
    let (guest_binding, _bytes) = candidate_inputs::external(
        probe.guest_path,
        probe.guest_sha256,
        32 * 1024 * 1024,
        context.capture_snapshot,
    )?;
    let targets = serde_json::to_vec(probe.targets)?;
    ensure!(
        targets.len() <= 4 * 1024 * 1024,
        "equivalence targets exceed 4 MiB"
    );
    if let Some(waivers) = probe.waivers {
        ensure!(waivers.len() <= 1024 * 1024, "waivers exceed 1 MiB");
    }
    ensure!(
        grants
            .read_only
            .iter()
            .any(|root| probe.guest_path.starts_with(root)),
        "guest path lacks a read-only grant"
    );
    let mut data = context.data;
    let object = data
        .as_object_mut()
        .context("stage metadata must be an object")?;
    object.insert("guest_sha256".into(), json!(probe.guest_sha256));
    object.insert(
        "targets_sha256".into(),
        json!(format!("{:x}", Sha256::digest(&targets))),
    );
    object.insert("execution_verified".into(), json!(false));
    Ok(AdmittedProbe {
        probe,
        guest_binding,
        python: python.to_owned(),
        targets,
        data,
        grants_digest: grants.digest().to_owned(),
    })
}

/// Prepare the existing confined Stage from an independently admitted workspace.
pub(crate) fn prepare_workspace(
    workspace: Workspace,
    grants: &Grants,
    limits: Limits,
    admitted: AdmittedProbe<'_>,
) -> Result<Prepared> {
    let AdmittedProbe {
        probe,
        guest_binding,
        python,
        targets,
        data,
        grants_digest,
    } = admitted;
    ensure!(
        grants.digest() == grants_digest,
        "stage grants differ from admitted probe"
    );
    let root = workspace.artifact_root()?;
    write_new(&root, "equivalence-targets.json", &targets)?;
    if let Some(waivers) = probe.waivers {
        write_new(&root, "equivalence-waivers.json", waivers)?;
    }
    workspace.verify_artifact_root(&root)?;
    guest_binding.verify()?;
    let runtime = workspace.runtime();
    let argv = worker_args(
        &python,
        probe.guest_path,
        &runtime,
        probe.timeout_seconds,
        probe.jobs,
        probe.waivers.is_some(),
    )?;
    let plan = workspace.plan(
        &PreparedArgv {
            argv: argv.clone(),
            changed_files: None,
        },
        grants,
    )?;
    let stage = Stage {
        workspace,
        plan,
        limits,
        argv,
        data,
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
    grants: std::sync::Arc<Grants>,
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
        let grants = grants.clone();
        let budget = budget.clone();
        async move {
            stage
                .execute_batch_capture(
                    &grants,
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

#[cfg(test)]
#[path = "candidate_equivalence_stage_tests.rs"]
mod tests;
