//! Shared generated-receipt rules for ambient and trusted-captured inputs.
use super::*;
use crate::mutation_native_runner as native_runner;
pub(super) fn generated_receipt_errors(
    payload: &Map<String, Value>,
    campaign: &CampaignContract,
    context: &ValidationContext<'_>,
) -> Vec<Rejection> {
    generated_errors_with(
        payload,
        campaign,
        context.runner,
        |path| {
            if path == native_runner::FINGERPRINT_KEY {
                let files = crate::source_fingerprint::files(context.repo_root).ok()?;
                crate::source_fingerprint::fingerprint(context.repo_root, &files).ok()
            } else {
                sha256_file(&binding_root(context, path).join(path))
            }
        },
        |recorded| lineage_accepts(recorded, context.package_root),
    )
}
/// `conductor/...` bindings resolve from the package's parent; native runner
/// sources are spelled from the repository root.
fn binding_root<'a>(context: &ValidationContext<'a>, path: &str) -> &'a Path {
    if path.starts_with("native/") {
        context.repo_root
    } else {
        context.package_root
    }
}
pub(crate) fn generated_errors_with(
    payload: &Map<String, Value>,
    campaign: &CampaignContract,
    runner: &RunnerState,
    hash: impl Fn(&str) -> Option<String>,
    lineage: impl Fn(Option<&Value>) -> bool,
) -> Vec<Rejection> {
    generated_errors_with_installed(payload, campaign, runner, hash, lineage, None, None, None)
}

pub(crate) fn generated_errors_with_installed(
    payload: &Map<String, Value>,
    campaign: &CampaignContract,
    runner: &RunnerState,
    hash: impl Fn(&str) -> Option<String>,
    lineage: impl Fn(Option<&Value>) -> bool,
    installed: Option<&native_runner::InstalledIdentity>,
    supporting: Option<&crate::mutation_cargo_supporting_binding::SupportingInputs>,
    supporting_error: Option<&str>,
) -> Vec<Rejection> {
    let mut errors = Vec::new();
    if payload.get("schema_version").and_then(Value::as_str) != Some(RECEIPT_SCHEMA) {
        errors.push(reject(
            RejectionKind::SchemaError,
            "receipt schema is not current",
        ));
    }
    // A patch receipt carries the same campaign fields, so without this a hand-authored receipt
    // could be offered as evidence for a generated campaign and skip the ratchet entirely.
    if payload.get("mutants_are_generated") != Some(&Value::Bool(true)) {
        errors.push(reject(
            RejectionKind::SchemaError,
            "receipt does not record engine-generated mutants",
        ));
    }
    match payload.get("status").and_then(Value::as_str) {
        Some("PASS") => {}
        other => {
            // A receipt that did not pass is not evidence for one reason: it
            // did not pass. RATCHET_HELD lands here too, by design -- a held
            // ratchet is honest debt to re-run, not a receipt the validator
            // cannot read. Everything below (survivor rows, score, outcome
            // accounting) presumes a PASS claim; judged on a held run it
            // fragments one debt verdict into a wall of defect-class noise,
            // which is exactly what the kinds exist to prevent. The two shape
            // checks above still fire alongside: a wrong schema or a
            // hand-authored patch offered to a generated campaign is a broken
            // receipt whichever way its status reads.
            return vec![reject(
                RejectionKind::NotPass,
                format!("status={}", python_repr(other.map(Value::from).as_ref())),
            )];
        }
    }
    errors.extend(identity_errors(payload, campaign));
    if native_runner::declared(payload)
        && campaign.generated_value.is_none()
        && campaign.mutation_engine == "cargo-mutants"
    {
        if let Err(reason) = crate::mutation_cargo_supporting_binding::compare(
            payload.get("supporting_inputs"),
            supporting,
        ) {
            let detail =
                supporting_error.map_or(reason.clone(), |error| format!("{reason}: {error}"));
            errors.push(reject(RejectionKind::NotPass, detail));
        }
    }

    if native_runner::declared(payload) {
        if let Some(detail) = native_runner::owned_cleanup_error(payload) {
            errors.push(reject(RejectionKind::SchemaError, detail));
        }
        errors.extend(native_binding_errors(
            payload, campaign, hash, lineage, installed,
        ));
    } else {
        errors.extend(runner_errors_with(payload, runner, lineage));
        errors.extend(binding_errors(payload, campaign, hash));
    }
    errors.extend(survivor_errors(payload, campaign));
    errors.extend(generated_outcome_errors(payload));
    errors.extend(cargo_value_errors(payload, campaign));
    errors
}
fn cargo_value_errors(payload: &Map<String, Value>, campaign: &CampaignContract) -> Vec<Rejection> {
    let mut errors = Vec::new();
    if let Some(metadata) = &campaign.generated_value {
        let cleanup = payload.get("cargo_value_process_cleanup");
        let commands = metadata["declaration"]["baseline_repetitions"]
            .as_u64()
            .and_then(|count| count.checked_add(1));
        if cleanup.and_then(|value| value.get("complete")) != Some(&Value::Bool(true))
            || cleanup
                .and_then(|value| value.get("commands_observed"))
                .and_then(Value::as_u64)
                != commands
            || payload
                .get("snapshot")
                .and_then(|value| value.get("removed"))
                != Some(&Value::Bool(true))
        {
            errors.push(reject(
                RejectionKind::SchemaError,
                "Cargo value process or snapshot cleanup is not complete",
            ));
        }
        let result = crate::mutation_evidence::cargo_value_receipt::evaluate(
            metadata,
            &campaign.source_sha256,
            payload.get("mutants").unwrap_or(&Value::Null),
            payload
                .get("cargo_value_observations")
                .unwrap_or(&Value::Null),
        );
        match result {
            Ok((value, attribution))
                if payload.get("test_value") == Some(&value)
                    && payload.get("attribution") == Some(&attribution) => {}
            Ok(_) => errors.push(reject(
                RejectionKind::SchemaError,
                "Cargo value or attribution differs from recomputed observations",
            )),
            Err(error) => errors.push(reject(
                RejectionKind::SchemaError,
                format!("Cargo value observations invalid: {error}"),
            )),
        }
        if let Some(contract) = &campaign.value_analysis {
            errors.extend(value_receipt_errors(payload.get("test_value"), contract));
        }
    }
    errors
}
fn identity_errors(payload: &Map<String, Value>, campaign: &CampaignContract) -> Vec<Rejection> {
    let mut errors = Vec::new();
    if payload.get("campaign_id").and_then(Value::as_str) != Some(&campaign.campaign_id) {
        errors.push(reject(RejectionKind::SchemaError, "campaign_id mismatch"));
    }
    if payload.get("manifest").and_then(Value::as_str) != Some(&campaign.manifest) {
        errors.push(reject(RejectionKind::SchemaError, "manifest path mismatch"));
    }
    if payload.get("manifest_sha256").and_then(Value::as_str) != Some(&campaign.manifest_sha256) {
        errors.push(reject(
            // Same reasoning as source drift below: the manifest PATH matches,
            // so this is a receipt for an earlier revision of the very campaign
            // it names -- the run passed, the campaign has since been refreshed,
            // and the campaign re-runs. A mismatched path or campaign_id stays
            // schema_error: that receipt does not belong to this campaign at all.
            RejectionKind::NotPass,
            "manifest hash mismatch",
        ));
    }
    if payload.get("source_sha256") != Some(&campaign.source_sha256) {
        errors.push(reject(
            RejectionKind::SchemaError,
            "source hash map mismatch",
        ));
    }
    if payload.get("test_sha256") != Some(&campaign.test_sha256) {
        errors.push(reject(RejectionKind::SchemaError, "test hash map mismatch"));
    }
    if campaign.source_drifted {
        errors.push(reject(
            // A stale PASS is not a current PASS -- the missing-evidence reason
            // for a drifted campaign is literally "no current complete PASS
            // receipt", so the debt class is not_pass: the campaign re-runs.
            RejectionKind::NotPass,
            "current source or test hashes drifted",
        ));
    }
    errors
}
/// A native receipt pins the compiled runner, not the Python files that did
/// not execute; the engine still has to be one this rule knows how to judge.
fn native_binding_errors(
    payload: &Map<String, Value>,
    campaign: &CampaignContract,
    hash: impl Fn(&str) -> Option<String>,
    lineage: impl Fn(Option<&Value>) -> bool,
    installed: Option<&native_runner::InstalledIdentity>,
) -> Vec<Rejection> {
    let mut errors: Vec<Rejection> =
        native_runner::errors_with_installed(payload, hash, lineage, installed)
            .into_iter()
            .map(|detail| reject(RejectionKind::RunnerMapMismatch, detail))
            .collect();
    if !matches!(
        campaign.mutation_engine.as_str(),
        "cargo-mutants" | "fest" | "mull"
    ) {
        errors.push(unsupported_engine(campaign));
    }
    errors
}
fn unsupported_engine(campaign: &CampaignContract) -> Rejection {
    reject(
        RejectionKind::SchemaError,
        format!(
            "unsupported generated mutation engine: {}",
            campaign.mutation_engine
        ),
    )
}
fn binding_errors(
    payload: &Map<String, Value>,
    campaign: &CampaignContract,
    hash: impl Fn(&str) -> Option<String>,
) -> Vec<Rejection> {
    let mut errors = Vec::new();
    let bindings = [
        ("core_sha256", "conductor/mutation_engine_generated.py"),
        ("scope_guard_sha256", "conductor/mutation_run_scope.py"),
    ];
    for (key, relative) in bindings {
        let recorded = payload.get(key).and_then(Value::as_str);
        let current = hash(relative);
        if recorded.is_none_or(|value| value.trim().is_empty()) {
            errors.push(reject(
                RejectionKind::RunnerMapMismatch,
                format!("{key} is missing"),
            ));
        } else if current.as_deref() != recorded {
            errors.push(reject(
                RejectionKind::RunnerMapMismatch,
                format!("{key} hash mismatch"),
            ));
        }
    }
    let adapter_path = match campaign.mutation_engine.as_str() {
        "cargo-mutants" => Some("conductor/mutation_engine_cargo.py"),
        "fest" => Some("conductor/mutation_engine_fest.py"),
        "mull" => Some("conductor/mutation_engine_mull.py"),
        _ => None,
    };
    let recorded_adapter = payload.get("adapter_sha256").and_then(Value::as_str);
    match adapter_path {
        Some(relative) => {
            let current_adapter = hash(relative);
            if recorded_adapter.is_none_or(|value| value.trim().is_empty()) {
                errors.push(reject(
                    RejectionKind::RunnerMapMismatch,
                    "adapter_sha256 is missing",
                ));
            } else if current_adapter.as_deref() != recorded_adapter {
                errors.push(reject(
                    RejectionKind::RunnerMapMismatch,
                    "adapter_sha256 hash mismatch",
                ));
            }
        }
        None => errors.push(unsupported_engine(campaign)),
    }
    errors
}
fn survivor_errors(payload: &Map<String, Value>, campaign: &CampaignContract) -> Vec<Rejection> {
    let mut errors = Vec::new();
    match payload.get("survivors").and_then(Value::as_array) {
        None => errors.push(reject(
            RejectionKind::SchemaError,
            "survivors must be a list",
        )),
        Some(rows) => {
            if rows.iter().any(|row| !row.is_string()) {
                errors.push(reject(
                    RejectionKind::SchemaError,
                    "survivors must contain only mutation IDs",
                ));
            }
            if !rows.is_empty() {
                errors.push(reject(
                    RejectionKind::SchemaError,
                    "PASS receipt reports surviving mutants",
                ));
            }
            let baseline: BTreeSet<&str> = campaign
                .survivor_baseline
                .iter()
                .map(String::as_str)
                .collect();
            let mut escaped: Vec<&str> = rows
                .iter()
                .filter_map(Value::as_str)
                .filter(|id| !baseline.contains(id))
                .collect();
            escaped.sort_unstable();
            escaped.dedup();
            if !escaped.is_empty() {
                // A survivor outside the baseline contradicts the PASS the
                // receipt claims -- the validator refuses the claim. It is not
                // `not_pass` debt: status says PASS, and CI must not wave a
                // genuinely regressed ratchet through as re-runnable debt.
                errors.push(reject(
                    RejectionKind::SchemaError,
                    format!(
                        "{} survivor(s) outside the recorded baseline: {}",
                        escaped.len(),
                        escaped.join(", ")
                    ),
                ));
            }
        }
    }
    errors
}

/// A measured unknown, unviable/no-coverage row or timeout cannot prove a kill.
/// Count equality and a score alone cannot substitute for complete observations.
pub(crate) fn generated_outcome_errors(payload: &Map<String, Value>) -> Vec<Rejection> {
    let mut errors = Vec::new();
    if !payload
        .get("baseline")
        .and_then(Value::as_object)
        .is_some_and(|baseline| {
            baseline.get("returncode").and_then(Value::as_i64) == Some(0)
                && baseline.get("timed_out") == Some(&Value::Bool(false))
        })
    {
        errors.push(reject(
            RejectionKind::SchemaError,
            "baseline must pass without timeout",
        ));
    }
    let Some(rows) = payload.get("mutants").and_then(Value::as_array) else {
        errors.push(reject(
            RejectionKind::SchemaError,
            "mutants must be a non-empty list",
        ));
        return errors;
    };
    let mut ids = BTreeSet::new();
    let mut counts = std::collections::BTreeMap::new();
    for row in rows {
        let id = row.get("id").and_then(Value::as_str);
        if id.is_none_or(str::is_empty) || !ids.insert(id.unwrap_or_default()) {
            errors.push(reject(
                RejectionKind::SchemaError,
                "mutants must have unique non-empty IDs",
            ));
        }
        let outcome = row.get("outcome").and_then(Value::as_str);
        if outcome != Some("KILLED") {
            errors.push(reject(
                RejectionKind::SchemaError,
                "PASS receipt has a non-killed mutant outcome",
            ));
        }
        if let Some(outcome) = outcome {
            *counts.entry(outcome).or_insert(0usize) += 1;
        }
    }
    if rows.is_empty() || !counts.contains_key("KILLED") {
        errors.push(reject(
            RejectionKind::SchemaError,
            "PASS receipt requires at least one killed mutant",
        ));
    }
    let declared = payload.get("outcome_counts").and_then(Value::as_object);
    if declared.is_none_or(|counts| counts.len() != 6) {
        errors.push(reject(
            RejectionKind::SchemaError,
            "outcome_counts has an invalid schema",
        ));
    }
    for outcome in [
        "KILLED",
        "NO_COVERAGE",
        "UNVIABLE",
        "ERROR",
        "SURVIVED",
        "TIMED_OUT",
    ] {
        if declared
            .and_then(|map| map.get(outcome))
            .and_then(Value::as_u64)
            != Some(*counts.get(outcome).unwrap_or(&0) as u64)
        {
            errors.push(reject(
                RejectionKind::SchemaError,
                "outcome_counts do not match mutant rows",
            ));
            break;
        }
    }
    errors.extend(completion_errors(payload, rows.len()));
    if payload.get("mutation_score").and_then(Value::as_f64) != Some(1.0) {
        errors.push(reject(
            RejectionKind::SchemaError,
            "mutation score is not 1.0",
        ));
    }
    errors
}

fn completion_errors(payload: &Map<String, Value>, rows: usize) -> Vec<Rejection> {
    let mut errors = Vec::new();
    if payload.get("engine_result").is_some_and(|engine| {
        engine.get("returncode").and_then(Value::as_i64) != Some(0)
            || engine.get("timed_out") != Some(&Value::Bool(false))
    }) {
        errors.push(reject(
            RejectionKind::SchemaError,
            "mutation engine must complete without failure or timeout",
        ));
    }
    let summary = payload.get("engine_summary").and_then(Value::as_object);
    let complete = payload.get("complete_campaign") == Some(&Value::Bool(true))
        || summary.is_some_and(|summary| {
            summary.get("mutants_generated").and_then(Value::as_u64) == Some(rows as u64)
                && summary.get("errors").and_then(Value::as_u64) == Some(0)
        });
    if payload
        .get("complete_campaign")
        .is_some_and(|value| !value.is_boolean())
        || !complete
        || payload.get("complete_campaign") == Some(&Value::Bool(false))
        || summary.is_some_and(|summary| {
            summary.get("mutants_generated").and_then(Value::as_u64) != Some(rows as u64)
                || summary.get("errors").and_then(Value::as_u64) != Some(0)
        })
    {
        errors.push(reject(
            RejectionKind::SchemaError,
            "partial or unproven complete campaign receipt",
        ));
    }
    errors
}
