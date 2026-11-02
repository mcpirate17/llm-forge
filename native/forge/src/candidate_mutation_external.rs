//! Sealed, owner-observed external-invariant measurement. The optional native
//! pytest hook samples mappings; it is not a historical loader attestation.
#[path = "candidate_mutation_external_coverage.rs"]
mod coverage;
use crate::{
    candidate_command::PreparedArgv,
    candidate_execution_plan::{self, Selection, Stage},
    candidate_execution_workspace::Workspace,
    candidate_inputs::{self, BoundFile},
    candidate_mutation_runner::Probe,
    candidate_policy::Policy,
    candidate_source::Candidate,
    confined_execution::Outcome as ProcessOutcome,
    process_signals::Interrupts,
};
use anyhow::{ensure, Context, Result};
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, os::unix::fs::MetadataExt, path::Path, sync::Arc};
#[derive(Clone, Debug, Serialize)]
pub(crate) struct Request {
    pub nodeid: String,
    pub pinned_torch: String,
    pub justification: String,
}
#[derive(Clone, Debug, Serialize)]
pub(crate) struct Outcome {
    pub nodeid: String,
    pub admitted: bool,
    pub reason: String,
}
pub(crate) struct Observation {
    candidate: String,
    policy: String,
    requests: String,
    probe: BoundFile,
    outcomes: Vec<Outcome>,
    evidence: Value,
}
impl Observation {
    pub(crate) fn verify_probe(&self, expected: &Probe) -> Result<()> {
        self.probe
            .verify_external_pin(&expected.path, &expected.sha256)
    }
    pub(crate) fn verify(
        &self,
        candidate: &Candidate,
        policy: &Policy,
        requests: &[Request],
    ) -> Result<()> {
        ensure!(
            crate::candidate_targeted_cargo::digest(candidate)? == self.candidate
                && policy.digest == self.policy
                && digest(requests)? == self.requests,
            "external-invariant observation binding changed"
        );
        self.probe.verify()
    }
    pub(crate) fn outcomes(&self) -> &[Outcome] {
        &self.outcomes
    }
    pub(crate) fn evidence(&self) -> &Value {
        &self.evidence
    }
}
fn digest<T: Serialize + ?Sized>(value: &T) -> Result<String> {
    let bytes = serde_json::to_vec(value)?;
    ensure!(
        bytes.len() <= 1024 * 1024,
        "invariant request serialization exceeds limit"
    );
    Ok(format!("{:x}", Sha256::digest(bytes)))
}
fn admit(requests: &[Request]) -> Result<()> {
    ensure!(
        !requests.is_empty() && requests.len() <= 128,
        "invariant request count outside limit"
    );
    let mut seen = BTreeSet::new();
    let mut bytes = 0usize;
    for request in requests {
        crate::candidate_mutation_external_cli::node_module(&request.nodeid)?;
        ensure!(seen.insert(&request.nodeid), "duplicate invariant nodeid");
        for value in [
            &request.nodeid,
            &request.pinned_torch,
            &request.justification,
        ] {
            bytes = bytes
                .checked_add(value.len())
                .context("invariant request bytes overflow")?;
        }
        ensure!(
            !request.nodeid.chars().any(char::is_control)
                && !request.pinned_torch.chars().any(char::is_control)
                && !request.justification.contains('\0'),
            "invariant request contains invalid controls"
        );
        ensure!(
            !request.pinned_torch.trim().is_empty()
                && request.pinned_torch.len() <= 256
                && !request.justification.trim().is_empty()
                && request.justification.len() <= 4096,
            "invariant pin or justification missing"
        );
    }
    ensure!(bytes <= 256 * 1024, "invariant request bytes exceed limit");
    Ok(())
}
fn tool(execution: &Selection, name: &str) -> Result<String> {
    Ok(execution
        .grants
        .programs
        .get(name)
        .with_context(|| format!("invariant {name} grant missing"))?
        .to_str()
        .context("invariant tool path not UTF-8")?
        .to_owned())
}
fn tools(execution: &Selection) -> Result<(String, String, String)> {
    require_proc(&execution.grants.read_only)?;
    let alias = execution
        .grants
        .python
        .as_ref()
        .context("invariant Python grant missing")?;
    let python = tool(execution, alias)?;
    let pytest = tool(execution, "pytest")?;
    let coverage = tool(execution, "coverage")?;
    let forge = execution
        .grants
        .programs
        .get("forge")
        .context("invariant Forge grant missing")?;
    let granted = std::fs::File::open(forge)?.metadata()?;
    let running = std::fs::File::open("/proc/self/exe")?.metadata()?;
    ensure!(
        granted.is_file() && granted.dev() == running.dev() && granted.ino() == running.ino(),
        "invariant Forge grant does not match running binary"
    );
    Ok((python, pytest, coverage))
}
fn require_proc(read_only: &[std::path::PathBuf]) -> Result<()> {
    ensure!(
        read_only
            .iter()
            .any(|path| path == std::path::Path::new("/proc")),
        "invariant observation requires an explicit read-only /proc grant"
    );
    Ok(())
}
fn tracked(execution: &Selection) -> Result<BTreeSet<String>> {
    let source = execution.source()?;
    let mut files = BTreeSet::new();
    let mut bytes = 0u64;
    for row in source.list_tree(&execution.candidate.tree_oid)? {
        if row.path.ends_with(".pyc") || row.path.ends_with(".pyo") {
            anyhow::bail!("tracked Python bytecode prevents invariant measurement");
        }
        if !row.path.ends_with(".py") {
            continue;
        }
        ensure!(
            row.object_type == "blob" && matches!(row.mode.as_str(), "100644" | "100755"),
            "invariant source must be a regular blob"
        );
        let size = row.size.context("invariant source size missing")?;
        ensure!(size <= 1024 * 1024, "invariant source exceeds 1 MiB");
        bytes = bytes
            .checked_add(size)
            .context("invariant source size overflow")?;
        ensure!(
            bytes <= 32 * 1024 * 1024 && files.len() < 4096,
            "invariant source inventory exceeds limit"
        );
        files.insert(row.path);
    }
    Ok(files)
}
fn argv(
    execution: &Selection,
    workspace: &Workspace,
    request: &Request,
    phase: &str,
    probe: &Probe,
    tools: &(String, String, String),
) -> Result<Vec<String>> {
    let source = execution.source()?;
    let argv = vec![
        "forge".into(),
        "candidate".into(),
        "mutation-invariant".into(),
        "--repo".into(),
        workspace
            .path()
            .to_str()
            .context("snapshot path not UTF-8")?
            .into(),
        "--source-root".into(),
        source
            .root()
            .to_str()
            .context("source path not UTF-8")?
            .into(),
        "--base".into(),
        execution.candidate.base_tree_oid.clone(),
        "--tree".into(),
        execution.candidate.tree_oid.clone(),
        "--python".into(),
        tools.0.clone(),
        "--pytest".into(),
        tools.1.clone(),
        "--coverage".into(),
        tools.2.clone(),
        format!("--nodeid={}", request.nodeid),
        format!("--torch-pin={}", request.pinned_torch),
        "--phase".into(),
        phase.into(),
        "--probe-sha256".into(),
        probe.sha256.clone(),
        "--probe-path".into(),
        probe.path.to_str().context("probe path not UTF-8")?.into(),
        "--execute".into(),
    ];
    ensure!(
        argv.iter().all(|v| v.len() <= 131071)
            && argv.iter().map(String::len).sum::<usize>() <= 1024 * 1024,
        "invariant argv exceeds limit"
    );
    Ok(argv)
}
#[allow(clippy::too_many_arguments)]
async fn phase(
    execution: &Selection,
    workspace: Workspace,
    request: &Request,
    phase: &str,
    probe: &Probe,
    tools: &(String, String, String),
    interrupts: &Interrupts,
    budget: Arc<crate::candidate_stage_artifact::ArtifactBudget>,
) -> Result<(Value, Option<crate::candidate_stage_artifact::Artifact>)> {
    ensure!(
        !interrupts.interrupted()?,
        "invariant measurement interrupted"
    );
    let argv = argv(execution, &workspace, request, phase, probe, tools)?;
    let command = workspace.plan(
        &PreparedArgv {
            argv: argv.clone(),
            changed_files: None,
        },
        &execution.grants,
    )?;
    let check = execution
        .checks()?
        .into_iter()
        .find(|c| c["check_id"] == "mutation-evidence")
        .context("mutation-evidence check inactive")?;
    let limits = candidate_execution_plan::limits(&check, &execution.grants)?;
    let snapshot = workspace
        .path()
        .to_str()
        .context("snapshot path not UTF-8")?
        .to_owned();
    let stage = Stage {
        workspace,
        plan: command,
        limits,
        argv,
        data: json!({"snapshot_root":snapshot,"phase":phase,"nodeid":request.nodeid,"probe_sha256":probe.sha256}),
    };
    let capture = crate::candidate_stage_artifact::Request {
        name: "invariant.json",
        maximum: 4 * 1024 * 1024,
        budget,
    };
    let (data, report, _, artifact) = stage
        .execute_capture(&execution.grants, interrupts, capture)
        .await?;
    ensure!(
        report.cleanup_complete && report.resources.verified && report.errors.is_empty(),
        "invariant Stage cleanup or resource evidence incomplete: {}",
        report.errors.join("; ")
    );
    if report.outcome != ProcessOutcome::Exited || report.exit_code != Some(0) {
        return Ok((data, None));
    }
    ensure!(
        report.signal.is_none(),
        "invariant Stage returned contradictory signal"
    );
    Ok((
        data,
        Some(artifact.context("successful invariant Stage missing artifact")?),
    ))
}
struct Measured {
    lines: BTreeSet<(String, u32)>,
    native: Vec<String>,
    artifact_sha: String,
}
fn decode(
    request: &Request,
    phase: &str,
    probe: &Probe,
    data: &Value,
    artifact: crate::candidate_stage_artifact::Artifact,
    source: &Path,
    tracked: &BTreeSet<String>,
) -> Result<Measured> {
    ensure!(
        format!("{:x}", Sha256::digest(&artifact.bytes)) == artifact.sha256,
        "invariant artifact hash changed"
    );
    let value = crate::tool_schema::parse_document_with_limit(
        std::str::from_utf8(&artifact.bytes)?,
        4 * 1024 * 1024,
    )?;
    ensure!(
        value["schema_version"] == 1
            && value["nodeid"] == request.nodeid
            && value["phase"] == phase
            && value["torch_version"] == request.pinned_torch
            && value["probe_sha256"] == probe.sha256,
        "invariant artifact binding mismatch"
    );
    let snapshot = Path::new(
        data["snapshot_root"]
            .as_str()
            .context("invariant snapshot missing")?,
    );
    ensure!(snapshot.is_absolute(), "invariant snapshot path invalid");
    Ok(Measured {
        lines: coverage::executed(&value["coverage"], snapshot, source, tracked)?,
        native: coverage::natives(&value["native"])?,
        artifact_sha: artifact.sha256,
    })
}
fn outcome(request: &Request, baseline: &Measured, node: &Measured) -> Outcome {
    let added = node.lines.difference(&baseline.lines).count();
    let natives = baseline.native.len() + node.native.len();
    let (admitted, reason) = if natives > 0 {
        (
            false,
            "repository native mapping or opaque compiler observed at a sampled checkpoint".into(),
        )
    } else if added > 0 {
        (
            false,
            format!("{added} production source line(s) executed beyond import baseline"),
        )
    } else {
        (
            true,
            format!(
                "no additional production lines or native mappings observed at sampled checkpoints; torch {}",
                request.pinned_torch
            ),
        )
    };
    Outcome {
        nodeid: request.nodeid.clone(),
        admitted,
        reason,
    }
}
/// A dropped waiter cancels its current owned Stage; cleanup remains with the
fn capture_probe(
    execution: &Selection,
    probe: &Probe,
    source_root: &Path,
    snapshot: &Path,
) -> Result<BoundFile> {
    let canonical = probe.path.canonicalize()?;
    ensure!(
        !canonical.starts_with(&execution.grants.scratch_parent),
        "invariant native probe is inside scratch parent"
    );
    ensure!(
        !canonical.starts_with(source_root),
        "invariant probe must be outside source"
    );
    let parent = canonical
        .parent()
        .context("invariant probe has no parent")?;
    ensure!(
        canonical
            .file_name()
            .is_some_and(|name| name == "conductor_native.so"),
        "invariant probe must be named conductor_native.so"
    );
    ensure!(
        execution
            .grants
            .read_only
            .iter()
            .any(|root| parent.starts_with(root)),
        "invariant probe directory lacks explicit read-only grant"
    );
    let (held, _) =
        candidate_inputs::external(&probe.path, &probe.sha256, 32 * 1024 * 1024, snapshot)?;
    Ok(held)
}

/// Stage actor. No caller-supplied measurement JSON enters this API.
pub(crate) async fn execute(
    execution: Arc<Selection>,
    requests: &[Request],
    probe: &Probe,
    interrupts: &Interrupts,
) -> Result<Observation> {
    admit(requests)?;
    let request_sha = digest(requests)?;
    let candidate = crate::candidate_targeted_cargo::digest(&execution.candidate)?;
    let tracked = tracked(&execution)?;
    for request in requests {
        ensure!(
            tracked.contains(crate::candidate_mutation_external_cli::node_module(
                &request.nodeid
            )?),
            "invariant node module not captured"
        );
    }
    let tool_paths = tools(&execution)?;
    let source = execution.source()?;
    let source_root = source.root().to_owned();
    let workspace = Workspace::new(&source, &execution.candidate.tree_oid, &execution.grants)?;
    let held = capture_probe(&execution, probe, &source_root, &workspace.path())?;
    let budget = Arc::new(crate::candidate_stage_artifact::ArtifactBudget::new(
        16 * 1024 * 1024,
    ));
    let mut first = Some(workspace);
    let mut outcomes = Vec::new();
    let mut phases = Vec::new();
    for request in requests {
        let mut pair = Vec::new();
        for label in ["baseline", "node"] {
            held.verify()?;
            let workspace = match first.take() {
                Some(v) => v,
                None => Workspace::new(&source, &execution.candidate.tree_oid, &execution.grants)?,
            };
            let (data, artifact) = phase(
                &execution,
                workspace,
                request,
                label,
                probe,
                &tool_paths,
                interrupts,
                budget.clone(),
            )
            .await?;
            held.verify()?;
            phases.push(json!({"nodeid":request.nodeid,"phase":label,"outcome":data["report"]["outcome"],"exit_code":data["report"]["exit_code"],"cleanup_complete":true,"resource_verified":true,"artifact_sha256":artifact.as_ref().map(|v|v.sha256.as_str())}));
            let Some(artifact) = artifact else {
                break;
            };
            pair.push(decode(
                request,
                label,
                probe,
                &data,
                artifact,
                &source_root,
                &tracked,
            )?);
        }
        if pair.len() != 2 {
            outcomes.push(Outcome {
                nodeid: request.nodeid.clone(),
                admitted: false,
                reason: "invariant measurement phase failed".into(),
            });
            continue;
        }
        let value = outcome(request, &pair[0], &pair[1]);
        phases.push(json!({"nodeid":request.nodeid,"phase":"comparison","baseline_sha256":pair[0].artifact_sha,"node_sha256":pair[1].artifact_sha,"baseline_lines":pair[0].lines.len(),"node_lines":pair[1].lines.len(),"baseline_native":pair[0].native.len(),"node_native":pair[1].native.len()}));
        outcomes.push(value);
    }
    held.verify()?;
    let evidence = json!({"scope":"sampled_native_mappings_not_historical_load_attestation","required_read_only_capability":"/proc","candidate_sha256":candidate,"tree":execution.candidate.tree_oid,"policy_sha256":execution.policy.digest,"grants_sha256":execution.grants.digest(),"requests_sha256":request_sha,"probe_sha256":probe.sha256,"phases":phases,"outcomes":outcomes});
    Ok(Observation {
        candidate,
        policy: execution.policy.digest.clone(),
        requests: request_sha,
        probe: held,
        outcomes,
        evidence,
    })
}
#[cfg(test)]
#[path = "candidate_mutation_external_tests.rs"]
mod tests;
