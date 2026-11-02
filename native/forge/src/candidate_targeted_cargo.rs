//! Supervised Cargo subset for future targeted-test coordination.
//! Plans bind a held selection; execution is owned by existing Stage/batch APIs.
//! Zero exit is not proof that selected tests were discovered or covered.
use crate::{
    batch_execution::Owners,
    candidate_assessment::{self, CheckResult, CheckStatus, Finding},
    candidate_command::PreparedArgv,
    candidate_execution_plan::{self, Selection as ExecutionSelection, Stage},
    candidate_execution_workspace::Workspace,
    candidate_source::{Candidate, TreeEntry},
    candidate_test_selection::Selection as TestSelection,
    confined_execution::{Cancellation, Outcome, Report},
    process_signals::ChildSignalMask,
};
use anyhow::{ensure, Context, Result};
use serde::Serialize;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Write,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Clone, Debug, Serialize)]
pub(crate) struct BoundPath {
    pub(crate) path: String,
    pub(crate) oid: String,
}
#[derive(Clone, Debug, Serialize)]
pub(crate) struct Job {
    pub(crate) manifest: String,
    manifest_oid: String,
    pub(crate) files: Vec<BoundPath>,
}
#[derive(Debug, Serialize)]
pub(crate) struct Plan {
    check_id: String,
    capture_chars: usize,
    tree: String,
    policy_sha256: String,
    grants_sha256: String,
    candidate_sha256: String,
    jobs: Vec<Job>,
    python_pending: bool,
    contracts_pending: bool,
    coverage_required: bool,
    coverage: Option<crate::candidate_targeted_rust_coverage::Plan>,
}
impl Plan {
    pub(crate) fn jobs(&self) -> &[Job] {
        &self.jobs
    }
}
pub(crate) fn digest(value: &impl Serialize) -> Result<String> {
    struct Sink {
        hash: Sha256,
        len: usize,
    }
    impl Write for Sink {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.len = self
                .len
                .checked_add(bytes.len())
                .filter(|n| *n <= 32 * 1024 * 1024)
                .ok_or_else(|| std::io::Error::other("candidate binding exceeds 32 MiB"))?;
            self.hash.update(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut sink = Sink {
        hash: Sha256::new(),
        len: 0,
    };
    serde_json::to_writer(&mut sink, value)?;
    Ok(format!("{:x}", sink.hash.finalize()))
}
pub(crate) fn check_binding(
    execution: &ExecutionSelection,
    selected: &TestSelection<'_>,
) -> Result<String> {
    selected.verify()?;
    ensure!(
        selected.result.status == CheckStatus::Passed,
        "test selection is not complete enough to execute"
    );
    ensure!(
        selected.evidence["candidate_tree"] == execution.candidate.tree_oid
            && selected.evidence["policy_path"] == execution.policy.path
            && selected.evidence["policy_sha256"] == execution.policy.digest,
        "test selection candidate/policy binding differs"
    );
    let actual = digest(&execution.candidate)?;
    ensure!(
        selected.evidence["supplied_candidate_sha256"] == actual,
        "test selection candidate bytes differ"
    );
    Ok(actual)
}
fn owner(entries: &BTreeMap<String, TreeEntry>, path: &str) -> Option<String> {
    let mut parent = Path::new(path).parent();
    while let Some(directory) = parent {
        let root = directory.to_str()?;
        let name = if root.is_empty() {
            "Cargo.toml".to_owned()
        } else {
            format!("{root}/Cargo.toml")
        };
        if entries.contains_key(&name) {
            return Some(name);
        }
        parent = directory.parent();
    }
    None
}
fn entry(entries: &BTreeMap<String, TreeEntry>, path: &str) -> Result<BoundPath> {
    crate::candidate_source::safe_path(path.as_bytes())?;
    ensure!(
        !path.chars().any(char::is_control),
        "Cargo test path contains controls"
    );
    let item = entries
        .get(path)
        .with_context(|| format!("Cargo test path absent from captured tree: {path}"))?;
    ensure!(
        item.object_type == "blob"
            && matches!(item.mode.as_str(), "100644" | "100755")
            && item.size.is_some_and(|size| size <= 1024 * 1024),
        "Cargo test path must be a bounded regular blob: {path}"
    );
    Ok(BoundPath {
        path: path.into(),
        oid: item.oid.clone(),
    })
}
fn prepare_jobs(
    entries: &BTreeMap<String, TreeEntry>,
    candidate: &Candidate,
    native: &Map<String, Value>,
) -> Result<Vec<Job>> {
    ensure!(native.len() <= 4096, "Cargo test source count exceeds 4096");
    let changed: BTreeSet<_> = candidate
        .changes
        .iter()
        .filter(|change| !change.deleted())
        .map(|change| change.path.as_str())
        .collect();
    let mut groups: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut bytes = 0usize;
    for (source, tests) in native {
        ensure!(
            changed.contains(source.as_str()),
            "Cargo source is not a live captured change"
        );
        let manifest = owner(entries, source).context("Cargo source has no captured manifest")?;
        let values = tests
            .as_array()
            .filter(|values| !values.is_empty())
            .context("Cargo test evidence must be nonempty array")?;
        ensure!(
            values.len() <= 4096,
            "Cargo test evidence count exceeds 4096"
        );
        let paths = groups.entry(manifest.clone()).or_default();
        paths.insert(source.clone());
        for value in values {
            let test = value
                .as_str()
                .context("Cargo test evidence path must be string")?;
            ensure!(
                test.ends_with(".rs") && owner(entries, test).as_deref() == Some(manifest.as_str()),
                "Cargo test evidence belongs to a different crate"
            );
            bytes = bytes
                .checked_add(test.len() + 16)
                .context("Cargo test plan size overflow")?;
            ensure!(bytes <= 8 * 1024 * 1024, "Cargo test plan exceeds 8 MiB");
            paths.insert(test.into());
        }
    }
    ensure!(groups.len() <= 256, "Cargo test crate count exceeds 256");
    groups
        .into_iter()
        .map(|(manifest, paths)| {
            let manifest_oid = entry(entries, &manifest)?.oid;
            let files = paths
                .into_iter()
                .map(|path| entry(entries, &path))
                .collect::<Result<Vec<_>>>()?;
            ensure!(files.len() <= 4095, "Cargo job has too many bound inputs");
            Ok(Job {
                manifest,
                manifest_oid,
                files,
            })
        })
        .collect()
}
pub(crate) fn plan(
    execution: &ExecutionSelection,
    selected: &TestSelection<'_>,
    check_id: &str,
) -> Result<Plan> {
    ensure!(
        matches!(check_id, "targeted-tests" | "targeted-tests-full"),
        "unsupported targeted test check"
    );
    let active = execution
        .checks()?
        .into_iter()
        .find(|check| check["check_id"] == check_id)
        .context("targeted test check is not active in this policy profile")?;
    let capture_chars = usize::try_from(
        active["max_output_chars"]
            .as_u64()
            .context("targeted output bound missing")?,
    )?;
    ensure!(
        (1..=crate::process_capture_tail::MAX_OUTPUT_CHARS).contains(&capture_chars),
        "targeted output bound invalid"
    );
    let candidate_sha256 = check_binding(execution, selected)?;
    let source = execution.source()?;
    let entries = source
        .list_tree(&execution.candidate.tree_oid)?
        .into_iter()
        .map(|entry| (entry.path.clone(), entry))
        .collect();
    let jobs = prepare_jobs(&entries, &execution.candidate, &selected.native_tests)?;
    let coverage = if check_id == "targeted-tests-full" {
        Some(crate::candidate_targeted_rust_coverage::plan(
            execution, &jobs,
        )?)
    } else {
        None
    };
    Ok(Plan {
        check_id: check_id.into(),
        capture_chars,
        tree: execution.candidate.tree_oid.clone(),
        policy_sha256: execution.policy.digest.clone(),
        grants_sha256: execution.grants.digest().to_owned(),
        candidate_sha256,
        jobs,
        python_pending: !selected.python_tests.is_empty(),
        contracts_pending: !selected.contract_targets.is_empty(),
        coverage_required: check_id == "targeted-tests-full",
        coverage,
    })
}
fn same_cli(execution: &ExecutionSelection) -> Result<()> {
    let program = granted_program(&execution.grants.programs)?;
    ensure!(
        execution
            .grants
            .environment
            .get("FORGE_CARGO_BIN")
            .is_some_and(|value| !value.is_empty()),
        "targeted Cargo tests require an explicit FORGE_CARGO_BIN grant"
    );
    let granted = std::fs::File::open(program)?.metadata()?;
    let running = std::fs::File::open("/proc/self/exe")?.metadata()?;
    ensure!(
        granted.is_file() && granted.dev() == running.dev() && granted.ino() == running.ino(),
        "granted forge alias does not name this running executable"
    );
    Ok(())
}
fn granted_program(programs: &BTreeMap<String, PathBuf>) -> Result<&Path> {
    Ok(programs
        .get("forge")
        .context("targeted Cargo tests require the operator-granted forge alias")?
        .as_path())
}
pub(crate) fn stage_owned(
    execution: &ExecutionSelection,
    plan: &Plan,
    index: usize,
    version: bool,
    permit: Option<&VersionPermit>,
    check: &Value,
) -> Result<Stage> {
    ensure!(
        digest(&execution.candidate)? == plan.candidate_sha256
            && execution.candidate.tree_oid == plan.tree
            && execution.policy.digest == plan.policy_sha256
            && execution.grants.digest() == plan.grants_sha256,
        "Cargo plan binding changed"
    );
    let active = execution
        .checks()?
        .into_iter()
        .find(|candidate| candidate["check_id"] == plan.check_id)
        .context("targeted test check no longer active")?;
    ensure!(
        &active == check,
        "Cargo stage limits differ from the captured policy check"
    );
    same_cli(execution)?;
    let job = plan
        .jobs
        .get(index)
        .context("Cargo job index outside plan")?;
    if version {
        ensure!(permit.is_none(), "version stage must not accept a permit");
    } else {
        let permit =
            permit.context("Cargo test stage requires a successful version observation")?;
        ensure!(
            permit.index == index
                && permit.candidate_sha256 == plan.candidate_sha256
                && permit.policy_sha256 == plan.policy_sha256,
            "Cargo version permit differs from plan"
        );
    }
    let source = execution.source()?;
    let workspace = Workspace::new(&source, &plan.tree, &execution.grants)?;
    let argv = stage_argv(execution, plan, job, &workspace.path(), version)?;
    let prepared = PreparedArgv {
        argv: argv.clone(),
        changed_files: None,
    };
    let command = workspace.plan(&prepared, &execution.grants)?;
    let mut limits = candidate_execution_plan::limits(check, &execution.grants)?;
    if version {
        limits.timeout = limits.timeout.min(std::time::Duration::from_secs(30));
        limits.cpu_seconds = limits.cpu_seconds.map(|cpu| cpu.min(30));
    }
    let root = workspace
        .path()
        .to_str()
        .context("snapshot root not UTF-8")?
        .to_owned();
    Ok(Stage {
        workspace,
        plan: command,
        limits,
        argv,
        data: json!({"scope":"captured_cargo_package", "tree":plan.tree, "candidate_sha256":plan.candidate_sha256,"policy_sha256":plan.policy_sha256,"grants_sha256":plan.grants_sha256,"manifest":job.manifest, "manifest_oid":job.manifest_oid, "bindings":job.files, "version":version,"cargo_version":permit.map(|item|item.version.as_str()),"coverage":plan.coverage_required,"snapshot_root":root}),
    })
}
fn stage_argv(
    execution: &ExecutionSelection,
    plan: &Plan,
    job: &Job,
    snapshot: &Path,
    version: bool,
) -> Result<Vec<String>> {
    let mut argv = vec![
        "forge".into(),
        "candidate".into(),
        "cargo-test".into(),
        "--repo".into(),
        snapshot
            .to_str()
            .context("snapshot path must be UTF-8")?
            .into(),
        "--base".into(),
        execution.candidate.base_tree_oid.clone(),
        "--tree".into(),
        plan.tree.clone(),
        format!("--manifest={}", job.manifest),
        if version {
            "--tool-version".into()
        } else {
            "--execute".into()
        },
    ];
    if let Some(coverage) = &plan.coverage {
        argv.extend([
            "--coverage".into(),
            "--llvm-cov".into(),
            coverage.cov.clone(),
            "--llvm-profdata".into(),
            coverage.profdata.clone(),
        ]);
    }
    for file in &job.files {
        argv.push(format!("--bind={}", file.path));
    }
    ensure!(
        argv.iter().all(|arg| arg.len() <= 131_071)
            && argv
                .iter()
                .try_fold(0usize, |n, arg| n.checked_add(arg.len()))
                .is_some_and(|n| n <= 1024 * 1024),
        "Cargo stage argv exceeds execution limits"
    );
    Ok(argv)
}
/// Caller owns the batch coordinator and retains the selection until all jobs
/// complete. Dropping the batch cancels; the existing owner still finishes cleanup.
pub(crate) async fn run_stage(
    stage: Stage,
    execution: Arc<ExecutionSelection>,
    mask: ChildSignalMask,
    cancel: Arc<Cancellation>,
    owners: Arc<Owners>,
) -> Result<(Value, Report, Vec<String>)> {
    stage
        .execute_batch(&execution.grants, mask, cancel, owners)
        .await
}

pub(crate) struct Observation {
    index: usize,
    version: bool,
    report: Report,
    argv: Vec<String>,
    data: Value,
    artifact: Option<crate::candidate_stage_artifact::Artifact>,
}
#[derive(Clone)]
pub(crate) struct VersionPermit {
    index: usize,
    candidate_sha256: String,
    policy_sha256: String,
    version: String,
}
fn verify_observation<'a>(plan: &'a Plan, observation: &Observation) -> Result<&'a Job> {
    let job = plan
        .jobs
        .get(observation.index)
        .context("Cargo observation index is invalid")?;
    ensure!(
        observation.data["tree"] == plan.tree
            && observation.data["candidate_sha256"] == plan.candidate_sha256
            && observation.data["policy_sha256"] == plan.policy_sha256
            && observation.data["grants_sha256"] == plan.grants_sha256
            && observation.data["bindings"] == serde_json::to_value(&job.files)?
            && observation.data["manifest"] == job.manifest
            && observation.data["manifest_oid"] == job.manifest_oid
            && observation.data["version"] == observation.version
            && observation.data["coverage"] == plan.coverage_required,
        "Cargo stage evidence differs from plan"
    );
    Ok(job)
}
pub(crate) fn accept_version(plan: &Plan, observation: &Observation) -> Result<VersionPermit> {
    ensure!(
        observation.version && observation.artifact.is_none(),
        "Cargo version observation required"
    );
    verify_observation(plan, observation)?;
    let version = match crate::candidate_execution_result::version(
        &observation.report,
        plan.capture_chars,
    )? {
        crate::candidate_execution_result::VersionEvidence::Available(value) => value,
        crate::candidate_execution_result::VersionEvidence::Unavailable(error) => {
            anyhow::bail!("Cargo version unavailable: {error}")
        }
    };
    ensure!(
        version
            .strip_prefix("cargo ")
            .is_some_and(|rest| rest.as_bytes().first().is_some_and(u8::is_ascii_digit)),
        "unrecognized Cargo version output"
    );
    if plan.coverage_required {
        ensure!(
            observation
                .report
                .stdout
                .lines()
                .any(|line| line.starts_with("rustc LLVM ") && line.ends_with("matched")),
            "matching Rust LLVM tool versions not observed"
        );
    }
    Ok(VersionPermit {
        index: observation.index,
        candidate_sha256: plan.candidate_sha256.clone(),
        policy_sha256: plan.policy_sha256.clone(),
        version,
    })
}
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_job(
    index: usize,
    version: bool,
    stage: Stage,
    execution: Arc<ExecutionSelection>,
    mask: ChildSignalMask,
    cancel: Arc<Cancellation>,
    owners: Arc<Owners>,
    budget: Arc<crate::candidate_stage_artifact::ArtifactBudget>,
) -> Result<Observation> {
    let capture = stage.data["coverage"] == true && !version;
    let (data, report, argv, artifact) = if capture {
        let request = crate::candidate_stage_artifact::Request {
            name: "rust-coverage.lcov",
            maximum: 4 * 1024 * 1024,
            budget,
        };
        stage
            .execute_batch_capture(&execution.grants, mask, cancel, owners, request)
            .await?
    } else {
        let (data, report, argv) = run_stage(stage, execution, mask, cancel, owners).await?;
        (data, report, argv, None)
    };
    ensure!(
        data["version"] == version,
        "Cargo observation role differs from stage"
    );
    Ok(Observation {
        index,
        version,
        report,
        argv,
        data,
        artifact,
    })
}
pub(crate) fn finding(id: &str, rule: &str, message: String) -> Finding {
    let mut finding = Finding {
        check_id: id.into(),
        rule_id: rule.into(),
        severity: crate::candidate_assessment::Severity::Critical,
        message,
        path: None,
        line: None,
        column: None,
        help: None,
        evidence: Map::new(),
        fingerprint: String::new(),
        exception_id: None,
        inherited: false,
    };
    candidate_assessment::finalize_finding(&mut finding);
    finding
}
pub(crate) fn observation_problem(id: &str, report: &Report) -> Option<Finding> {
    if !report.cleanup_complete || !report.resources.verified || !report.errors.is_empty() {
        return Some(finding(
            id,
            "targeted-test-crash",
            "test stage cleanup or resource containment is incomplete".into(),
        ));
    }
    match report.outcome {
        Outcome::TimedOut => Some(finding(
            id,
            "targeted-test-timeout",
            "test stage timed out".into(),
        )),
        Outcome::Cancelled => Some(finding(
            id,
            "targeted-test-killed",
            "test stage was cancelled".into(),
        )),
        Outcome::Failed => Some(finding(
            id,
            "targeted-test-crash",
            "test stage supervision failed".into(),
        )),
        Outcome::Exited if report.signal.is_some() => Some(finding(
            id,
            "targeted-test-crash",
            "test stage exited with contradictory signal evidence".into(),
        )),
        Outcome::Exited if report.exit_code != Some(0) => Some(finding(
            id,
            "targeted-test-failed",
            format!("test stage exited {:?}", report.exit_code),
        )),
        Outcome::Exited => None,
    }
}
/// The caller passes only observations returned by `run_job`, in any order.
/// A fast policy can pass for successfully completed admitted Cargo invocations.
/// Full coverage and selected Python or contract obligations remain blocking.
pub(crate) fn finish(
    execution: &ExecutionSelection,
    selected: &TestSelection<'_>,
    plan: &Plan,
    check_id: &str,
    observations: Vec<Observation>,
) -> Result<(CheckResult, Value)> {
    ensure!(
        check_id == plan.check_id && plan.coverage_required == (check_id == "targeted-tests-full"),
        "targeted test mode changed"
    );
    ensure!(
        check_binding(execution, selected)? == plan.candidate_sha256
            && execution.policy.digest == plan.policy_sha256
            && execution.grants.digest() == plan.grants_sha256
            && execution.candidate.tree_oid == plan.tree,
        "targeted test input changed"
    );
    let (rows, mut findings) = collect_observations(check_id, plan, observations)?;
    findings.extend(scope_findings(check_id, plan));
    let coverage_verified = plan
        .coverage
        .as_ref()
        .is_some_and(|v| v.unsupported.is_empty())
        && rows.iter().filter(|r| r["coverage"].is_object()).count() == plan.jobs.len();
    let files = plan
        .jobs
        .iter()
        .flat_map(|job| job.files.iter().map(|file| file.path.clone()))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let metrics = Map::from_iter([
        ("cargo_stages".into(), json!(rows.len())),
        ("test_discovery_verified".into(), json!(false)),
        ("coverage_verified".into(), json!(coverage_verified)),
    ]);
    let result = CheckResult {
        check_id: check_id.into(),
        status: if findings.is_empty() && plan.jobs.is_empty() {
            CheckStatus::Skipped
        } else if findings.is_empty() {
            CheckStatus::Passed
        } else if findings.iter().all(|finding| {
            matches!(
                finding.rule_id.as_str(),
                "targeted-test-failed" | "changed-code-coverage"
            )
        }) {
            CheckStatus::Failed
        } else {
            CheckStatus::Error
        },
        duration_ms: 0,
        findings,
        files,
        command: Vec::new(),
        tool_version: None,
        cache_key: None,
        cache_hit: false,
        exit_code: None,
        skipped_reason: if plan.jobs.is_empty()
            && !plan.python_pending
            && !plan.contracts_pending
            && !plan.coverage_required
        {
            Some("no selected tests".into())
        } else {
            None
        },
        stdout_tail: String::new(),
        stderr_tail: String::new(),
        metrics,
    };
    Ok((
        result,
        json!({"schema_version":1,"candidate_tree":plan.tree,"candidate_sha256":plan.candidate_sha256,"policy_sha256":plan.policy_sha256,"grants_sha256":plan.grants_sha256,"toolchain_assurance":"operator_granted_version_observed_not_attested","cargo_stages":rows,"python_pending":plan.python_pending,"contract_runtime_pending":plan.contracts_pending,"test_discovery_verified":false,"coverage_verified":coverage_verified,"scope":"captured_cargo_invocations_only","review_complete":false}),
    ))
}

fn scope_findings(id: &str, plan: &Plan) -> Vec<Finding> {
    let mut findings = Vec::new();
    if plan.python_pending {
        findings.push(finding(
            id,
            "pytest-adapter-unavailable",
            "selected Python tests have not been executed by a bound pytest adapter".into(),
        ));
    }
    if plan.contracts_pending {
        findings.push(finding(
            id,
            "contract-runtime-unavailable",
            "selected legacy Cargo contracts lack bound candidate-built runtime setup".into(),
        ));
    }
    if plan.coverage_required
        && plan
            .coverage
            .as_ref()
            .is_none_or(|v| !v.unsupported.is_empty())
    {
        findings.push(finding(
            id,
            "coverage-incomplete",
            "selected non-Rust native sources lack supported changed-line coverage".into(),
        ));
    }
    findings
}

#[cfg(test)]
#[path = "candidate_targeted_cargo_tests.rs"]
mod tests;

fn bounded_observation(observation: &Observation, plan: &Plan, bytes: &mut usize) -> Result<()> {
    ensure!(
        observation.argv.len() <= 4096 && observation.report.errors.len() <= 64,
        "Cargo observation count exceeds limit"
    );
    ensure!(
        observation.report.stdout.chars().count() <= plan.capture_chars
            && observation.report.stderr.chars().count() <= plan.capture_chars,
        "Cargo output exceeds capture bound"
    );
    for value in observation.argv.iter().chain(&observation.report.errors) {
        *bytes = bytes
            .checked_add(value.len())
            .context("Cargo observation byte overflow")?;
        ensure!(
            value.len() <= 131_071 && *bytes <= 8 * 1024 * 1024,
            "Cargo observation exceeds byte limit"
        );
    }
    Ok(())
}
fn observation_row(job: &Job, observation: &Observation, version: Option<&str>) -> Value {
    json!({"manifest":job.manifest,"manifest_oid":job.manifest_oid,"bindings": if observation.version { json!([]) } else { json!(job.files) },"version_stage":observation.version,"cargo_version":version,"outcome":observation.report.outcome,"exit_code":observation.report.exit_code,"cleanup_complete":observation.report.cleanup_complete,"resource_verified":observation.report.resources.verified,"stdout_bytes":observation.report.stdout_bytes,"stderr_bytes":observation.report.stderr_bytes,"argv":observation.argv})
}
fn add_coverage(
    id: &str,
    plan: &Plan,
    job: &Job,
    observation: &Observation,
    row: &mut Value,
) -> Result<Option<crate::candidate_python_coverage::Coverage>> {
    let Some(coverage) = &plan.coverage else {
        ensure!(
            observation.artifact.is_none(),
            "unexpected Rust coverage artifact"
        );
        return Ok(None);
    };
    if observation_problem(id, &observation.report).is_some() {
        ensure!(
            observation.artifact.is_none(),
            "failed Cargo test produced accepted coverage"
        );
        return Ok(None);
    }
    let artifact = observation
        .artifact
        .as_ref()
        .context("successful Cargo test lacks Rust coverage")?;
    ensure!(
        format!("{:x}", Sha256::digest(&artifact.bytes)) == artifact.sha256,
        "Rust coverage digest changed"
    );
    let root = observation.data["snapshot_root"]
        .as_str()
        .context("Rust coverage snapshot root missing")?;
    let measured = crate::candidate_targeted_rust_coverage::evaluate(
        coverage,
        &job.manifest,
        root,
        &artifact.bytes,
    )?;
    row["coverage_sha256"] = json!(artifact.sha256);
    row["coverage"] = serde_json::to_value(&measured)?;
    Ok(Some(measured))
}
fn aggregate_coverage(
    plan: &Plan,
    rows: &[crate::candidate_python_coverage::Coverage],
    id: &str,
    findings: &mut Vec<Finding>,
) -> Result<()> {
    let Some(coverage) = &plan.coverage else {
        return Ok(());
    };
    if rows.len() != plan.jobs.len() {
        return Ok(());
    }
    let (mut high_hit, mut high_total, mut other_hit, mut other_total) =
        (0usize, 0usize, 0usize, 0usize);
    for row in rows {
        high_hit = high_hit
            .checked_add(row.high.covered)
            .context("coverage total overflow")?;
        high_total = high_total
            .checked_add(row.high.measurable)
            .context("coverage total overflow")?;
        other_hit = other_hit
            .checked_add(row.other.covered)
            .context("coverage total overflow")?;
        other_total = other_total
            .checked_add(row.other.measurable)
            .context("coverage total overflow")?;
    }
    if (high_total > 0 && high_hit as f64 * 100.0 < coverage.high * high_total as f64)
        || (other_total > 0 && other_hit as f64 * 100.0 < coverage.other * other_total as f64)
    {
        let mut item = finding(
            id,
            "changed-code-coverage",
            "changed Rust coverage is below its risk threshold".into(),
        );
        item.severity = crate::candidate_assessment::Severity::High;
        findings.push(item);
    }
    Ok(())
}
fn collect_observations(
    id: &str,
    plan: &Plan,
    observations: Vec<Observation>,
) -> Result<(Vec<Value>, Vec<Finding>)> {
    ensure!(
        observations.len() <= plan.jobs.len().saturating_mul(2),
        "too many Cargo stage observations"
    );
    let mut by_stage = BTreeMap::new();
    let mut bytes = 0usize;
    for observation in observations {
        verify_observation(plan, &observation)?;
        bounded_observation(&observation, plan, &mut bytes)?;
        let key = (observation.index, observation.version);
        ensure!(
            by_stage.insert(key, observation).is_none(),
            "duplicate Cargo stage observation"
        );
    }
    let mut rows = Vec::new();
    let mut findings = Vec::new();
    let mut measurements = Vec::new();
    for (index, job) in plan.jobs.iter().enumerate() {
        let version = by_stage
            .remove(&(index, true))
            .context("Cargo version stage missing")?;
        match accept_version(plan, &version) {
            Ok(permit) => {
                rows.push(observation_row(job, &version, Some(&permit.version)));
                let test = by_stage
                    .remove(&(index, false))
                    .context("Cargo test stage missing after successful version")?;
                ensure!(
                    test.data["cargo_version"] == permit.version,
                    "Cargo test did not bind the observed version"
                );
                if let Some(problem) = observation_problem(id, &test.report) {
                    findings.push(problem);
                }
                let mut row = observation_row(job, &test, Some(&permit.version));
                if let Some(measured) = add_coverage(id, plan, job, &test, &mut row)? {
                    measurements.push(measured);
                }
                rows.push(row);
            }
            Err(error) => {
                ensure!(
                    !by_stage.contains_key(&(index, false)),
                    "Cargo test ran after failed version observation"
                );
                rows.push(observation_row(job, &version, None));
                findings.push(finding(
                    id,
                    "required-analyzer-unavailable",
                    format!("Cargo version observation failed: {error:#}"),
                ));
            }
        }
    }
    ensure!(by_stage.is_empty(), "unexpected Cargo stage observation");
    aggregate_coverage(plan, &measurements, id, &mut findings)?;
    Ok((rows, findings))
}
