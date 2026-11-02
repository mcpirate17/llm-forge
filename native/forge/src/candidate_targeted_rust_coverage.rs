//! Captured Git changed-line scope for supervised Rust coverage.
use crate::{
    candidate_execution_plan::Selection,
    candidate_execution_workspace::Workspace,
    candidate_python_coverage::{ChangedFile, Coverage},
    candidate_source::lines::RangeScope,
    candidate_targeted_cargo::Job,
};
use anyhow::{ensure, Context, Result};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
#[derive(Debug, Serialize)]
pub(crate) struct Plan {
    pub cov: String,
    pub profdata: String,
    pub files: BTreeMap<String, Vec<ChangedFile>>,
    pub unsupported: Vec<String>,
    pub high: f64,
    pub other: f64,
}
fn tool(execution: &Selection, name: &str) -> Result<String> {
    Ok(execution
        .grants
        .programs
        .get(name)
        .with_context(|| format!("{name} operator grant missing"))?
        .to_str()
        .context("LLVM tool path not UTF-8")?
        .to_owned())
}
fn lines(
    ranges: &[crate::candidate_source::lines::LineRange],
    total: &mut usize,
) -> Result<BTreeSet<u32>> {
    let mut out = BTreeSet::new();
    let mut last = 0;
    for range in ranges {
        ensure!(
            range.start > last && range.end >= range.start,
            "invalid Rust changed-line ranges"
        );
        let count = usize::try_from(u64::from(range.end) - u64::from(range.start) + 1)?;
        *total = total
            .checked_add(count)
            .context("Rust changed-line overflow")?;
        ensure!(*total <= 65_536, "Rust changed lines exceed limit");
        out.extend(range.start..=range.end);
        last = range.end;
    }
    Ok(out)
}
fn only_lf(text: &[u8]) -> bool {
    text.iter()
        .enumerate()
        .all(|(i, b)| *b != b'\r' || text.get(i + 1) == Some(&b'\n'))
}
pub(crate) fn plan(execution: &Selection, jobs: &[Job]) -> Result<Plan> {
    let source = execution.source()?;
    let changes: BTreeMap<_, _> = execution
        .candidate
        .changes
        .iter()
        .filter(|c| !c.deleted() && !c.classes.iter().any(|v| v == "test"))
        .map(|c| (c.path.as_str(), c))
        .collect();
    let mut owners = BTreeMap::new();
    let mut unsupported = BTreeSet::new();
    for job in jobs {
        for item in &job.files {
            if changes.contains_key(item.path.as_str()) {
                if !item.path.ends_with(".rs") {
                    unsupported.insert(item.path.clone());
                    continue;
                }
                ensure!(
                    owners
                        .insert(item.path.clone(), (job.manifest.clone(), item.oid.clone()))
                        .is_none(),
                    "Rust source belongs to multiple Cargo jobs"
                );
            }
        }
    }
    ensure!(
        owners.len() <= 4096 && unsupported.len() <= 4096,
        "Rust coverage source count exceeds limit"
    );
    let paths: Vec<_> = owners.keys().map(String::as_str).collect();
    let ranges = source.changed_ranges(&execution.candidate, &paths)?;
    let workspace = Workspace::new(&source, &execution.candidate.tree_oid, &execution.grants)?;
    let mut files: BTreeMap<String, Vec<ChangedFile>> = BTreeMap::new();
    let mut held = Vec::new();
    let mut total_lines = 0usize;
    let mut total_bytes = 0usize;
    for row in ranges.files {
        if row.scope != RangeScope::Changed {
            continue;
        }
        let (manifest, oid) = owners
            .get(&row.path)
            .context("Rust source binding missing")?;
        let (bound, bytes) =
            crate::candidate_inputs::snapshot(&workspace.path(), &row.path, oid, 1024 * 1024)?;
        total_bytes = total_bytes
            .checked_add(bytes.len())
            .context("Rust source bytes overflow")?;
        ensure!(
            total_bytes <= 32 * 1024 * 1024
                && std::str::from_utf8(&bytes).is_ok()
                && only_lf(&bytes),
            "Rust source is oversized, non-UTF-8 or has unsupported bare CR"
        );
        let risk = changes
            .get(row.path.as_str())
            .context("Rust change missing")?
            .risk
            == "high";
        files
            .entry(manifest.clone())
            .or_default()
            .push(ChangedFile {
                path: row.path,
                absolute_path: String::new(),
                lines: lines(&row.ranges, &mut total_lines)?,
                high_risk: risk,
            });
        held.push(bound);
    }
    for item in &held {
        item.verify()?;
    }
    workspace.discard()?;
    Ok(Plan {
        cov: tool(execution, "llvm-cov")?,
        profdata: tool(execution, "llvm-profdata")?,
        files,
        unsupported: unsupported.into_iter().collect(),
        high: execution.policy.document["high_risk_coverage_threshold"]
            .as_f64()
            .context("high risk threshold missing")?,
        other: execution.policy.document["coverage_threshold"]
            .as_f64()
            .context("coverage threshold missing")?,
    })
}
pub(crate) fn evaluate(plan: &Plan, manifest: &str, root: &str, bytes: &[u8]) -> Result<Coverage> {
    let files = plan.files.get(manifest).map(Vec::as_slice).unwrap_or(&[]);
    crate::candidate_rust_coverage::evaluate(bytes, root, files, plan.high, plan.other)
}
#[cfg(test)]
#[path = "candidate_targeted_rust_coverage_tests.rs"]
mod tests;
