//! Ordered regular command execution with explicit aggregate operator admission.
//! This produces fresh observations, never full-review or approval evidence.
#[path = "candidate_batch_run.rs"]
mod execution;
#[path = "candidate_batch_output.rs"]
mod output;
use crate::{
    candidate_execution_plan::{CheckPlan, Selection, SelectionArgs},
    process_signals::Interrupts,
};
use anyhow::{ensure, Context, Result};
use clap::Args;
use serde_json::{json, Value};
use std::{collections::BTreeSet, sync::Arc};

#[derive(Args)]
pub struct BatchArgs {
    #[command(flatten)]
    selection: SelectionArgs,
    /// Repeat exact active check IDs; omission selects the active profile.
    #[arg(long)]
    check: Vec<String>,
    /// Launch commands; otherwise prepare stages and report missing implementations.
    #[arg(long)]
    execute: bool,
}

pub fn run(args: BatchArgs) -> Result<Value> {
    let interrupts = if args.execute {
        Some(Interrupts::install()?)
    } else {
        None
    };
    let selection = Arc::new(Selection::load(&args.selection)?);
    let budget = selection
        .grants
        .batch
        .context("execute-batch requires an operator batch budget")?;
    let workers = selection.policy.document["max_workers"]
        .as_u64()
        .context("missing policy max_workers")?;
    let parallelism = budget.parallelism(workers, selection.grants.resources)?;
    let plans = select(&selection, &args.check)?;
    let mut report = selection.output();
    report["execution_scope"] = json!("selected_regular_commands");
    report["review_complete"] = json!(false);
    report["cache"] = json!("disabled_fresh_execution");
    report["executed"] = json!(args.execute);
    report["budget"] = serde_json::to_value(budget)?;
    report["budget"]["admitted_parallelism"] = json!(parallelism);
    report["unexecuted"] = json!(plans
        .iter()
        .filter(|p| p.scheduled && p.external.is_none())
        .map(|p| json!({"check_id":p.id,"kind":p.normalized["kind"],
            "reason":"scheduled check has no native batch implementation"}))
        .collect::<Vec<_>>());
    let quota = Arc::new(output::Quota::new(&report, &plans, args.execute)?);
    let (rows, cancelled) = if let Some(interrupts) = &interrupts {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .max_blocking_threads(parallelism + 2)
            .build()?;
        runtime.block_on(execution::run(
            selection,
            &plans,
            parallelism,
            interrupts,
            quota,
        ))?
    } else {
        (dry_run(&selection, &plans, &quota)?, false)
    };
    report["success"] = json!(
        !cancelled
            && rows.iter().all(|row| row["error"].is_null()
                && (row["skipped"] == true
                    || !args.execute
                    || row["result"]["status"] == "passed"))
    );
    report["cancelled"] = json!(cancelled);
    report["checks"] = json!(rows);
    output::bytes(&report, output::MAX_BYTES)?;
    Ok(report)
}

fn select(selection: &Selection, requested: &[String]) -> Result<Vec<Arc<CheckPlan>>> {
    ensure!(requested.len() <= 256, "batch check selection exceeds 256");
    let ids: BTreeSet<_> = requested.iter().map(String::as_str).collect();
    ensure!(ids.len() == requested.len(), "duplicate requested check ID");
    let checks = selection.checks()?;
    for id in &ids {
        ensure!(
            checks.iter().any(|c| c["check_id"].as_str() == Some(id)),
            "requested check {id} is absent or inactive in this profile"
        );
    }
    let selected: Vec<_> = checks
        .into_iter()
        .filter(|c| ids.is_empty() || c["check_id"].as_str().is_some_and(|id| ids.contains(id)))
        .collect();
    ensure!(selected.len() <= 256, "batch check selection exceeds 256");
    ensure!(
        selection
            .candidate
            .changes
            .len()
            .checked_mul(selected.len())
            .is_some_and(|n| n <= 10_000_000),
        "batch selection exceeds 10 million change/check pairs"
    );
    let mut plans = Vec::with_capacity(selected.len());
    let mut bytes = 0usize;
    for normalized in selected {
        let plan = selection.plan(normalized)?;
        for file in &plan.files {
            bytes = bytes
                .checked_add(file.len() + 8)
                .context("selected files size overflow")?;
            ensure!(
                bytes <= 32 * 1024 * 1024,
                "batch selected files exceed 32 MiB"
            );
        }
        plans.push(Arc::new(plan));
    }
    Ok(plans)
}

fn dry_run(
    selection: &Selection,
    plans: &[Arc<CheckPlan>],
    quota: &output::Quota,
) -> Result<Vec<Value>> {
    plans
        .iter()
        .map(|plan| {
            let mut row = output::row(plan, false);
            let prepared = if plan.scheduled && plan.external.is_some() {
                (|| -> Result<Value> {
                    Ok(json!([
                        plan.stage(selection, true)?.dry_run()?,
                        plan.stage(selection, false)?.dry_run()?
                    ]))
                })()
            } else {
                Ok(json!([]))
            };
            match prepared {
                Ok(stages) => row["stages"] = stages,
                Err(error) => return Ok(output::error(plan, false, &error)),
            }
            if let Err(error) = quota.retain(&row, plan, false) {
                return Ok(output::error(plan, false, &error));
            }
            Ok(row)
        })
        .collect()
}
