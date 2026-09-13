//! Native orchestration for Conductor mutation evidence.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::mutation_manifest::{
    lexical_absolute, load_campaigns, load_registry_manifest_paths, python_str_or_empty,
    read_json_object, safe_relative, string_list, BrokenCampaign, CampaignContract,
};
use crate::mutation_receipt::{
    campaign_scope_error, load_receipts, receipt_errors, AnchorConfig, Receipt, RejectionKind,
    RunnerState, ValidationContext,
};

#[derive(Debug, Deserialize)]
struct VerificationRequest {
    repo_root: String,
    package_root: String,
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

/// One test path's verdict: the best receipt that validates as evidence, or a
/// missing row naming every rejection with the kind produced where it arose.
/// Counts are bumped as rejections are observed -- not only on paths that end
/// up missing -- so a decode failure on a sibling receipt registers even when
/// another receipt covers the path.
fn evaluate_test_path<'a>(
    test_path: &str,
    campaigns: &'a [CampaignContract],
    receipt_index: &HashMap<&str, Vec<&'a Receipt>>,
    context: &ValidationContext<'_>,
    broken: &[BrokenCampaign],
    counts: &mut BTreeMap<String, usize>,
) -> (Option<Value>, Option<Value>) {
    let matching: Vec<&CampaignContract> = campaigns
        .iter()
        .filter(|campaign| campaign.covers_test(test_path))
        .collect();
    let mut candidates: Vec<(String, String, &CampaignContract, &Receipt)> = Vec::new();
    let mut rejections: Vec<Value> = Vec::new();
    for campaign in &matching {
        if let Some(error) = campaign_scope_error(campaign, test_path) {
            *counts
                .entry(RejectionKind::ScopeError.as_str().to_owned())
                .or_insert(0) += 1;
            rejections.push(json!({
                "receipt": campaign.campaign_id,
                "kind": RejectionKind::ScopeError.as_str(),
                "detail": error,
            }));
            continue;
        }
        for receipt in receipt_index
            .get(campaign.campaign_id.as_str())
            .into_iter()
            .flatten()
        {
            let errors = receipt_errors(receipt, campaign, context);
            if errors.is_empty() {
                candidates.push((
                    python_str_or_empty(receipt.value.get("generated_at")),
                    receipt.name.clone(),
                    campaign,
                    receipt,
                ));
            } else {
                for error in errors {
                    *counts.entry(error.kind.as_str().to_owned()).or_insert(0) += 1;
                    rejections.push(json!({
                        "receipt": receipt.name,
                        "kind": error.kind.as_str(),
                        "detail": error.detail,
                    }));
                }
            }
        }
    }
    candidates.sort_by(|left, right| {
        (left.0.as_str(), left.1.as_str()).cmp(&(right.0.as_str(), right.1.as_str()))
    });
    if let Some((_, _, campaign, receipt)) = candidates.last() {
        return (
            Some(json!({
                "path": test_path,
                "campaign_id": campaign.campaign_id,
                "receipt": receipt.relative,
                "scope": campaign.test_scopes.get(test_path).cloned().unwrap_or(Value::Null),
            })),
            None,
        );
    }
    // Only surface unloadable manifests when nothing ranked this path at all: one of
    // them may be the campaign that was meant to cover it. When a campaign did match,
    // the receipt errors are the real reason and broken siblings are noise.
    if matching.is_empty() {
        for entry in broken {
            *counts
                .entry(RejectionKind::ManifestLoadError.as_str().to_owned())
                .or_insert(0) += 1;
            rejections.push(json!({
                "receipt": entry.manifest,
                "kind": RejectionKind::ManifestLoadError.as_str(),
                "detail": format!(
                    "{} (registered campaign failed to load): {}",
                    entry.manifest, entry.error
                ),
            }));
        }
    }
    let reason_kind = if matching.is_empty() {
        RejectionKind::NoCampaign
    } else {
        RejectionKind::NotPass
    };
    *counts.entry(reason_kind.as_str().to_owned()).or_insert(0) += 1;
    (
        None,
        Some(json!({
            "path": test_path,
            "reason": if matching.is_empty() {
                "no registered campaign ranks this test file"
            } else {
                "no current complete PASS receipt"
            },
            "reason_kind": reason_kind.as_str(),
            "campaigns": matching
                .iter()
                .map(|campaign| campaign.campaign_id.clone())
                .collect::<Vec<_>>(),
            "receipt_rejections": rejections,
        })),
    )
}

#[pyfunction]
pub fn verify_mutation_evidence_native(py: Python<'_>, request_json: &str) -> PyResult<String> {
    let request: VerificationRequest = parse_json(request_json, "verification request")?;
    let root = lexical_absolute(Path::new(&request.repo_root)).map_err(value_error)?;
    let package_root = lexical_absolute(Path::new(&request.package_root)).map_err(value_error)?;
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
        package_root: &package_root,
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
    let mut rejection_counts: BTreeMap<String, usize> = BTreeMap::new();
    for test_path in &normalized {
        match evaluate_test_path(
            test_path,
            &campaigns,
            &receipt_index,
            &context,
            &broken,
            &mut rejection_counts,
        ) {
            (Some(row), None) => evidence.push(row),
            (None, Some(row)) => missing.push(row),
            _ => unreachable!("every test path yields evidence or a missing row"),
        }
    }
    // A receipt the loader could not parse at all belongs to the same defect
    // class as one whose detail does not decode. Zero stays absent: a kind is
    // listed only when it happened, so presence stays signal for the caller.
    if !malformed.is_empty() {
        *rejection_counts
            .entry(RejectionKind::DecodeError.as_str().to_owned())
            .or_insert(0) += malformed.len();
    }

    let result = json!({
        "schema_version": "llm.mutation-testing.evidence-check.v2",
        "status": if missing.is_empty() { "PASS" } else { "FAIL" },
        "enforcement": "changed_tests",
        "checked_test_paths": normalized.into_iter().collect::<Vec<_>>(),
        "evidence": evidence,
        "missing_evidence": missing,
        "rejection_counts": rejection_counts,
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
