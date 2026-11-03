//! Captured Python-targeted Stage planning. Guest Python tools are operator grants;
//! all process ownership, cancellation and resource enforcement remain Stage's.
use crate::{
    candidate_command::PreparedArgv,
    candidate_execution_plan::{self, Selection as Execution, Stage},
    candidate_execution_workspace::Workspace,
    candidate_python_coverage::ChangedFile,
    candidate_source::{lines::RangeScope, TreeEntry},
    candidate_test_selection::Selection,
};
use anyhow::{ensure, Context, Result};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    os::unix::fs::MetadataExt,
};

pub(crate) struct Plan {
    pub(crate) tests: Vec<String>,
    pub(crate) shards: Vec<Vec<String>>,
    pub(crate) bindings: Vec<(String, String)>,
    pub(crate) changed: Vec<ChangedFile>,
    pub(crate) tree: String,
    pub(crate) policy: String,
    pub(crate) grants: String,
    pub(crate) candidate: String,
    pub(crate) full: bool,
    pub(crate) record_owned: bool,
    pub(crate) record_configs: Vec<Option<RecordConfig>>,
    pub(crate) workers: usize,
    pub(crate) interpreter: String,
    pub(crate) pytest: String,
    pub(crate) coverage_tool: Option<String>,
    pub(crate) threshold: f64,
    pub(crate) high_threshold: f64,
}
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
pub(crate) struct RecordConfig {
    path: String,
    oid: String,
    lines: bool,
}
type RecordShards = (Vec<Vec<String>>, Vec<Option<RecordConfig>>);
fn shards(tests: &[String], maximum: usize) -> Result<Vec<Vec<String>>> {
    ensure!(
        !tests.is_empty() && tests.len() <= 4096,
        "invalid selected Python test count"
    );
    let n = if maximum == 0 {
        1
    } else {
        tests.len().div_ceil(maximum)
    };
    ensure!(n <= 256, "Python test shard count exceeds 256");
    let mut result = vec![Vec::new(); n];
    for (index, test) in tests.iter().enumerate() {
        result[index % n].push(test.clone());
    }
    Ok(result)
}
fn regular(entries: &BTreeMap<String, TreeEntry>, path: &str) -> Result<(String, String)> {
    crate::candidate_source::safe_path(path.as_bytes())?;
    ensure!(
        !path.chars().any(char::is_control),
        "Python input path contains control characters"
    );
    let entry = entries
        .get(path)
        .context("selected Python input absent from captured tree")?;
    ensure!(
        entry.object_type == "blob"
            && matches!(entry.mode.as_str(), "100644" | "100755")
            && entry.size.is_some_and(|n| n <= 1024 * 1024),
        "selected Python input is not a bounded regular blob"
    );
    Ok((path.into(), entry.oid.clone()))
}
fn granted(execution: &Execution, full: bool) -> Result<(String, String, Option<String>)> {
    let alias = execution
        .grants
        .python
        .as_ref()
        .context("Python interpreter grant missing")?;
    let path = execution
        .grants
        .programs
        .get(alias)
        .context("Python interpreter alias not granted")?;
    let forge = execution
        .grants
        .programs
        .get("forge")
        .context("forge executable grant missing")?;
    let expected = std::fs::File::open(forge)?.metadata()?;
    let running = std::fs::File::open("/proc/self/exe")?.metadata()?;
    ensure!(
        expected.is_file() && expected.dev() == running.dev() && expected.ino() == running.ino(),
        "forge grant differs from running executable"
    );
    let interpreter = path
        .to_str()
        .context("interpreter grant must be UTF-8")?
        .to_owned();
    let pytest = execution
        .grants
        .programs
        .get("pytest")
        .context("pytest entrypoint grant missing")?
        .to_str()
        .context("pytest grant must be UTF-8")?
        .to_owned();
    let coverage = if full {
        Some(
            execution
                .grants
                .programs
                .get("coverage")
                .context("coverage entrypoint grant missing")?
                .to_str()
                .context("coverage grant must be UTF-8")?
                .to_owned(),
        )
    } else {
        None
    };
    Ok((interpreter, pytest, coverage))
}
fn checked_paths(
    execution: &Execution,
    selected: &Selection<'_>,
    entries: &BTreeMap<String, TreeEntry>,
) -> Result<Vec<(String, String)>> {
    let mut paths: BTreeSet<String> = selected.python_tests.iter().cloned().collect();
    paths.extend(
        execution
            .candidate
            .changes
            .iter()
            .filter(|c| {
                !c.deleted()
                    && c.classes.iter().any(|v| v == "python")
                    && !c.classes.iter().any(|v| v == "test")
            })
            .map(|c| c.path.clone()),
    );
    ensure!(paths.len() <= 4096, "Python binding count exceeds 4096");
    let mut bytes = 0usize;
    let mut result = Vec::new();
    for path in paths {
        bytes = bytes
            .checked_add(path.len())
            .context("Python binding size overflow")?;
        ensure!(bytes <= 8 * 1024 * 1024, "Python binding paths too large");
        result.push(regular(entries, &path)?);
    }
    Ok(result)
}
fn record_setting(bytes: &[u8]) -> Result<Option<bool>> {
    let parsed: toml::Value = toml::from_str(std::str::from_utf8(bytes)?)?;
    let table = parsed
        .get("tool")
        .and_then(|v| v.get("forge"))
        .and_then(|v| v.get("test_record"));
    let Some(value) = table else { return Ok(None) };
    let table = value.as_table().context("test_record must be a table")?;
    table
        .get("store")
        .and_then(toml::Value::as_str)
        .context("test_record store missing")?;
    Ok(Some(match table.get("lines") {
        None => true,
        Some(v) => v.as_bool().context("test_record lines must be boolean")?,
    }))
}

fn record_groups(
    execution: &Execution,
    entries: &BTreeMap<String, TreeEntry>,
    tests: &[String],
    maximum: usize,
    bindings: &mut Vec<(String, String)>,
) -> Result<RecordShards> {
    let source = execution.source()?;
    let workspace = Workspace::new(&source, &execution.candidate.tree_oid, &execution.grants)?;
    let mut paths = BTreeSet::from(["pyproject.toml".to_owned()]);
    for test in tests {
        let mut parent = std::path::Path::new(test).parent();
        while let Some(dir) = parent {
            if dir.as_os_str().is_empty() {
                break;
            }
            paths.insert(format!("{}/pyproject.toml", dir.display()));
            parent = dir.parent();
        }
    }
    ensure!(
        paths.len() <= 4096,
        "recorder configuration inventory exceeds limit"
    );
    let mut settings = BTreeMap::new();
    let mut held = Vec::new();
    for path in paths {
        if !entries.contains_key(&path) {
            continue;
        }
        let (_, oid) = regular(entries, &path)?;
        let (binding, bytes) =
            crate::candidate_inputs::snapshot(&workspace.path(), &path, &oid, 1024 * 1024)?;
        if let Some(lines) = record_setting(&bytes)? {
            settings.insert(
                path.clone(),
                RecordConfig {
                    path: path.clone(),
                    oid: oid.clone(),
                    lines,
                },
            );
        }
        if !bindings.iter().any(|(existing, _)| existing == &path) {
            bindings.push((path, oid));
        }
        held.push(binding);
    }
    let groups = group_record_shards(tests, maximum, &settings)?;
    for binding in held {
        binding.verify()?;
    }
    workspace.discard()?;
    Ok(groups)
}
fn group_record_shards(
    tests: &[String],
    maximum: usize,
    settings: &BTreeMap<String, RecordConfig>,
) -> Result<RecordShards> {
    let mut groups: BTreeMap<Option<RecordConfig>, Vec<String>> = BTreeMap::new();
    for test in tests {
        let mut parent = std::path::Path::new(test).parent();
        let mut chosen = None;
        while let Some(dir) = parent {
            let key = if dir.as_os_str().is_empty() {
                "pyproject.toml".to_owned()
            } else {
                format!("{}/pyproject.toml", dir.display())
            };
            if let Some(config) = settings.get(&key) {
                chosen = Some(config.clone());
                break;
            }
            parent = dir.parent();
        }
        groups.entry(chosen).or_default().push(test.clone());
    }
    let mut output = Vec::new();
    let mut configs = Vec::new();
    for (config, paths) in groups {
        for group in shards(&paths, maximum)? {
            output.push(group);
            configs.push(config.clone());
        }
    }
    ensure!(output.len() <= 256, "Python test shard count exceeds 256");
    Ok((output, configs))
}

fn changed_files(execution: &Execution, bindings: &[(String, String)]) -> Result<Vec<ChangedFile>> {
    let source = execution.source()?;
    let paths: Vec<_> = execution
        .candidate
        .changes
        .iter()
        .filter(|c| {
            !c.deleted()
                && c.classes.iter().any(|v| v == "python")
                && !c.classes.iter().any(|v| v == "test")
        })
        .map(|c| c.path.as_str())
        .collect();
    let ranges = source.changed_ranges(&execution.candidate, &paths)?;
    let workspace = Workspace::new(&source, &execution.candidate.tree_oid, &execution.grants)?;
    let oids: BTreeMap<_, _> = bindings
        .iter()
        .map(|(path, oid)| (path.as_str(), oid.as_str()))
        .collect();
    let risks: BTreeMap<_, _> = execution
        .candidate
        .changes
        .iter()
        .map(|c| (c.path.as_str(), c.risk.as_str()))
        .collect();
    let mut held = Vec::new();
    let mut output = Vec::new();
    let mut total = 0usize;
    let mut input_bytes = 0usize;
    for row in ranges.files {
        if row.scope != RangeScope::Changed {
            continue;
        }
        let oid = oids
            .get(row.path.as_str())
            .context("changed Python source not bound")?;
        let (input, bytes) =
            crate::candidate_inputs::snapshot(&workspace.path(), &row.path, oid, 1024 * 1024)?;
        input_bytes = input_bytes
            .checked_add(bytes.len())
            .context("Python source byte overflow")?;
        ensure!(
            input_bytes <= 32 * 1024 * 1024,
            "Python source bytes exceed 32 MiB"
        );
        let text = std::str::from_utf8(&bytes).context("Python coverage source invalid UTF-8")?;
        let projected =
            crate::candidate_analysis::project_git_lines(text, &row.ranges, &mut total)?;
        let risk = *risks
            .get(row.path.as_str())
            .context("changed Python source missing")?
            == "high";
        output.push(ChangedFile {
            path: row.path.clone(),
            absolute_path: String::new(),
            lines: projected.into_iter().collect(),
            high_risk: risk,
        });
        held.push(input);
    }
    for input in &held {
        input.verify()?;
    }
    workspace.discard()?;
    Ok(output)
}
pub(crate) fn plan(
    execution: &Execution,
    selected: &Selection<'_>,
    check_id: &str,
) -> Result<Plan> {
    ensure!(
        matches!(check_id, "targeted-tests" | "targeted-tests-full"),
        "invalid targeted Python policy"
    );
    let candidate = crate::candidate_targeted_cargo::check_binding(execution, selected)?;
    let check = execution
        .checks()?
        .into_iter()
        .find(|v| v["check_id"] == check_id)
        .context("targeted check not active")?;
    let maximum = usize::try_from(
        check["shard_max_files"]
            .as_u64()
            .context("missing shard maximum")?,
    )?;
    let workers = usize::try_from(
        check["shard_workers"]
            .as_u64()
            .context("missing shard workers")?,
    )?;
    ensure!((1..=32).contains(&workers), "invalid shard worker count");
    let tests = selected.python_tests.clone();
    let source = execution.source()?;
    let entries = source
        .list_tree(&execution.candidate.tree_oid)?
        .into_iter()
        .map(|v| (v.path.clone(), v))
        .collect();
    let mut bindings = checked_paths(execution, selected, &entries)?;
    let full = check_id == "targeted-tests-full";
    let (shards, record_configs) = if full {
        record_groups(execution, &entries, &tests, maximum, &mut bindings)?
    } else {
        let shards = shards(&tests, maximum)?;
        let configs = vec![None; shards.len()];
        (shards, configs)
    };
    let record_owned = record_configs
        .iter()
        .any(|config| config.as_ref().is_some_and(|c| c.lines));
    if full && entries.contains_key("pyproject.toml") {
        let config = regular(&entries, "pyproject.toml")?;
        if !bindings.iter().any(|item| item.0 == config.0) {
            bindings.push(config);
        }
    }
    let changed = if full {
        changed_files(execution, &bindings)?
    } else {
        Vec::new()
    };
    let threshold = execution.policy.document["coverage_threshold"]
        .as_f64()
        .context("missing coverage threshold")?;
    let high_threshold = execution.policy.document["high_risk_coverage_threshold"]
        .as_f64()
        .context("missing high-risk coverage threshold")?;
    let (interpreter, pytest, coverage_tool) = granted(execution, full)?;
    Ok(Plan {
        tests,
        shards,
        bindings,
        changed,
        tree: execution.candidate.tree_oid.clone(),
        policy: execution.policy.digest.clone(),
        grants: execution.grants.digest().into(),
        candidate,
        full,
        record_owned,
        record_configs,
        workers,
        interpreter,
        pytest,
        coverage_tool,
        threshold,
        high_threshold,
    })
}
pub(crate) fn stage_owned(
    execution: &Execution,
    plan: &Plan,
    index: usize,
    version: bool,
    permit: Option<&VersionPermit>,
) -> Result<Stage> {
    ensure!(
        crate::candidate_targeted_cargo::digest(&execution.candidate)? == plan.candidate
            && execution.policy.digest == plan.policy
            && execution.grants.digest() == plan.grants
            && execution.candidate.tree_oid == plan.tree,
        "Python test plan binding changed"
    );
    ensure!(
        granted(execution, plan.full)?
            == (
                plan.interpreter.clone(),
                plan.pytest.clone(),
                plan.coverage_tool.clone()
            ),
        "Python tool grants changed"
    );
    if !version {
        let permit = permit.context("Python tool version observation required")?;
        ensure!(
            permit.candidate == plan.candidate
                && permit.grants == plan.grants
                && permit.policy == plan.policy
                && permit.full == plan.full,
            "Python version permit changed"
        );
    }
    let tests = plan
        .shards
        .get(index)
        .context("Python shard index invalid")?;
    let check_id = if plan.full {
        "targeted-tests-full"
    } else {
        "targeted-tests"
    };
    let check = execution
        .checks()?
        .into_iter()
        .find(|c| c["check_id"] == check_id)
        .context("targeted policy missing")?;
    let source = execution.source()?;
    let workspace = Workspace::new(&source, &plan.tree, &execution.grants)?;
    let argv = stage_argv(execution, plan, tests, index, &workspace.path(), version)?;
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
        limits.cpu_seconds = limits.cpu_seconds.map(|v| v.min(30));
    }
    let snapshot_root = workspace
        .path()
        .to_str()
        .context("snapshot root not UTF-8")?
        .to_owned();
    Ok(Stage {
        workspace,
        plan: command,
        limits,
        argv,
        data: json!({"scope":"captured_pytest_shard", "index":index,"version":version,"candidate_sha256":plan.candidate,"tree":plan.tree,"policy_sha256":plan.policy,"grants_sha256":plan.grants,"interpreter":plan.interpreter,"pytest":plan.pytest,"coverage_tool":plan.coverage_tool,"tests":tests,"bindings":plan.bindings,"full":plan.full,"record_owned":plan.record_owned,"record_config":plan.record_configs[index],"snapshot_root":snapshot_root}),
    })
}
fn stage_argv(
    execution: &Execution,
    plan: &Plan,
    tests: &[String],
    index: usize,
    snapshot: &std::path::Path,
    version: bool,
) -> Result<Vec<String>> {
    let mut argv = vec![
        "forge".into(),
        "candidate".into(),
        "python-test".into(),
        "--repo".into(),
        snapshot.to_str().context("snapshot path not UTF-8")?.into(),
        "--base".into(),
        execution.candidate.base_tree_oid.clone(),
        "--tree".into(),
        plan.tree.clone(),
        "--interpreter".into(),
        plan.interpreter.clone(),
        "--pytest".into(),
        plan.pytest.clone(),
    ];
    if plan.full {
        argv.push("--coverage".into());
        argv.push("--coverage-tool".into());
        argv.push(
            plan.coverage_tool
                .clone()
                .context("coverage grant missing")?,
        );
    }
    if !version {
        if let Some(config) = plan.record_configs[index].as_ref().filter(|c| c.lines) {
            argv.push("--record-owned".into());
            argv.push("--record-config".into());
            argv.push(config.path.clone());
        }
    }
    argv.push(if version {
        "--tool-version".into()
    } else {
        "--execute".into()
    });
    for test in tests {
        argv.push(format!("--test={test}"));
    }
    for (path, _) in &plan.bindings {
        argv.push(format!("--bind={path}"));
    }
    let size = argv
        .iter()
        .try_fold(0usize, |n, s| n.checked_add(s.len()))
        .context("Python argv size overflow")?;
    ensure!(
        argv.len() <= 8192 && size <= 1024 * 1024 && argv.iter().all(|s| s.len() <= 131071),
        "Python argv exceeds limit"
    );
    Ok(argv)
}
#[derive(Clone)]
pub(crate) struct VersionPermit {
    candidate: String,
    grants: String,
    policy: String,
    full: bool,
    version: String,
}
pub(crate) struct Observation {
    data: Value,
    report: crate::confined_execution::Report,
    artifact: Option<crate::candidate_stage_artifact::Artifact>,
    record: Option<crate::candidate_stage_artifact::Artifact>,
}
fn verify_observation(plan: &Plan, observation: &Observation) -> Result<(usize, bool)> {
    let data = &observation.data;
    let index = usize::try_from(
        data["index"]
            .as_u64()
            .context("missing Python shard index")?,
    )?;
    let tests = plan
        .shards
        .get(index)
        .context("Python shard outside plan")?;
    let version = data["version"]
        .as_bool()
        .context("missing Python stage role")?;
    ensure!(
        data["candidate_sha256"] == plan.candidate
            && data["tree"] == plan.tree
            && data["policy_sha256"] == plan.policy
            && data["grants_sha256"] == plan.grants
            && data["interpreter"] == plan.interpreter
            && data["pytest"] == plan.pytest
            && data["coverage_tool"] == serde_json::to_value(&plan.coverage_tool)?
            && data["tests"] == serde_json::to_value(tests)?
            && data["bindings"] == serde_json::to_value(&plan.bindings)?
            && data["full"] == plan.full
            && data["record_owned"] == plan.record_owned
            && data["record_config"] == serde_json::to_value(&plan.record_configs[index])?,
        "Python observation differs from plan"
    );
    Ok((index, version))
}
pub(crate) async fn run_stage(
    stage: Stage,
    execution: std::sync::Arc<Execution>,
    mask: crate::process_signals::ChildSignalMask,
    cancel: std::sync::Arc<crate::confined_execution::Cancellation>,
    owners: std::sync::Arc<crate::batch_execution::Owners>,
    budget: std::sync::Arc<crate::candidate_stage_artifact::ArtifactBudget>,
) -> Result<Observation> {
    let full = stage.data["full"]
        .as_bool()
        .context("missing Python coverage mode")?;
    let version = stage.data["version"]
        .as_bool()
        .context("missing Python stage role")?;
    if full && !version && stage.data["record_config"]["lines"] == true {
        let (data, report, _, artifacts) = stage
            .execute_batch_capture_pair(
                &execution.grants,
                mask,
                cancel,
                owners,
                crate::candidate_stage_artifact::Request {
                    name: "coverage.json",
                    maximum: 4 * 1024 * 1024,
                    budget: budget.clone(),
                },
                crate::candidate_stage_artifact::Request {
                    name: "test-record.sqlite",
                    maximum: 4 * 1024 * 1024,
                    budget,
                },
            )
            .await?;
        let (artifact, record) = match artifacts {
            Some(mut rows) => {
                ensure!(rows.len() == 2, "expected two Python artifacts");
                let record = rows.pop();
                (rows.pop(), record)
            }
            None => (None, None),
        };
        Ok(Observation {
            data,
            report,
            artifact,
            record,
        })
    } else if full && !version {
        let (data, report, _, artifact) = stage
            .execute_batch_capture(
                &execution.grants,
                mask,
                cancel,
                owners,
                crate::candidate_stage_artifact::Request {
                    name: "coverage.json",
                    maximum: 4 * 1024 * 1024,
                    budget,
                },
            )
            .await?;
        Ok(Observation {
            data,
            report,
            artifact,
            record: None,
        })
    } else {
        let (data, report, _) = stage
            .execute_batch(&execution.grants, mask, cancel, owners)
            .await?;
        Ok(Observation {
            data,
            report,
            artifact: None,
            record: None,
        })
    }
}
pub(crate) fn version_permit(plan: &Plan, observation: &Observation) -> Result<VersionPermit> {
    let (_, version) = verify_observation(plan, observation)?;
    ensure!(
        version && observation.artifact.is_none() && observation.record.is_none(),
        "Python version observation required"
    );
    let report = &observation.report;
    let version = crate::candidate_execution_result::version(report, 100_000)?;
    ensure!(
        matches!(
            version,
            crate::candidate_execution_result::VersionEvidence::Available(_)
        ),
        "pytest version stage failed"
    );
    ensure!(
        report.stdout.lines().any(|v| v.starts_with("Python 3.")),
        "Python interpreter version not observed"
    );
    ensure!(
        report.stdout.lines().any(|v| v.starts_with("pytest ")),
        "pytest version not observed"
    );
    if plan.full {
        ensure!(
            report
                .stdout
                .lines()
                .any(|v| v.starts_with("Coverage.py, version ")
                    || v.starts_with("Coverage.py version ")),
            "coverage version not observed"
        );
    }
    Ok(VersionPermit {
        candidate: plan.candidate.clone(),
        grants: plan.grants.clone(),
        policy: plan.policy.clone(),
        full: plan.full,
        version: report.stdout.clone(),
    })
}

/// The coordinator combines this Python-only result with independently required
/// native/contract obligations. Coverage is admitted only from owned artifacts.
pub(crate) fn finish(
    execution: &Execution,
    selected: &Selection<'_>,
    plan: &Plan,
    permit: &VersionPermit,
    observations: Vec<Observation>,
    interrupts: &crate::process_signals::Interrupts,
) -> Result<(crate::candidate_assessment::CheckResult, Value)> {
    ensure!(
        crate::candidate_targeted_cargo::check_binding(execution, selected)? == plan.candidate
            && execution.policy.digest == plan.policy
            && execution.grants.digest() == plan.grants
            && execution.candidate.tree_oid == plan.tree,
        "Python test completion binding changed"
    );
    ensure!(
        permit.candidate == plan.candidate
            && permit.policy == plan.policy
            && permit.grants == plan.grants
            && permit.full == plan.full,
        "Python version permit changed"
    );
    ensure!(
        observations.len() == plan.shards.len(),
        "missing Python shard observation"
    );
    let id = if plan.full {
        "targeted-tests-full"
    } else {
        "targeted-tests"
    };
    let (mut findings, rows, captured, records) = collect(plan, id, observations)?;
    let mut coverage = None;
    let mut function_metrics = serde_json::Map::new();
    if plan.full && findings.is_empty() {
        ensure!(
            captured.len() == plan.shards.len(),
            "Python coverage shard incomplete"
        );
        let result = crate::candidate_python_coverage::evaluate_shards(
            &captured,
            &plan.changed,
            plan.high_threshold,
            plan.threshold,
        )?;
        if !result.high_pass || !result.other_pass {
            findings.push(crate::candidate_targeted_cargo::finding(
                id,
                "changed-code-coverage",
                "changed Python coverage is below its risk threshold".into(),
            ));
        }
        coverage = Some(serde_json::to_value(result)?);
        if plan
            .record_configs
            .iter()
            .all(|c| c.as_ref().is_some_and(|c| c.lines))
        {
            function_metrics =
                record_findings(execution, plan, &records, id, &mut findings, interrupts)?;
        }
    }
    let status = if findings.is_empty() {
        crate::candidate_assessment::CheckStatus::Passed
    } else if findings.iter().all(|f| {
        matches!(
            f.rule_id.as_str(),
            "targeted-test-failed" | "changed-code-coverage" | "untested-changed-function"
        )
    }) {
        crate::candidate_assessment::CheckStatus::Failed
    } else {
        crate::candidate_assessment::CheckStatus::Error
    };
    let record_evaluated = function_metrics.contains_key("changed_functions");
    let result = crate::candidate_assessment::CheckResult {
        check_id: id.into(),
        status,
        duration_ms: 0,
        findings,
        files: plan.tests.clone(),
        command: Vec::new(),
        tool_version: Some(permit.version.clone()),
        cache_key: None,
        cache_hit: false,
        exit_code: None,
        skipped_reason: None,
        stdout_tail: String::new(),
        stderr_tail: String::new(),
        metrics: function_metrics,
    };
    Ok((
        result,
        json!({"scope":"captured_pytest_shards","candidate_tree":plan.tree,"candidate_sha256":plan.candidate,"policy_sha256":plan.policy,"grants_sha256":plan.grants,"interpreter":plan.interpreter,"pytest":plan.pytest,"coverage_tool":plan.coverage_tool,"version":permit.version,"bindings":plan.bindings,"shards":rows,"coverage":coverage,"record_scope":if record_evaluated { Some("fresh owned shard test and fixture windows; blob-matched, not execution authentication") } else if plan.full { Some("not evaluated: shard results unavailable or line recording disabled") } else { None },"record_evaluated":record_evaluated,"record_configs":plan.record_configs,"review_complete":false}),
    ))
}

fn record_findings(
    execution: &Execution,
    plan: &Plan,
    records: &[Vec<u8>],
    id: &str,
    findings: &mut Vec<crate::candidate_assessment::Finding>,
    interrupts: &crate::process_signals::Interrupts,
) -> Result<serde_json::Map<String, Value>> {
    ensure!(
        records.len() == plan.shards.len(),
        "test-record shard incomplete"
    );
    let (count, untested) = owned_untested(execution, plan, records, interrupts)?;
    let mut metrics = serde_json::Map::new();
    metrics.insert("changed_functions".into(), json!(count));
    metrics.insert("untested_changed_functions".into(), json!(untested.len()));
    if !untested.is_empty() {
        let names: Vec<_> = untested
            .iter()
            .take(20)
            .map(|(path, name, line)| format!("{path}:{line} {name}"))
            .collect();
        let extra = if untested.len() > 20 {
            format!(", and {} more", untested.len() - 20)
        } else {
            String::new()
        };
        let mut finding = crate::candidate_assessment::Finding {
            check_id: id.into(),
            rule_id: "untested-changed-function".into(),
            severity: crate::candidate_assessment::Severity::Medium,
            message: format!(
                "{} of {} changed functions ran in no recorded test: {}{}",
                untested.len(),
                count,
                names.join(", "),
                extra
            ),
            path: None,
            line: None,
            column: None,
            help: None,
            evidence: serde_json::Map::from_iter([(
                "untested".into(),
                json!(untested
                    .iter()
                    .map(|(path, name, line)| json!({"path":path,"qualname":name,"line":line}))
                    .collect::<Vec<_>>()),
            )]),
            fingerprint: String::new(),
            exception_id: None,
            inherited: false,
        };
        crate::candidate_assessment::finalize_finding(&mut finding);
        findings.push(finding);
    }
    Ok(metrics)
}

type Collected = (
    Vec<crate::candidate_assessment::Finding>,
    Vec<Value>,
    Vec<(Vec<u8>, String)>,
    Vec<Vec<u8>>,
);
fn collect(plan: &Plan, id: &str, observations: Vec<Observation>) -> Result<Collected> {
    let mut findings = Vec::new();
    let mut rows = Vec::new();
    let mut captured = Vec::new();
    let mut records = Vec::new();
    let mut seen = BTreeSet::new();
    for observation in observations {
        let (index, version) = verify_observation(plan, &observation)?;
        ensure!(
            !version && seen.insert(index),
            "duplicate or invalid Python shard observation"
        );
        let problem = crate::candidate_targeted_cargo::observation_problem(id, &observation.report);
        let succeeded = problem.is_none();
        if let Some(finding) = problem {
            findings.push(finding);
        }
        let mut artifact_sha = None;
        if let Some(artifact) = observation.artifact {
            use sha2::{Digest, Sha256};
            ensure!(
                plan.full
                    && succeeded
                    && format!("{:x}", Sha256::digest(&artifact.bytes)) == artifact.sha256,
                "Python coverage artifact digest changed"
            );
            let root = observation.data["snapshot_root"]
                .as_str()
                .context("Python shard root missing")?;
            artifact_sha = Some(artifact.sha256);
            captured.push((artifact.bytes, root.to_owned()));
        } else if plan.full && succeeded {
            anyhow::bail!("successful Python shard has no coverage artifact");
        }
        let record_sha = if let Some(record) = observation.record {
            use sha2::{Digest, Sha256};
            ensure!(
                plan.record_configs[index].as_ref().is_some_and(|c| c.lines)
                    && succeeded
                    && format!("{:x}", Sha256::digest(&record.bytes)) == record.sha256,
                "Python test-record artifact binding changed"
            );
            let sha = record.sha256.clone();
            records.push(record.bytes);
            Some(sha)
        } else {
            ensure!(
                !plan.record_configs[index].as_ref().is_some_and(|c| c.lines) || !succeeded,
                "successful shard lacks its test-record artifact"
            );
            None
        };
        rows.push(json!({"index":index,"tests":plan.shards[index],"outcome":observation.report.outcome,"exit_code":observation.report.exit_code,"cleanup_complete":observation.report.cleanup_complete,"resource_verified":observation.report.resources.verified,"coverage_sha256":artifact_sha,"test_record_sha256":record_sha,"record_config":plan.record_configs[index]}));
    }
    rows.sort_by_key(|row| row["index"].as_u64().unwrap_or(u64::MAX));
    Ok((findings, rows, captured, records))
}

pub(crate) fn intersect_missing(
    previous: Option<BTreeSet<(String, String, usize)>>,
    current: BTreeSet<(String, String, usize)>,
) -> BTreeSet<(String, String, usize)> {
    match previous {
        None => current,
        Some(previous) => previous.intersection(&current).cloned().collect(),
    }
}

type ChangedRecordLines = Vec<(String, Vec<usize>)>;
fn record_inputs(
    workspace: &Workspace,
    plan: &Plan,
) -> Result<(Vec<crate::candidate_inputs::BoundFile>, ChangedRecordLines)> {
    let mut held = Vec::new();
    let oids: BTreeMap<_, _> = plan
        .bindings
        .iter()
        .map(|(p, o)| (p.as_str(), o.as_str()))
        .collect();
    let mut changed = Vec::new();
    for file in &plan.changed {
        let oid = oids
            .get(file.path.as_str())
            .context("changed source blob missing")?;
        held.push(
            crate::candidate_inputs::snapshot(&workspace.path(), &file.path, oid, 1024 * 1024)?.0,
        );
        changed.push((
            file.path.clone(),
            file.lines.iter().map(|n| *n as usize).collect::<Vec<_>>(),
        ));
    }
    Ok((held, changed))
}

/// Rehydrate only owned shard records in a private workspace and compare windows
/// against candidate blob bytes. No historical host database is consulted.
fn owned_untested(
    execution: &Execution,
    plan: &Plan,
    records: &[Vec<u8>],
    interrupts: &crate::process_signals::Interrupts,
) -> Result<(usize, Vec<conductor_native::test_record_untested::Untested>)> {
    use cap_std::fs::{OpenOptions, OpenOptionsExt};
    use std::io::Write;
    ensure!(
        !records.is_empty() && records.len() <= 256,
        "invalid test-record shards"
    );
    let source = execution.source()?;
    let workspace = Workspace::new(&source, &plan.tree, &execution.grants)?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let result = (|| -> Result<_> {
        let (held, changed) = record_inputs(&workspace, plan)?;
        let root = workspace.artifact_root()?;
        let mut missing: Option<BTreeSet<(String, String, usize)>> = None;
        let mut count = None;
        for (index, bytes) in records.iter().enumerate() {
            ensure!(
                !interrupts.interrupted()?,
                "test-record evaluation interrupted"
            );
            ensure!(
                std::time::Instant::now() < deadline,
                "test-record evaluation exceeded deadline"
            );
            let name = format!("record-{index}.sqlite");
            let mut file = root.open_with(
                &name,
                OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .custom_flags(libc::O_NOFOLLOW),
            )?;
            file.write_all(bytes)?;
            file.sync_all()?;
            workspace.verify_artifact_root(&root)?;
            let (current, untested) = conductor_native::test_record_untested::untested_owned(
                &workspace.path(),
                &workspace.runtime().join(&name),
                &changed,
            )?;
            if let Some(previous) = count {
                ensure!(previous == current, "changed function count drift");
            }
            count = Some(current);
            let current: BTreeSet<_> = untested.into_iter().collect();
            missing = Some(intersect_missing(missing, current));
        }
        ensure!(
            !interrupts.interrupted()?,
            "test-record evaluation interrupted"
        );
        ensure!(
            std::time::Instant::now() < deadline,
            "test-record evaluation exceeded deadline"
        );
        for file in held {
            file.verify()?;
        }
        workspace.verify_artifact_root(&root)?;
        Ok((
            count.unwrap_or(0),
            missing.unwrap_or_default().into_iter().collect(),
        ))
    })();
    let cleanup = workspace.discard();
    match (result, cleanup) {
        (Ok(value), Ok(())) => {
            ensure!(
                !interrupts.interrupted()?,
                "test-record cleanup interrupted"
            );
            ensure!(
                std::time::Instant::now() < deadline,
                "test-record evaluation exceeded deadline"
            );
            Ok(value)
        }
        (result, cleanup) => anyhow::bail!(
            "test-record evaluation={:?}; workspace cleanup={:?}",
            result.err(),
            cleanup.err()
        ),
    }
}

#[cfg(test)]
#[path = "candidate_targeted_python_tests.rs"]
mod tests;
