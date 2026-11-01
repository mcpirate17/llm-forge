use super::*;
use crate::{
    batch_execution::{self, Owners},
    candidate_execution_result::{self, VersionEvidence},
    confined_execution::Cancellation,
    process_signals::ChildSignalMask,
};

pub(super) async fn run(
    selection: Arc<Selection>,
    plans: &[Arc<CheckPlan>],
    parallelism: usize,
    interrupts: &Interrupts,
    quota: Arc<output::Quota>,
) -> Result<(Vec<Value>, bool)> {
    if plans.is_empty() {
        return Ok((Vec::new(), interrupts.interrupted()?));
    }
    let signal = interrupts.register_reactor()?;
    let pending = interrupts.interrupted()?;
    let mask = interrupts.child_mask();
    let batch =
        batch_execution::start(plans.to_vec(), parallelism, move |plan, cancel, owners| {
            let selection = selection.clone();
            let quota = quota.clone();
            async move {
                let row = check(selection, plan.clone(), mask, cancel, owners).await?;
                quota.retain(&row, &plan, true)?;
                Ok(row)
            }
        })?;
    let cancel = batch.cancellation();
    let wake = if pending { cancel.cancel() } else { Ok(()) };
    let wait = batch.wait();
    tokio::pin!(wait);
    let rows = tokio::select! {
        rows = &mut wait => { let rows = rows?; wake?; rows },
        received = async {
            let mut bytes = [0u8;std::mem::size_of::<libc::signalfd_siginfo>()];
            ensure!(signal.read(&mut bytes).await? > 0,"interrupt descriptor closed");
            Ok::<_,anyhow::Error>(())
        } => {
            let wake = cancel.cancel();
            let rows = wait.await?;
            // Never propagate a signal-observation error before owned cleanup.
            // Cleanup errors include retained recovery paths and take priority.
            received?;
            wake?;
            rows
        }
    };
    let cancelled = cancel.is_cancelled() || interrupts.interrupted()?;
    ensure!(
        rows.len() == plans.len(),
        "batch result count differs from selection"
    );
    Ok((
        rows.into_iter()
            .zip(plans)
            .map(|(row, plan)| row.unwrap_or_else(|error| output::error(plan, true, &error)))
            .collect(),
        cancelled,
    ))
}

async fn prepare(
    selection: Arc<Selection>,
    plan: Arc<CheckPlan>,
    version: bool,
) -> Result<crate::candidate_execution_plan::Stage> {
    tokio::task::spawn_blocking(move || plan.stage(&selection, version))
        .await
        .context("candidate stage preparation task failed")?
}

async fn check(
    selection: Arc<Selection>,
    plan: Arc<CheckPlan>,
    mask: ChildSignalMask,
    cancel: Arc<Cancellation>,
    owners: Arc<Owners>,
) -> Result<Value> {
    let mut row = output::row(&plan, true);
    if !plan.scheduled || plan.external.is_none() {
        return Ok(row);
    }
    ensure!(
        !cancel.is_cancelled(),
        "batch cancelled before version preparation"
    );
    let (version_data, version_report, version_argv) =
        prepare(selection.clone(), plan.clone(), true)
            .await?
            .execute_batch(&selection.grants, mask, cancel.clone(), owners.clone())
            .await?;
    let evidence = candidate_execution_result::version(
        &version_report,
        plan.version_limits(&selection)?.max_output_chars,
    )?;
    let (stages, report, argv) = if matches!(evidence, VersionEvidence::Available(_)) {
        ensure!(
            !cancel.is_cancelled(),
            "batch cancelled before command preparation"
        );
        let (data, report, argv) = prepare(selection.clone(), plan.clone(), false)
            .await?
            .execute_batch(&selection.grants, mask, cancel, owners)
            .await?;
        (vec![version_data, data], report, argv)
    } else {
        (vec![version_data], version_report, version_argv)
    };
    row["stages"] = json!(stages);
    row["result"] = serde_json::to_value(candidate_execution_result::map_result(
        &plan.normalized,
        &plan.files,
        &argv,
        Some(&evidence),
        &report,
    )?)?;
    Ok(row)
}
