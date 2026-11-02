//! Coordinator for freshly executed targeted Cargo and Python obligations.
//! Selection stays borrowed by the caller; spawned jobs own only immutable
//! plans, index/permit data, and an Arc to the operator-granted execution.
use crate::{
    batch_execution,
    candidate_execution_plan::Selection as Execution,
    candidate_targeted_cargo as cargo, candidate_targeted_contract as contract,
    candidate_targeted_python as python,
    candidate_test_selection::Selection,
    process_signals::{ChildSignalMask, Interrupts},
};
use anyhow::{ensure, Context, Result};
use serde_json::{json, Value};
use std::{collections::BTreeMap, sync::Arc};

enum Job {
    Cargo(usize, Option<cargo::VersionPermit>),
    Python(usize, Option<python::VersionPermit>),
    Contract(Option<contract::VersionPermit>),
}
enum Seen {
    Cargo(usize, cargo::Observation),
    Python(python::Observation),
    Contract(contract::Observation),
}
struct Plans {
    cargo: Option<cargo::Plan>,
    python: Option<python::Plan>,
    contract: Option<contract::Plan>,
    check: Value,
    budget: Arc<crate::candidate_stage_artifact::ArtifactBudget>,
}
fn prepare(execution: &Execution, selected: &Selection<'_>, id: &str) -> Result<Plans> {
    selected.verify()?;
    let check = execution
        .checks()?
        .into_iter()
        .find(|v| v["check_id"] == id)
        .context("targeted check is inactive")?;
    let cargo = if selected.native_tests.is_empty() {
        None
    } else {
        Some(cargo::plan(execution, selected, id)?)
    };
    let python = if selected.python_tests.is_empty() {
        None
    } else {
        Some(python::plan(execution, selected, id)?)
    };
    let contract = if selected.contract_targets.is_empty() {
        ensure!(
            selected.contract_test_paths.is_empty(),
            "contract selection inconsistent"
        );
        None
    } else {
        Some(contract::plan(execution, selected, id)?)
    };
    let count = cargo.as_ref().map_or(0, |v| v.jobs().len())
        + usize::from(python.is_some())
        + usize::from(contract.is_some());
    ensure!(count <= 256, "targeted version stages exceed batch limit");
    Ok(Plans {
        cargo,
        python,
        contract,
        check,
        budget: Arc::new(crate::candidate_stage_artifact::ArtifactBudget::new(
            16 * 1024 * 1024,
        )),
    })
}
async fn worker(
    job: Job,
    execution: Arc<Execution>,
    plans: Arc<Plans>,
    mask: ChildSignalMask,
    cancel: Arc<crate::confined_execution::Cancellation>,
    owners: Arc<batch_execution::Owners>,
) -> Result<Seen> {
    match job {
        Job::Cargo(index, permit) => {
            let plan = plans.cargo.as_ref().context("Cargo plan missing")?;
            let version = permit.is_none();
            let stage = cargo::stage_owned(
                &execution,
                plan,
                index,
                version,
                permit.as_ref(),
                &plans.check,
            )?;
            Ok(Seen::Cargo(
                index,
                cargo::run_job(
                    index,
                    version,
                    stage,
                    execution,
                    mask,
                    cancel,
                    owners,
                    plans.budget.clone(),
                )
                .await?,
            ))
        }
        Job::Python(index, permit) => {
            let plan = plans.python.as_ref().context("Python plan missing")?;
            let stage =
                python::stage_owned(&execution, plan, index, permit.is_none(), permit.as_ref())?;
            Ok(Seen::Python(
                python::run_stage(stage, execution, mask, cancel, owners, plans.budget.clone())
                    .await?,
            ))
        }
        Job::Contract(permit) => {
            let plan = plans.contract.as_ref().context("contract plan missing")?;
            let stage = contract::stage_owned(&execution, plan, permit.is_none(), permit.as_ref())?;
            Ok(Seen::Contract(
                contract::run_stage(stage, execution, mask, cancel, owners, plans.budget.clone())
                    .await?,
            ))
        }
    }
}
async fn execute(
    jobs: Vec<Job>,
    execution: Arc<Execution>,
    plans: Arc<Plans>,
    interrupts: &Interrupts,
    parallelism: usize,
) -> Result<Vec<Seen>> {
    if jobs.is_empty() {
        return Ok(Vec::new());
    }
    let signal = interrupts.register_reactor()?;
    ensure!(
        !interrupts.interrupted()?,
        "targeted execution interrupted before launch"
    );
    let mask = interrupts.child_mask();
    let batch = batch_execution::start(jobs, parallelism, move |job, cancel, owners| {
        worker(job, execution.clone(), plans.clone(), mask, cancel, owners)
    })?;
    let cancel = batch.cancellation();
    let wait = batch.wait();
    tokio::pin!(wait);
    let results = tokio::select! {
        rows=&mut wait => rows?,
        received=async { let mut bytes=[0u8;std::mem::size_of::<libc::signalfd_siginfo>()]; ensure!(signal.read(&mut bytes).await?>0,"interrupt descriptor closed"); Ok::<_,anyhow::Error>(()) } => {
            let wake=cancel.cancel(); let rows=wait.await?; received?; wake?; rows
        }
    };
    ensure!(
        !cancel.is_cancelled() && !interrupts.interrupted()?,
        "targeted execution cancelled after cleanup"
    );
    results.into_iter().collect()
}
fn version_jobs(plans: &Plans) -> Vec<Job> {
    let mut jobs = Vec::new();
    if let Some(plan) = &plans.cargo {
        jobs.extend((0..plan.jobs().len()).map(|index| Job::Cargo(index, None)));
    }
    if plans.python.is_some() {
        jobs.push(Job::Python(0, None));
    }
    if plans.contract.is_some() {
        jobs.push(Job::Contract(None));
    }
    jobs
}
type AdmittedStages = (
    Vec<Job>,
    Vec<cargo::Observation>,
    Option<python::VersionPermit>,
    Option<contract::VersionPermit>,
);
fn command_jobs(plans: &Plans, versions: Vec<Seen>) -> Result<AdmittedStages> {
    let mut jobs = Vec::new();
    let mut cargo_rows = Vec::new();
    let mut python_permit = None;
    let mut contract_permit = None;
    let mut seen = BTreeMap::new();
    for row in versions {
        match row {
            Seen::Cargo(index, observation) => {
                ensure!(
                    seen.insert(index, ()).is_none(),
                    "duplicate Cargo version observation"
                );
                let plan = plans.cargo.as_ref().context("unexpected Cargo version")?;
                if let Ok(permit) = cargo::accept_version(plan, &observation) {
                    jobs.push(Job::Cargo(index, Some(permit)));
                }
                cargo_rows.push(observation);
            }
            Seen::Python(observation) => {
                ensure!(
                    python_permit.is_none(),
                    "duplicate Python version observation"
                );
                let plan = plans.python.as_ref().context("unexpected Python version")?;
                python_permit = Some(python::version_permit(plan, &observation)?);
            }
            Seen::Contract(observation) => {
                ensure!(
                    contract_permit.is_none(),
                    "duplicate contract version observation"
                );
                let plan = plans
                    .contract
                    .as_ref()
                    .context("unexpected contract version")?;
                contract_permit = Some(contract::version_permit(plan, &observation)?);
            }
        }
    }
    if let Some(plan) = &plans.cargo {
        ensure!(
            seen.len() == plan.jobs().len(),
            "missing Cargo version observation"
        );
    }
    if let Some(plan) = &plans.python {
        let permit = python_permit
            .as_ref()
            .context("missing Python version observation")?;
        jobs.extend((0..plan.shards.len()).map(|index| Job::Python(index, Some(permit.clone()))));
    }
    if plans.contract.is_some() {
        jobs.push(Job::Contract(Some(
            contract_permit
                .as_ref()
                .context("missing contract version")?
                .clone(),
        )));
    }
    ensure!(jobs.len() <= 256, "targeted test stages exceed batch limit");
    Ok((jobs, cargo_rows, python_permit, contract_permit))
}
/// Must be awaited on a live Tokio reactor. Parent retains `selected` and calls
/// verify again after return; no borrowed selection enters spawned tasks.
pub(crate) async fn run(
    execution: Arc<Execution>,
    selected: &Selection<'_>,
    id: &str,
    interrupts: &Interrupts,
    parallelism: usize,
) -> Result<(crate::candidate_assessment::CheckResult, Value)> {
    ensure!(
        (1..=32).contains(&parallelism),
        "invalid targeted parallelism"
    );
    if selected.native_tests.is_empty()
        && selected.python_tests.is_empty()
        && selected.contract_targets.is_empty()
    {
        cargo::check_binding(&execution, selected)?;
        ensure!(
            execution.checks()?.iter().any(|v| v["check_id"] == id),
            "targeted check is inactive"
        );
        return empty(
            id,
            !selected.contract_targets.is_empty() || !selected.contract_test_paths.is_empty(),
        );
    }
    let plans = Arc::new(prepare(&execution, selected, id)?);
    ensure!(
        plans.cargo.is_some() || plans.python.is_some() || plans.contract.is_some(),
        "no targeted tests to execute"
    );
    let versions = execute(
        version_jobs(&plans),
        execution.clone(),
        plans.clone(),
        interrupts,
        parallelism,
    )
    .await?;
    selected.verify()?;
    let (jobs, mut cargo_rows, python_permit, contract_permit) = command_jobs(&plans, versions)?;
    let workers = plans
        .python
        .as_ref()
        .map_or(parallelism, |p| parallelism.min(p.workers));
    let results = execute(jobs, execution.clone(), plans.clone(), interrupts, workers).await?;
    let mut python_rows = Vec::new();
    let mut contract_row = None;
    for row in results {
        match row {
            Seen::Cargo(_, row) => cargo_rows.push(row),
            Seen::Python(row) => python_rows.push(row),
            Seen::Contract(row) => {
                ensure!(
                    contract_row.replace(row).is_none(),
                    "duplicate contract execution"
                );
            }
        }
    }
    selected.verify()?;
    let python = match (&plans.python, python_permit) {
        (Some(plan), Some(permit)) => Some(python::finish(
            &execution,
            selected,
            plan,
            &permit,
            python_rows,
        )?),
        (None, None) => None,
        _ => anyhow::bail!("Python completion missing"),
    };
    let cargo = match &plans.cargo {
        Some(plan) => Some(cargo::finish(&execution, selected, plan, id, cargo_rows)?),
        None => None,
    };
    let contract = match (&plans.contract, contract_permit, contract_row) {
        (Some(plan), Some(permit), Some(row)) => {
            Some(contract::finish(&execution, selected, plan, &permit, row)?)
        }
        (None, None, None) => None,
        _ => anyhow::bail!("contract completion missing"),
    };
    merge(
        id,
        cargo,
        python,
        contract,
        !selected.contract_targets.is_empty() || !selected.contract_test_paths.is_empty(),
    )
}
fn empty(id: &str, contracts: bool) -> Result<(crate::candidate_assessment::CheckResult, Value)> {
    use crate::candidate_assessment::{CheckResult, CheckStatus};
    ensure!(
        matches!(id, "targeted-tests" | "targeted-tests-full"),
        "invalid targeted check ID"
    );
    let findings = if contracts {
        vec![cargo::finding(
            id,
            "contract-runtime-unavailable",
            "selected contracts have not run".into(),
        )]
    } else {
        Vec::new()
    };
    Ok((
        CheckResult {
            check_id: id.into(),
            status: if contracts {
                CheckStatus::Error
            } else {
                CheckStatus::Skipped
            },
            duration_ms: 0,
            findings,
            files: Vec::new(),
            command: Vec::new(),
            tool_version: None,
            cache_key: None,
            cache_hit: false,
            exit_code: None,
            skipped_reason: if contracts {
                None
            } else {
                Some("no selected tests".into())
            },
            stdout_tail: String::new(),
            stderr_tail: String::new(),
            metrics: serde_json::Map::new(),
        },
        json!({"scope":"targeted_cargo_and_pytest","contract_runtime_pending":contracts,"review_complete":false}),
    ))
}
fn merge(
    id: &str,
    cargo: Option<(crate::candidate_assessment::CheckResult, Value)>,
    python: Option<(crate::candidate_assessment::CheckResult, Value)>,
    contract: Option<(crate::candidate_assessment::CheckResult, Value)>,
    contracts_pending: bool,
) -> Result<(crate::candidate_assessment::CheckResult, Value)> {
    use crate::candidate_assessment::CheckStatus;
    let python_evidence = python.as_ref().map(|(_, value)| value.clone());
    let cargo_evidence = cargo.as_ref().map(|(_, value)| value.clone());
    let contract_evidence = contract.as_ref().map(|(_, value)| value.clone());
    let contract_observed = contract.is_some();
    let python_observed = python.is_some();
    let mut rows = Vec::new();
    if let Some((row, _)) = cargo {
        rows.push(row)
    }
    if let Some((row, _)) = python {
        rows.push(row)
    }
    if let Some((row, _)) = contract {
        rows.push(row)
    }
    ensure!(!rows.is_empty(), "no targeted execution result");
    if python_observed || contract_observed {
        for row in &mut rows {
            let before = row.findings.len();
            row.findings.retain(|f| {
                !(python_observed && f.rule_id == "pytest-adapter-unavailable")
                    && !(contract_observed && f.rule_id == "contract-runtime-unavailable")
            });
            if before != row.findings.len()
                && row.status == CheckStatus::Error
                && row.findings.is_empty()
            {
                row.status = CheckStatus::Passed;
            }
        }
    }
    let mut result = rows.remove(0);
    for row in rows {
        result.findings.extend(row.findings);
        result.files.extend(row.files);
        if row.status == CheckStatus::Error {
            result.status = CheckStatus::Error
        } else if row.status == CheckStatus::Failed && result.status != CheckStatus::Error {
            result.status = CheckStatus::Failed
        }
    }
    if contracts_pending
        && !contract_observed
        && !result
            .findings
            .iter()
            .any(|f| f.rule_id == "contract-runtime-unavailable")
    {
        result.findings.push(cargo::finding(
            id,
            "contract-runtime-unavailable",
            "selected contracts have not run".into(),
        ));
        result.status = CheckStatus::Error;
    }
    result.files.sort();
    result.files.dedup();
    Ok((
        result,
        json!({"scope":"targeted_cargo_pytest_contracts", "cargo":cargo_evidence,"python":python_evidence,"contract":contract_evidence,"contract_runtime_pending":contracts_pending && !contract_observed,"review_complete":false}),
    ))
}

#[cfg(test)]
#[path = "candidate_targeted_execution_tests.rs"]
mod tests;
