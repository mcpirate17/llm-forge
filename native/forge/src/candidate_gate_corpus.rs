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
    bundle: Option<RunnerBundle>,
    native_pin: Option<crate::candidate_native_runner_pin::NativeRunnerPin>,
    native_lock: Option<crate::candidate_inputs::BoundFile>,
    checker: Checker<'a>,
    interpreters: Vec<Interpreter>,
    pub phase: Phase,
}
impl Corpus<'_> {
    pub fn verify(&self) -> Result<()> {
        self.capture.verify()?;
        if let Some(bundle) = &self.bundle {
            bundle.verify()?;
        }
        if let Some(pin) = &self.native_pin {
            pin.verify()?;
        }
        if let Some(lock) = &self.native_lock {
            lock.verify()?;
        }
        self.checker.verify()?;
        for item in &self.interpreters {
            item.verify()?;
        }
        Ok(())
    }
}
fn require_installed_lock(
    commit: Option<&str>,
    lock: Option<crate::candidate_inputs::BoundFile>,
    stamp: &crate::candidate_engine_identity::BuildStamp,
) -> Result<crate::candidate_inputs::BoundFile> {
    anyhow::ensure!(
        commit == Some(stamp.git_commit),
        "candidate uv.lock does not pin the running native CLI revision"
    );
    lock.context("installed candidate runner requires held uv.lock")
}
fn capture_bundle(
    args: &GateArgs,
    selection: &Selection,
    source: &Source,
    snapshot: &Snapshot,
) -> Result<Option<RunnerBundle>> {
    Ok(
        match (
            &args.review.mutation_runner_bundle,
            &args.review.mutation_runner_sha256,
        ) {
            (Some(manifest), Some(sha256)) => Some(RunnerBundle::capture(
                RunnerBundleSpec { manifest, sha256 },
                source,
                snapshot,
                std::slice::from_ref(&selection.grants.scratch_parent),
            )?),
            (None, None) => None,
            _ => anyhow::bail!("runner bundle and sha256 must be supplied together"),
        },
    )
}
type GateNative = (
    Option<crate::candidate_native_runner_pin::NativeRunnerPin>,
    Option<crate::candidate_inputs::BoundFile>,
);
fn capture_native(
    args: &GateArgs,
    selection: &Selection,
    source: &Source,
    snapshot: &Snapshot,
    stamp: &crate::candidate_engine_identity::BuildStamp,
    capture: &mut Capture<'_>,
) -> Result<GateNative> {
    let native_pin = match (
        &args.review.native_runner_pin,
        &args.review.native_runner_pin_sha256,
    ) {
        (Some(path), Some(sha256)) => Some(
            crate::candidate_native_runner_pin::NativeRunnerPin::capture(
                path,
                sha256,
                &[
                    source.root().to_owned(),
                    source.common_dir().to_owned(),
                    snapshot.path(),
                    selection.grants.scratch_parent.clone(),
                ],
                stamp,
            )?,
        ),
        (None, None) => None,
        _ => anyhow::bail!("native runner pin and sha256 must be supplied together"),
    };
    let native_lock = if native_pin.is_some() && capture.native_inventory_absent()? {
        let entries = source.list_tree(&selection.candidate.tree_oid)?;
        let (commit, _, lock) =
            crate::candidate_engine_identity::installed_pin(snapshot, &entries)?;
        Some(require_installed_lock(commit.as_deref(), lock, stamp)?)
    } else {
        None
    };
    Ok((native_pin, native_lock))
}
pub(super) fn collect<'a>(
    args: &GateArgs,
    selection: &'a Selection,
    source: &Source,
    snapshot: &'a Snapshot,
    interrupts: &'a crate::process_signals::Interrupts,
    stamp: &crate::candidate_engine_identity::BuildStamp,
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
    let baseline = crate::mutation_corpus_cli::capture_baseline(&mut capture, &path)?;
    let bundle = capture_bundle(args, selection, source, snapshot)?;
    let (native_pin, native_lock) =
        capture_native(args, selection, source, snapshot, stamp, &mut capture)?;
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
    let (report, code, _, interpreters) = mutation_corpus_audit::audit_installed(
        &catalog,
        &mut capture,
        bundle.as_ref(),
        native_pin.as_ref(),
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
        native_pin,
        native_lock,
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
