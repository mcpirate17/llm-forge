//! Bound selected legacy contracts in one supervised candidate-private Stage.
use crate::{
    candidate_command::PreparedArgv,
    candidate_execution_plan::{self, Selection as Execution, Stage},
    candidate_execution_workspace::Workspace,
    candidate_test_selection::Selection,
};
use anyhow::{ensure, Context, Result};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    os::unix::fs::MetadataExt,
    sync::Arc,
};
pub(crate) struct Plan {
    candidate: String,
    policy: String,
    grants: String,
    tree: String,
    id: String,
    python: String,
    python_libdir: Option<String>,
    slop: bool,
    targets: Vec<String>,
    bindings: Vec<(String, String)>,
}
fn python(execution: &Execution) -> Result<String> {
    let alias = execution
        .grants
        .python
        .as_ref()
        .context("contract Python grant missing")?;
    let path = execution
        .grants
        .programs
        .get(alias)
        .context("contract Python alias missing")?;
    let forge = execution
        .grants
        .programs
        .get("forge")
        .context("forge grant missing")?;
    let a = std::fs::File::open(forge)?.metadata()?;
    let b = std::fs::File::open("/proc/self/exe")?.metadata()?;
    ensure!(
        a.is_file() && a.dev() == b.dev() && a.ino() == b.ino(),
        "forge grant differs from running binary"
    );
    Ok(path.to_str().context("Python path not UTF-8")?.into())
}
fn python_libdir(execution: &Execution) -> Result<Option<String>> {
    execution
        .grants
        .environment
        .get("FORGE_CONTRACT_PYTHON_LIB_DIR")
        .map(|raw| {
            let path = std::path::Path::new(raw);
            ensure!(
                path.is_absolute()
                    && path.canonicalize()? == path
                    && path.is_dir()
                    && execution
                        .grants
                        .read_only
                        .iter()
                        .any(|grant| path.starts_with(grant)),
                "Python library directory lacks explicit read grant"
            );
            Ok(raw.clone())
        })
        .transpose()
}
pub(crate) fn plan(execution: &Execution, selected: &Selection<'_>, id: &str) -> Result<Plan> {
    ensure!(
        matches!(id, "targeted-tests" | "targeted-tests-full"),
        "invalid contract check"
    );
    let candidate = crate::candidate_targeted_cargo::check_binding(execution, selected)?;
    ensure!(
        execution.checks()?.iter().any(|c| c["check_id"] == id),
        "contract check inactive"
    );
    let targets = crate::candidate_contract_run::targets(&selected.contract_targets)?;
    let expected: BTreeSet<_> = targets
        .iter()
        .map(|t| format!("native/conductor-native/tests/{t}.rs"))
        .collect();
    ensure!(
        expected == selected.contract_test_paths.iter().cloned().collect(),
        "contract target paths differ from selection"
    );
    let source = execution.source()?;
    let entries: BTreeMap<_, _> = source
        .list_tree(&execution.candidate.tree_oid)?
        .into_iter()
        .map(|v| (v.path.clone(), v))
        .collect();
    let mut paths = expected;
    paths.insert("native/conductor-native/Cargo.toml".into());
    paths.insert("native/forge/Cargo.toml".into());
    if entries.contains_key("native/slop-core/Cargo.toml") {
        paths.insert("native/slop-core/Cargo.toml".into());
    }
    paths.extend(
        execution
            .candidate
            .changes
            .iter()
            .filter(|c| !c.deleted() && c.classes.iter().any(|v| v == "native" || v == "python"))
            .map(|c| c.path.clone()),
    );
    ensure!(paths.len() <= 4095, "contract binding count exceeds limit");
    let mut bytes = 0usize;
    let mut bindings = Vec::new();
    for path in paths {
        crate::candidate_source::safe_path(path.as_bytes())?;
        bytes = bytes
            .checked_add(path.len())
            .context("contract path overflow")?;
        ensure!(bytes <= 8 * 1024 * 1024, "contract paths exceed limit");
        let entry = entries
            .get(&path)
            .context("contract source absent from tree")?;
        ensure!(
            entry.object_type == "blob"
                && matches!(entry.mode.as_str(), "100644" | "100755")
                && entry.size.is_some_and(|v| v <= 1024 * 1024),
            "contract input must be bounded regular blob"
        );
        bindings.push((path, entry.oid.clone()));
    }
    let python = python(execution)?;
    let workspace = Workspace::new(&source, &execution.candidate.tree_oid, &execution.grants)?;
    let slop = crate::candidate_contract_run::slop_needed(
        &workspace.path(),
        &workspace.runtime(),
        std::path::Path::new(&python),
        &targets,
    )?;
    workspace.discard()?;
    Ok(Plan {
        candidate,
        policy: execution.policy.digest.clone(),
        grants: execution.grants.digest().into(),
        tree: execution.candidate.tree_oid.clone(),
        id: id.into(),
        python,
        python_libdir: python_libdir(execution)?,
        slop,
        targets,
        bindings,
    })
}
#[derive(Clone)]
pub(crate) struct VersionPermit {
    candidate: String,
    grants: String,
    policy: String,
    version: String,
}
pub(crate) fn stage_owned(
    execution: &Execution,
    plan: &Plan,
    version: bool,
    permit: Option<&VersionPermit>,
) -> Result<Stage> {
    ensure!(
        crate::candidate_targeted_cargo::digest(&execution.candidate)? == plan.candidate
            && execution.policy.digest == plan.policy
            && execution.grants.digest() == plan.grants
            && execution.candidate.tree_oid == plan.tree
            && python(execution)? == plan.python
            && python_libdir(execution)? == plan.python_libdir,
        "contract plan binding changed"
    );
    if !version {
        let p = permit.context("contract version observation required")?;
        ensure!(
            p.candidate == plan.candidate && p.policy == plan.policy && p.grants == plan.grants,
            "contract version permit changed"
        );
    }
    let check = execution
        .checks()?
        .into_iter()
        .find(|c| c["check_id"] == plan.id)
        .context("contract policy inactive")?;
    let source = execution.source()?;
    let workspace = Workspace::new(&source, &plan.tree, &execution.grants)?;
    let mut argv = vec![
        "forge".into(),
        "candidate".into(),
        "contract-test".into(),
        "--repo".into(),
        workspace
            .path()
            .to_str()
            .context("snapshot not UTF-8")?
            .into(),
        "--base".into(),
        execution.candidate.base_tree_oid.clone(),
        "--tree".into(),
        plan.tree.clone(),
        "--python".into(),
        plan.python.clone(),
    ];
    argv.push(if version {
        "--tool-version".into()
    } else {
        "--execute".into()
    });
    for target in &plan.targets {
        argv.push(format!("--target={target}"));
    }
    for (path, _) in &plan.bindings {
        if path != "native/conductor-native/Cargo.toml" {
            argv.push(format!("--bind={path}"));
        }
    }
    let size = argv
        .iter()
        .try_fold(0usize, |n, s| n.checked_add(s.len()))
        .context("contract argv overflow")?;
    ensure!(
        argv.len() <= 8192 && size <= 1024 * 1024 && argv.iter().all(|s| s.len() <= 131071),
        "contract argv exceeds bounds"
    );
    let command = workspace.plan(
        &PreparedArgv {
            argv: argv.clone(),
            changed_files: None,
        },
        &execution.grants,
    )?;
    let mut limits = candidate_execution_plan::limits(&check, &execution.grants)?;
    if version {
        limits.timeout = limits.timeout.min(std::time::Duration::from_secs(30));
        limits.cpu_seconds = limits.cpu_seconds.map(|n| n.min(30));
    }
    Ok(Stage {
        workspace,
        plan: command,
        limits,
        argv,
        data: json!({"scope":"candidate_contract_runtime","candidate":plan.candidate,"policy":plan.policy,"grants":plan.grants,"tree":plan.tree,"python":plan.python,"python_libdir":plan.python_libdir,"slop":plan.slop,"targets":plan.targets,"bindings":plan.bindings,"version":version}),
    })
}
pub(crate) struct Observation {
    data: Value,
    report: crate::confined_execution::Report,
    artifact: Option<crate::candidate_stage_artifact::Artifact>,
}
fn verify(plan: &Plan, row: &Observation) -> Result<bool> {
    let d = &row.data;
    ensure!(
        d["candidate"] == plan.candidate
            && d["policy"] == plan.policy
            && d["grants"] == plan.grants
            && d["tree"] == plan.tree
            && d["python"] == plan.python
            && d["python_libdir"] == serde_json::to_value(&plan.python_libdir)?
            && d["slop"] == plan.slop
            && d["targets"] == serde_json::to_value(&plan.targets)?
            && d["bindings"] == serde_json::to_value(&plan.bindings)?,
        "contract observation binding changed"
    );
    d["version"]
        .as_bool()
        .context("contract observation role missing")
}
pub(crate) async fn run_stage(
    stage: Stage,
    execution: Arc<Execution>,
    mask: crate::process_signals::ChildSignalMask,
    cancel: Arc<crate::confined_execution::Cancellation>,
    owners: Arc<crate::batch_execution::Owners>,
    budget: Arc<crate::candidate_stage_artifact::ArtifactBudget>,
) -> Result<Observation> {
    let version = stage.data["version"]
        .as_bool()
        .context("contract role missing")?;
    if version {
        let (data, report, _) = stage
            .execute_batch(&execution.grants, mask, cancel, owners)
            .await?;
        Ok(Observation {
            data,
            report,
            artifact: None,
        })
    } else {
        let request = crate::candidate_stage_artifact::Request {
            name: "contract-evidence.json",
            maximum: 1024 * 1024,
            budget,
        };
        let (data, report, _, artifact) = stage
            .execute_batch_capture(&execution.grants, mask, cancel, owners, request)
            .await?;
        Ok(Observation {
            data,
            report,
            artifact,
        })
    }
}
pub(crate) fn version_permit(plan: &Plan, row: &Observation) -> Result<VersionPermit> {
    ensure!(
        verify(plan, row)? && row.artifact.is_none(),
        "contract version observation required"
    );
    let version = crate::candidate_execution_result::version(&row.report, 100_000)?;
    ensure!(
        matches!(
            version,
            crate::candidate_execution_result::VersionEvidence::Available(_)
        ),
        "contract version stage failed"
    );
    ensure!(
        row.report.stdout.lines().any(|l| l.starts_with("cargo "))
            && row
                .report
                .stdout
                .lines()
                .any(|l| l.starts_with("Python 3.")),
        "contract Cargo/Python versions missing"
    );
    Ok(VersionPermit {
        candidate: plan.candidate.clone(),
        grants: plan.grants.clone(),
        policy: plan.policy.clone(),
        version: row.report.stdout.clone(),
    })
}
fn artifact(plan: &Plan, row: &Observation) -> Result<Value> {
    use sha2::{Digest, Sha256};
    let artifact = row
        .artifact
        .as_ref()
        .context("successful contract stage missing evidence")?;
    ensure!(
        format!("{:x}", Sha256::digest(&artifact.bytes)) == artifact.sha256,
        "contract evidence digest changed"
    );
    let text = std::str::from_utf8(&artifact.bytes).context("contract evidence not UTF-8")?;
    let value = crate::tool_schema::parse_document_with_limit(text, 1024 * 1024)?;
    ensure!(
        value["scope"] == "candidate_built_contract_runtime"
            && value["python"] == plan.python
            && value["slop_candidate_built"] == plan.slop
            && value["slop_scope"] == "legacy_target_mapping_not_runtime_import_attestation"
            && value["targets"] == serde_json::to_value(&plan.targets)?,
        "contract runtime evidence differs from plan"
    );
    let records = value["artifacts"]
        .as_array()
        .context("contract artifacts missing")?;
    let mut expected: BTreeSet<&str> = ["conductor_native", "forge"].into_iter().collect();
    if plan.slop {
        expected.insert("slop_core");
    }
    let mut seen = BTreeSet::new();
    for record in records {
        let object = record.as_object().context("invalid contract artifact")?;
        ensure!(object.len() == 3, "invalid contract artifact fields");
        let name = record["name"]
            .as_str()
            .context("contract artifact name missing")?;
        let hash = record["sha256"]
            .as_str()
            .context("contract artifact digest missing")?;
        ensure!(
            expected.contains(name)
                && seen.insert(name)
                && hash.len() == 64
                && hash
                    .bytes()
                    .all(|v| v.is_ascii_digit() || (b'a'..=b'f').contains(&v))
                && record["bytes"]
                    .as_u64()
                    .is_some_and(|n| n > 0 && n <= 256 * 1024 * 1024),
            "invalid contract artifact provenance"
        );
    }
    ensure!(seen == expected, "required contract artifact missing");
    Ok(json!({"sha256":artifact.sha256,"runtime":value}))
}
pub(crate) fn finish(
    execution: &Execution,
    selected: &Selection<'_>,
    plan: &Plan,
    permit: &VersionPermit,
    row: Observation,
) -> Result<(crate::candidate_assessment::CheckResult, Value)> {
    ensure!(
        crate::candidate_targeted_cargo::check_binding(execution, selected)? == plan.candidate
            && execution.policy.digest == plan.policy
            && execution.grants.digest() == plan.grants
            && execution.candidate.tree_oid == plan.tree,
        "contract completion binding changed"
    );
    ensure!(
        permit.candidate == plan.candidate
            && permit.policy == plan.policy
            && permit.grants == plan.grants,
        "contract permit changed"
    );
    ensure!(
        !verify(plan, &row)?,
        "contract execution observation required"
    );
    let problem = crate::candidate_targeted_cargo::observation_problem(&plan.id, &row.report);
    let evidence = if problem.is_none() {
        Some(artifact(plan, &row)?)
    } else {
        ensure!(
            row.artifact.is_none(),
            "failed contract stage returned accepted evidence"
        );
        None
    };
    let status = match &problem {
        None => crate::candidate_assessment::CheckStatus::Passed,
        Some(v) if v.rule_id == "targeted-test-failed" => {
            crate::candidate_assessment::CheckStatus::Failed
        }
        Some(_) => crate::candidate_assessment::CheckStatus::Error,
    };
    let result = crate::candidate_assessment::CheckResult {
        check_id: plan.id.clone(),
        status,
        duration_ms: row.report.duration_ms,
        findings: problem.into_iter().collect(),
        files: plan.bindings.iter().map(|v| v.0.clone()).collect(),
        command: Vec::new(),
        tool_version: Some(permit.version.clone()),
        cache_key: None,
        cache_hit: false,
        exit_code: row.report.exit_code.map(i64::from),
        skipped_reason: None,
        stdout_tail: row.report.stdout,
        stderr_tail: row.report.stderr,
        metrics: serde_json::Map::new(),
    };
    Ok((
        result,
        json!({"scope":"candidate_built_contract_runtime","tree":plan.tree,"candidate_sha256":plan.candidate,"policy_sha256":plan.policy,"grants_sha256":plan.grants,"python_libdir":plan.python_libdir,"targets":plan.targets,"bindings":plan.bindings,"evidence":evidence,"review_complete":false}),
    ))
}
#[cfg(test)]
#[path = "candidate_targeted_contract_tests.rs"]
mod tests;
