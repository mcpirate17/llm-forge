//! Assess supplied findings against the exact candidate; never verify execution.
use crate::{candidate_assessment, candidate_plan};
use anyhow::{ensure, Context, Result};
use clap::Args;
use serde_json::{json, Value};
use std::path::PathBuf;

const MAX_RESULTS_BYTES: usize = 16 * 1024 * 1024;

#[derive(Args)]
pub struct AssessArgs {
    #[command(flatten)]
    plan: candidate_plan::PlanArgs,
    /// JSON array of check results. Supplied results are not execution evidence.
    #[arg(long)]
    results: PathBuf,
}

pub fn run(args: AssessArgs) -> Result<Value> {
    let bytes = crate::bounded_file::read(&args.results, MAX_RESULTS_BYTES)?;
    let text = std::str::from_utf8(&bytes).context("candidate results must be UTF-8")?;
    let value = crate::tool_schema::parse_document_with_limit(text, MAX_RESULTS_BYTES)
        .context("invalid candidate result JSON")?;
    let array = value
        .as_array()
        .context("candidate results must be an array")?;
    ensure!(
        array.len() <= 20_000,
        "candidate results exceed 20000 entries"
    );
    let results: Vec<candidate_assessment::CheckResult> =
        serde_json::from_value(value).context("invalid candidate result fields")?;
    let prepared = candidate_plan::prepare(args.plan)?;
    let assessment = candidate_assessment::assess(&prepared.policy, &prepared.candidate, &results)?;
    Ok(json!({"schema_version":1,"assessment_only":true,
        "plan":prepared.report,"assessment":assessment}))
}
