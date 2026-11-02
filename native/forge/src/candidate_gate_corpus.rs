//! Hold the native corpus observations until gate disposition.
use super::{types::Phase, GateArgs};
use crate::{
    candidate_execution_plan::Selection,
    candidate_mutation_runner::{RunnerBundle, RunnerBundleSpec},
    candidate_review_mutation::{inputs::Capture, registry},
    candidate_snapshot::Snapshot,
    candidate_source::Source,
    mutation_corpus_audit::{self, Interpreter},
    mutation_corpus_patch::Checker,
};
use anyhow::{Context, Result};
use serde_json::{json, Map, Value};
fn delta_rows(delta: &Value, side: &str) -> Result<Map<String, Value>> {
    let mut rows = Map::new();
    for key in conductor_native::mutation_corpus_delta::BASELINE_KEYS {
        let field = format!("{side}_{key}");
        let values = delta[&field]
            .as_array()
            .context("corpus delta array missing")?;
        if !values.is_empty() {
            rows.insert(key.into(), Value::Array(values.clone()));
        }
    }
    Ok(rows)
}
fn describe(label: &str, rows: &Map<String, Value>) -> Result<String> {
    let mut parts = Vec::new();
    for key in conductor_native::mutation_corpus_delta::BASELINE_KEYS {
        let Some(values) = rows.get(key) else {
            continue;
        };
        let values = values.as_array().context("corpus delta IDs missing")?;
        let names: Vec<_> = values
            .iter()
            .take(3)
            .map(|value| value.as_str().context("corpus delta ID is not a string"))
            .collect::<Result<_>>()?;
        parts.push(format!(
            "{key} {} ({}{})",
            values.len(),
            names.join(", "),
            if values.len() > 3 { "..." } else { "" }
        ));
    }
    Ok(format!("{label} {}", parts.join("; ")))
}
fn phase(report: Value, code: u8) -> Result<Phase> {
    let status = report["status"].as_str().context("corpus status missing")?;
    let delta = &report["reproducibility"]["baseline"];
    let new = delta_rows(delta, "new")?;
    let resolved = delta_rows(delta, "resolved")?;
    let detail = if code == 0 {
        format!(
            "{} registered campaign(s) reproduce at the recorded baseline",
            report["campaigns"]
        )
    } else {
        let mut segments = Vec::new();
        if !new.is_empty() {
            segments.push(describe("new", &new)?);
        }
        if !resolved.is_empty() {
            segments.push(describe("baseline entries no longer failing:", &resolved)?);
        }
        format!("{status}: {}", segments.join(" | "))
    };
    Ok(Phase::new(
        "mutation-corpus",
        code == 0,
        detail,
        json!({"status":status,"exit_code":code,
        "campaigns":report["campaigns"],"new":new,"resolved":resolved,"git_build_profile":report["git_build_profile"]}),
    ))
}
pub(super) struct Corpus<'a> {
    capture: Capture<'a>,
    bundle: RunnerBundle,
    checker: Checker<'a>,
    interpreters: Vec<Interpreter>,
    pub phase: Phase,
}
impl Corpus<'_> {
    pub fn verify(&self) -> Result<()> {
        self.capture.verify()?;
        self.bundle.verify()?;
        self.checker.verify()?;
        for item in &self.interpreters {
            item.verify()?;
        }
        Ok(())
    }
}
pub(super) fn collect<'a>(
    args: &GateArgs,
    selection: &'a Selection,
    source: &Source,
    snapshot: &'a Snapshot,
    interrupts: &'a crate::process_signals::Interrupts,
) -> Result<Corpus<'a>> {
    let mut capture = Capture::new(
        source,
        snapshot,
        &selection.candidate,
        &selection.policy,
        selection.date(),
    )?;
    let config = capture.config()?;
    let registry = registry::registry(&mut capture, config.as_ref())?
        .context("candidate mutation registry absent")?;
    let catalog = registry::catalog(&mut capture, &registry, &[])?;
    let configured =
        conductor_native::project_paths::mutation_registry_from_config(config.as_ref())
            .map_err(anyhow::Error::msg)?;
    let path = crate::mutation_corpus_cli::baseline_path(&configured, None)?;
    let baseline = if capture.inputs.entries.contains_key(&path) {
        Some(capture.json(&path)?)
    } else {
        capture.inputs.absent(&path)?;
        None
    };
    let manifest = args
        .review
        .mutation_runner_bundle
        .as_deref()
        .context("corpus audit requires a pinned mutation runner bundle")?;
    let digest = args
        .review
        .mutation_runner_sha256
        .as_deref()
        .context("corpus audit requires the mutation runner bundle SHA-256")?;
    let bundle = RunnerBundle::capture(
        RunnerBundleSpec {
            manifest,
            sha256: digest,
        },
        source,
        snapshot,
        std::slice::from_ref(&selection.grants.scratch_parent),
    )?;
    let profile = crate::mutation_corpus_git_profile::Profile::open(
        &args.git_build_profile,
        &args.git_build_profile_sha256,
        &args.git_sha256,
        source.root(),
    )?;
    let checker =
        Checker::open(snapshot, &args.git, &args.git_sha256, profile)?.with_interrupts(interrupts);
    let changed: Vec<_> = selection
        .candidate
        .changes
        .iter()
        .filter(|row| {
            !row.deleted()
                && matches!(
                    row.status.as_bytes().first(),
                    Some(b'A' | b'C' | b'M' | b'R')
                )
        })
        .map(|row| row.path.clone())
        .collect();
    let (report, code, _, interpreters) = mutation_corpus_audit::audit(
        &catalog,
        &mut capture,
        &bundle,
        &checker,
        mutation_corpus_audit::AuditOptions {
            changed: &changed,
            baseline: baseline.as_ref(),
            root: source.root(),
            summary: true,
            record: false,
        },
    )?;
    let phase = phase(report, code)?;
    let result = Corpus {
        capture,
        bundle,
        checker,
        interpreters,
        phase,
    };
    result.verify()?;
    Ok(result)
}

#[cfg(test)]
#[path = "candidate_gate_corpus_tests.rs"]
mod tests;
