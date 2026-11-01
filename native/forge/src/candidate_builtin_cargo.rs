//! Native Cargo audit consumes only separately pinned operator evidence.
use super::*;
use conductor_native::{candidate_cargo_audit, candidate_cargo_evidence, candidate_cargo_run};
pub(super) fn run(
    selection: &Selection,
    plan: &CheckPlan,
    workspace: &Workspace,
    source: &Source,
    cancel: Option<&Cancellation>,
    execute: bool,
    started: Instant,
) -> Result<Value> {
    let directory = workspace.directory()?;
    let snapshot = workspace.path();
    let spec = selection
        .grants
        .cargo_audit_evidence
        .as_ref()
        .context("cargo-audit requires operator cargo_audit_evidence grants")?;
    let evidence = candidate_cargo_evidence::load(
        spec,
        &[source.root(), &snapshot],
        candidate_cargo_evidence::now().map_err(anyhow::Error::msg)?,
    )
    .map_err(anyhow::Error::msg)?;
    live(cancel)?;
    let files: Vec<&str> = plan.files.iter().map(String::as_str).collect();
    if !execute {
        candidate_cargo_audit::plan(&directory, &files).map_err(anyhow::Error::msg)?;
        return Ok(Value::Null);
    }
    let evaluated = candidate_cargo_run::evaluate(&directory, &files, &evidence, || {
        live(cancel).map_err(|_| "native cargo audit cancelled".to_owned())
    })
    .map_err(anyhow::Error::msg)?;
    live(cancel)?;
    crate::json_canon::encoded_size(&evaluated, MAX_RESULT_BYTES)?;
    result::finish(&plan.id, plan.files.clone(), evaluated, started)
}
