//! Snapshot-backed native checks. No analyzer process, result cache or approval.
#[path = "candidate_builtin_integrity.rs"]
mod integrity;
#[path = "candidate_builtin_result.rs"]
mod result;
use crate::{
    candidate_execution_plan::{CheckPlan, Selection},
    candidate_execution_workspace::Workspace,
    candidate_source::Source,
    confined_execution::Cancellation,
};
use anyhow::{ensure, Context, Result};
use cap_std::fs::Dir;
use serde_json::{json, Value};
use std::{sync::Arc, time::Instant};

const MAX_INPUT_BYTES: usize = 32 * 1024 * 1024;
const MAX_RESULT_BYTES: usize = 32 * 1024 * 1024;
const IMPLEMENTATION: &str = "forge-builtin/1";
struct Prepared {
    payload: Value,
    files: Vec<String>,
}

pub(crate) fn supports(plan: &CheckPlan) -> bool {
    plan.normalized["kind"] == "builtin"
        && matches!(
            plan.id.as_str(),
            "candidate-integrity" | "dependency-integrity"
        )
}
pub(crate) fn metadata(plan: &CheckPlan) -> Value {
    let mut value = json!({"implementation":IMPLEMENTATION});
    if plan.id == "candidate-integrity" {
        let (major, minor, patch) = conductor_native::candidate_unicode::UNICODE_VERSION;
        value["unicode_version"] = json!(format!("{major}.{minor}.{patch}"));
    }
    value
}
pub(crate) fn validate(selection: &Selection, plan: &CheckPlan) -> Result<()> {
    if plan.scheduled && supports(plan) && plan.id == "candidate-integrity" {
        ensure!(
            selection.surface.is_some(),
            "candidate-integrity requires explicit --surface"
        );
    }
    Ok(())
}
fn live(cancel: Option<&Cancellation>) -> Result<()> {
    ensure!(
        !cancel.is_some_and(Cancellation::is_cancelled),
        "builtin check cancelled"
    );
    Ok(())
}
pub(crate) fn prepare(selection: &Selection, plan: &CheckPlan) -> Result<()> {
    owned(selection, plan, None, false).map(|_| ())
}
pub(crate) async fn execute(
    selection: Arc<Selection>,
    plan: Arc<CheckPlan>,
    cancel: Arc<Cancellation>,
) -> Result<Value> {
    // The batch owner joins this task even after cancellation. The owned worker
    // completes bounded evaluation and snapshot cleanup before its future ends.
    tokio::task::spawn_blocking(move || owned(&selection, &plan, Some(&cancel), true))
        .await
        .context("builtin worker stopped before returning cleanup evidence")?
}
fn owned(
    selection: &Selection,
    plan: &CheckPlan,
    cancel: Option<&Cancellation>,
    execute: bool,
) -> Result<Value> {
    ensure!(supports(plan), "unsupported native builtin");
    validate(selection, plan)?;
    live(cancel)?;
    let started = Instant::now();
    let source = selection.source()?;
    let workspace = Workspace::new(&source, &selection.candidate.tree_oid, &selection.grants)?;
    let attempt = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<Value> {
        let directory = workspace.directory()?;
        let prepared = match plan.id.as_str() {
            "candidate-integrity" => integrity::prepare(selection, &source, &directory, cancel)?,
            "dependency-integrity" => dependency(selection, cancel)?,
            _ => anyhow::bail!("unsupported native builtin"),
        };
        crate::json_canon::encoded_size(&prepared.payload, MAX_INPUT_BYTES)?;
        live(cancel)?;
        if !execute {
            return Ok(Value::Null);
        }
        let evaluated = if plan.id == "dependency-integrity" {
            conductor_native::candidate_checks::dependency_integrity_with(
                &prepared.payload,
                |path| match directory.metadata(path) {
                    Ok(metadata) => Ok(metadata.is_file()),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
                    Err(error) => Err(format!("cannot inspect candidate lockfile {path}: {error}")),
                },
            )
        } else {
            conductor_native::candidate_checks::evaluate(&plan.id, &prepared.payload)
        }
        .map_err(anyhow::Error::msg)?;
        live(cancel)?;
        crate::json_canon::encoded_size(&evaluated, MAX_RESULT_BYTES)?;
        result::finish(&plan.id, prepared.files, evaluated, started)
    }))
    .map_err(|_| anyhow::anyhow!("native builtin evaluator panicked"))
    .and_then(|result| result);
    workspace.discard().with_context(|| match &attempt {
        Err(error) => format!("builtin cleanup failed after {error:#}"),
        Ok(_) => "builtin cleanup failed".into(),
    })?;
    live(cancel)?;
    attempt
}
fn dependency(selection: &Selection, cancel: Option<&Cancellation>) -> Result<Prepared> {
    let mut files = Vec::new();
    let mut budget = Budget::default();
    for change in &selection.candidate.changes {
        live(cancel)?;
        if change.classes.iter().any(|class| class == "dependency") {
            budget.add(change.path.len() + 8)?;
            files.push(change.path.clone());
        }
    }
    files.sort_unstable();
    files.dedup();
    Ok(Prepared {
        payload: json!({"paths":files}),
        files,
    })
}
#[derive(Default)]
struct Budget {
    bytes: usize,
}
impl Budget {
    fn add(&mut self, amount: usize) -> Result<()> {
        self.bytes = self
            .bytes
            .checked_add(amount)
            .context("builtin payload size overflow")?;
        ensure!(
            self.bytes <= MAX_INPUT_BYTES,
            "builtin payload exceeds 32 MiB"
        );
        Ok(())
    }
}
