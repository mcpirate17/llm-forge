//! Run one policy command with separately supplied operator capabilities.
use crate::{
    candidate_execution_plan::{CheckPlan, Selection, SelectionArgs},
    candidate_execution_result::{self, VersionEvidence},
    process_signals::Interrupts,
};
use anyhow::{ensure, Context, Result};
use clap::Args;
use serde_json::{json, Value};

#[derive(Args)]
pub struct ExecuteArgs {
    #[command(flatten)]
    selection: SelectionArgs,
    /// Exact active external check ID; does not execute a whole review pipeline.
    #[arg(long)]
    check: String,
    /// Launch both stages; otherwise only prepare and validate.
    #[arg(long)]
    execute: bool,
}
pub fn run(args: ExecuteArgs) -> Result<Value> {
    let interrupts = if args.execute {
        Some(Interrupts::install()?)
    } else {
        None
    };
    let selection = Selection::load(&args.selection)?;
    let normalized = selection
        .checks()?
        .into_iter()
        .find(|check| check["check_id"].as_str() == Some(args.check.as_str()))
        .context("requested check is absent or inactive in this profile")?;
    let plan = selection.plan(normalized)?;
    ensure!(
        plan.external.is_some(),
        "execute requires an external command check"
    );
    let mut output = selection.output();
    output
        .as_object_mut()
        .context("invalid selection output")?
        .extend(
            plan.output(args.execute)
                .as_object()
                .context("invalid check output")?
                .clone(),
        );
    if !plan.scheduled {
        output["skipped_reason"] = json!("no selected candidate files");
        return Ok(output);
    }
    if !args.execute {
        let version = plan.stage(&selection, true)?.dry_run()?;
        let command = plan.stage(&selection, false)?.dry_run()?;
        output["stages"] = json!([version, command]);
        return Ok(output);
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(2)
        .build()?;
    runtime.block_on(execute(
        &selection,
        &plan,
        interrupts.as_ref().context("interrupt owner missing")?,
        &mut output,
    ))?;
    Ok(output)
}
async fn execute(
    selection: &Selection,
    plan: &CheckPlan,
    interrupts: &Interrupts,
    output: &mut Value,
) -> Result<()> {
    let (version_data, version_report, version_argv) = plan
        .stage(selection, true)?
        .execute(&selection.grants, interrupts)
        .await?;
    let evidence = candidate_execution_result::version(
        &version_report,
        plan.version_limits(selection)?.max_output_chars,
    )?;
    let (stages, report, command) = if matches!(evidence, VersionEvidence::Available(_)) {
        let (data, report, argv) = plan
            .stage(selection, false)?
            .execute(&selection.grants, interrupts)
            .await?;
        (vec![version_data, data], report, argv)
    } else {
        (vec![version_data], version_report, version_argv)
    };
    output["stages"] = json!(stages);
    output["result"] = serde_json::to_value(candidate_execution_result::map_result(
        &plan.normalized,
        &plan.files,
        &command,
        Some(&evidence),
        &report,
    )?)?;
    Ok(())
}
