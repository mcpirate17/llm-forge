//! Native orchestration for Conductor mutation evidence.

use std::collections::{BTreeSet, HashMap};
use std::path::Path;

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::mutation_manifest::{
    lexical_absolute, load_campaigns, load_registry_manifest_paths, python_str_or_empty,
    read_json_object, safe_relative, string_list, CampaignContract,
};
use crate::mutation_receipt::{
    campaign_scope_error, load_receipts, receipt_errors, AnchorConfig, Receipt, RunnerState,
    ValidationContext,
};

#[derive(Debug, Deserialize)]
struct VerificationRequest {
    repo_root: String,
    registry_path: String,
    canonical_test_patterns: Vec<String>,
    receipt_directories: Vec<String>,
    candidate_paths: Vec<String>,
    symbol_hashes: HashMap<String, HashMap<String, String>>,
    campaigns_override: Option<Vec<CampaignContract>>,
    runner: RunnerState,
    anchor: AnchorConfig,
}

#[derive(Debug, Deserialize)]
struct PlanRequest {
    repo_root: String,
    registry_path: String,
}

fn value_error(message: impl Into<String>) -> PyErr {
    PyValueError::new_err(message.into())
}

fn parse_json<T: for<'de> Deserialize<'de>>(text: &str, label: &str) -> PyResult<T> {
    serde_json::from_str(text).map_err(|error| value_error(format!("invalid {label}: {error}")))
}

#[pyfunction]
pub fn plan_mutation_evidence_native(request_json: &str) -> PyResult<String> {
    let request: PlanRequest = parse_json(request_json, "plan request")?;
    let root = lexical_absolute(Path::new(&request.repo_root)).map_err(value_error)?;
    let registry = root.join(&request.registry_path);
    let (registry_payload, manifests) =
        load_registry_manifest_paths(&root, &registry, None).map_err(value_error)?;
    let receipt_directories = string_list(
        registry_payload.get("receipt_directories"),
        "registry.receipt_directories",
    )
    .map_err(value_error)?
    .into_iter()
    .map(|path| safe_relative(&path, "receipt directory"))
    .collect::<Result<Vec<_>, _>>()
    .map_err(value_error)?;
    // A manifest this pass cannot read contributes no symbols, which is exactly right: the
    // campaign it describes cannot supply evidence either. Aborting here instead would let one
    // lane's half-written manifest refuse the gate for every other lane in the repository.
    let mut symbols = BTreeSet::new();
    for manifest in manifests {
        let path = root.join(manifest);
        let Ok((_, payload)) = read_json_object(&path, "campaign") else {
            continue;
        };
        if let Some(rows) = payload.get("source_symbols").and_then(Value::as_object) {
            for symbol_path in rows.keys() {
                let Ok(symbol_path) = safe_relative(symbol_path, "source_symbols key") else {
                    continue;
                };
                symbols.insert(symbol_path);
            }
        }
    }
    serde_json::to_string(&json!({
        "symbol_paths": symbols.into_iter().collect::<Vec<_>>(),
        "receipt_directories": receipt_directories,
    }))
    .map_err(|error| value_error(error.to_string()))
}

#[pyfunction]
pub fn verify_mutation_evidence_native(py: Python<'_>, request_json: &str) -> PyResult<String> {
    let request: VerificationRequest = parse_json(request_json, "verification request")?;
    let root = lexical_absolute(Path::new(&request.repo_root)).map_err(value_error)?;
    let anchor = lexical_absolute(Path::new(&request.anchor.repo)).map_err(value_error)?;
    let registry = root.join(&request.registry_path);
    let candidate_paths: BTreeSet<String> = request.candidate_paths.iter().cloned().collect();
    let (campaigns, broken) = match request.campaigns_override {
        Some(campaigns) => (campaigns, Vec::new()),
        None => load_campaigns(
            &root,
            &registry,
            &request.canonical_test_patterns,
            &request.symbol_hashes,
            &candidate_paths,
        )
        .map_err(value_error)?,
    };
    let (receipts, malformed) =
        load_receipts(py, &root, &request.receipt_directories).map_err(value_error)?;
    let context = ValidationContext {
        repo_root: &root,
        anchor_repo: &anchor,
        runner: &request.runner,
        anchor: &request.anchor,
    };

    let mut receipt_index: HashMap<&str, Vec<&Receipt>> = HashMap::new();
    for receipt in &receipts {
        if let Some(campaign_id) = receipt.value.get("campaign_id").and_then(Value::as_str) {
            receipt_index.entry(campaign_id).or_default().push(receipt);
        }
    }

    let normalized = candidate_paths;
    let mut evidence = Vec::new();
    let mut missing = Vec::new();
    for test_path in &normalized {
        let matching: Vec<&CampaignContract> = campaigns
            .iter()
            .filter(|campaign| {
                campaign
                    .source_sha256
                    .as_object()
                    .is_some_and(|sources| sources.contains_key(test_path))
                    && campaign
                        .ranked_test_paths
                        .iter()
                        .any(|path| path == test_path)
            })
            .collect();
        let mut candidates: Vec<(String, String, &CampaignContract, &Receipt)> = Vec::new();
        let mut rejection_reasons = Vec::new();
        for campaign in &matching {
            if let Some(error) = campaign_scope_error(campaign, test_path) {
                rejection_reasons.push(format!("{}: {error}", campaign.campaign_id));
                continue;
            }
            for receipt in receipt_index
                .get(campaign.campaign_id.as_str())
                .into_iter()
                .flatten()
            {
                let errors = receipt_errors(receipt, campaign, &context);
                if errors.is_empty() {
                    candidates.push((
                        python_str_or_empty(receipt.value.get("generated_at")),
                        receipt.name.clone(),
                        campaign,
                        receipt,
                    ));
                } else {
                    rejection_reasons.push(format!("{}: {}", receipt.name, errors.join(", ")));
                }
            }
        }
        candidates.sort_by(|left, right| {
            (left.0.as_str(), left.1.as_str()).cmp(&(right.0.as_str(), right.1.as_str()))
        });
        if let Some((_, _, campaign, receipt)) = candidates.last() {
            evidence.push(json!({
                "path": test_path,
                "campaign_id": campaign.campaign_id,
                "receipt": receipt.relative,
                "scope": campaign.test_scopes.get(test_path).cloned().unwrap_or(Value::Null),
            }));
        } else {
            // Only surface unloadable manifests when nothing ranked this path at all: one of
            // them may be the campaign that was meant to cover it. When a campaign did match,
            // the receipt errors are the real reason and broken siblings are noise.
            if matching.is_empty() {
                for entry in &broken {
                    rejection_reasons.push(format!(
                        "{} (registered campaign failed to load): {}",
                        entry.manifest, entry.error
                    ));
                }
            }
            missing.push(json!({
                "path": test_path,
                "reason": if matching.is_empty() {
                    "no registered campaign ranks this test file"
                } else {
                    "no current complete PASS receipt"
                },
                "receipt_rejections": rejection_reasons,
            }));
        }
    }

    let result = json!({
        "schema_version": "llm.mutation-testing.evidence-check.v1",
        "status": if missing.is_empty() { "PASS" } else { "FAIL" },
        "enforcement": "changed_tests",
        "checked_test_paths": normalized.into_iter().collect::<Vec<_>>(),
        "evidence": evidence,
        "missing_evidence": missing,
        "malformed_receipts": malformed,
        "broken_campaigns": broken,
    });
    serde_json::to_string(&result).map_err(|error| value_error(error.to_string()))
}

pub fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    crate::mutation_manifest::register(module)?;
    crate::mutation_receipt::register(module)?;
    module.add_function(wrap_pyfunction!(plan_mutation_evidence_native, module)?)?;
    module.add_function(wrap_pyfunction!(verify_mutation_evidence_native, module)?)?;
    Ok(())
}
