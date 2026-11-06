//! Equivalence check integration for the native review coordinator.
use super::{result, ReviewArgs};
use crate::{
    candidate_assessment::{self, CheckResult, CheckStatus, Finding},
    candidate_equivalence_inputs::{self, Captured},
    candidate_equivalence_stage,
    candidate_execution_plan::{CheckPlan, Selection},
    candidate_snapshot::Snapshot,
    process_signals::Interrupts,
};
use anyhow::{ensure, Context, Result};
use serde_json::{json, Value};
use std::{sync::Arc, time::Instant};

pub(super) fn collect<'a>(
    args: &ReviewArgs,
    execution: &'a Arc<Selection>,
    snapshot: &'a Snapshot,
    plan: &CheckPlan,
    runtime: &tokio::runtime::Runtime,
    interrupts: &Interrupts,
    parallelism: usize,
) -> Result<(CheckResult, Value, Captured<'a>)> {
    let started = Instant::now();
    let store = args
        .equivalence_store
        .as_deref()
        .zip(args.equivalence_store_sha256.as_deref());
    let captured = candidate_equivalence_inputs::capture(
        execution,
        snapshot,
        store,
        args.equivalence_artifacts.as_deref(),
        parallelism,
    )?;
    let mut row = result::result("equivalence-probe", CheckStatus::Passed);
    if captured.targets.is_empty() {
        return Ok((row, captured.evidence.clone(), captured));
    }
    let guest = args
        .equivalence_guest
        .as_deref()
        .context("equivalence guest path not granted")?;
    let hash = args
        .equivalence_guest_sha256
        .as_deref()
        .context("equivalence guest SHA-256 not granted")?;
    let prepared = candidate_equivalence_stage::prepare(
        execution,
        &snapshot.path(),
        &plan.normalized,
        candidate_equivalence_stage::Probe {
            guest_path: guest,
            guest_sha256: hash,
            targets: &captured.targets,
            waivers: captured.waivers.as_deref(),
            timeout_seconds: captured.timeout_seconds,
            jobs: captured.jobs,
        },
    )?;
    let (stage, report, bytes, summary_sha256) =
        runtime.block_on(candidate_equivalence_stage::execute(
            prepared,
            Arc::new(execution.grants.clone()),
            interrupts,
        ))?;
    captured.verify()?;
    let text = std::str::from_utf8(&bytes).context("equivalence summary is not UTF-8")?;
    let summary: Value = serde_json::from_str(text)?;
    let (findings, metrics) = slop_core::gate_report::findings(text, None)?;
    let mut findings: Vec<Value> = serde_json::from_str(&findings)?;
    let publication = captured
        .artifacts
        .as_ref()
        .map(|destination| destination.publish(&bytes, report.cleanup_complete));
    if let Some(Err(error)) = &publication {
        findings.push(json!({"rule_id":"run-artifact-unwritten", "severity":"medium",
            "message":format!("this run's summary was not recorded ({error:#}), so the backlog will not see it")}));
    }
    ensure!(findings.len() <= 4096, "equivalence findings exceed 4096");
    for item in &mut findings {
        item["check_id"] = json!("equivalence-probe");
    }
    let mut typed: Vec<Finding> = serde_json::from_value(json!(findings))?;
    for finding in &mut typed {
        candidate_assessment::finalize_finding(finding);
    }
    if !typed.is_empty() {
        row.status = CheckStatus::Failed;
    }
    row.findings = typed;
    row.files = captured.targets.iter().map(|v| v.0.clone()).collect();
    row.duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    row.metrics = serde_json::from_str(&metrics)?;
    row.tool_version = Some("native-equivalence-guest/1".into());
    row.exit_code = report.exit_code.map(i64::from);
    let evidence = json!({"inputs":captured.evidence,"stage":stage,"summary_sha256":summary_sha256,
        "guest_sha256":hash,"summary":summary,
        "published_artifact":publication.and_then(Result::ok),"guest_output_authenticated":false,"approval":false});
    Ok((row, evidence, captured))
}
