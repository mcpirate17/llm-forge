//! Native orchestration for Conductor mutation evidence.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

pub use crate::mutation_manifest::profile::{execution_profile, validate_fest_path};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::mutation_manifest::{
    lexical_absolute, load_campaigns, load_registry_manifest_paths, python_str_or_empty,
    read_json_object, safe_relative, string_list, BrokenCampaign, CampaignContract,
};
use crate::mutation_receipt::{
    campaign_scope_error, load_receipts, receipt_errors, AnchorConfig, Receipt, Rejection,
    RejectionKind, RunnerState, ValidationContext,
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

/// Ambient compatibility planner. This does not hold/authenticate coordinator inputs.
pub fn plan(request: &Value) -> Result<Value, String> {
    admit(request)?;
    let request: PlanRequest =
        serde_json::from_value(request.clone()).map_err(|error| error.to_string())?;
    let root = lexical_absolute(Path::new(&request.repo_root))?;
    let registry = root.join(safe_relative(&request.registry_path, "mutation registry")?);
    let (registry_payload, manifests) = load_registry_manifest_paths(&root, &registry, None)?;
    let receipt_directories = string_list(
        registry_payload.get("receipt_directories"),
        "registry.receipt_directories",
    )?
    .into_iter()
    .map(|path| safe_relative(&path, "receipt directory"))
    .collect::<Result<Vec<_>, _>>()?;
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
    Ok(json!({
        "symbol_paths": symbols.into_iter().collect::<Vec<_>>(),
        "receipt_directories": receipt_directories,
    }))
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
    evaluate_test_path_with(
        test_path,
        campaigns,
        receipt_index,
        broken,
        counts,
        &mut |receipt, campaign| receipt_errors(receipt, campaign, context),
    )
}
fn evaluate_test_path_with<'a>(
    test_path: &str,
    campaigns: &'a [CampaignContract],
    receipt_index: &HashMap<&str, Vec<&'a Receipt>>,
    broken: &[BrokenCampaign],
    counts: &mut BTreeMap<String, usize>,
    validate: &mut impl FnMut(&Receipt, &CampaignContract) -> Vec<Rejection>,
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
            let errors = validate(receipt, campaign);
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
    if matching.is_empty() {
        report_broken(broken, counts, &mut rejections);
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

/// Ambient compatibility verification: not a held/captured-input authority.
/// Coordinator adoption should use validate_generated_captured after its own capture.
pub fn verify(request: &Value) -> Result<Value, String> {
    admit(request)?;
    let request: VerificationRequest =
        serde_json::from_value(request.clone()).map_err(|error| error.to_string())?;
    let root = lexical_absolute(Path::new(&request.repo_root))?;
    let package_root = lexical_absolute(Path::new(&request.package_root))?;
    let anchor = lexical_absolute(Path::new(&request.anchor.repo))?;
    let registry = root.join(safe_relative(&request.registry_path, "mutation registry")?);
    let candidate_paths: BTreeSet<String> = request.candidate_paths.iter().cloned().collect();
    let (campaigns, broken) = match request.campaigns_override {
        Some(campaigns) => (campaigns, Vec::new()),
        None => load_campaigns(
            &root,
            &registry,
            &request.canonical_test_patterns,
            &request.symbol_hashes,
            &candidate_paths,
        )?,
    };
    let (receipts, malformed) = load_receipts(&root, &request.receipt_directories)?;
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
    admit(&result)?;
    Ok(result)
}

#[cfg(feature = "python")]
#[path = "mutation_evidence_bindings.rs"]
mod bindings;
#[cfg(feature = "python")]
pub(crate) use bindings::register;
#[path = "mutation_verifier_inputs.rs"]
pub(crate) mod inputs;
use inputs::admit;
pub use inputs::{parse_request, validate_generated_captured};
/// Single receipt compatibility validation; ambient runner/anchor I/O is not held.
pub fn validate_receipt(request: &Value) -> Result<Value, String> {
    admit(request)?;
    crate::mutation_receipt::validate(request)
}
/// Ambient compatibility lineage observation, not authenticated runtime identity.
pub fn runner_lineage_accepts(request: &Value) -> Result<bool, String> {
    admit(request)?;
    crate::mutation_receipt::accepts_lineage(request)
}
#[cfg(test)]
#[path = "mutation_verifier_tests.rs"]
mod tests;

fn report_broken(
    broken: &[BrokenCampaign],
    counts: &mut BTreeMap<String, usize>,
    rejections: &mut Vec<Value>,
) {
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

/// Ambient manifest validator. Use only within a separately admitted held tree;
/// this function itself does not retain capture bindings.
pub fn load_campaign(request: &Value) -> Result<Value, String> {
    admit(request)?;
    crate::mutation_manifest::load_campaign_value(request)
}
/// Ambient registry validator, distinct from authenticated coordinator capture.
pub fn load_registry(request: &Value) -> Result<Value, String> {
    admit(request)?;
    crate::mutation_manifest::load_registry_value(request)
}

/// Internal bounded Value admission reused by held coordinator adapters.
pub(crate) fn admit_captured(value: &Value) -> Result<(), String> {
    inputs::admit(value)
}

/// No ambient reads: raw manifest identity and actual captured source/test hashes.
pub fn load_generated_campaign_captured(
    path: &str,
    text: &str,
    current: &BTreeMap<String, String>,
) -> Result<Value, String> {
    if text.len() > 2 * 1024 * 1024 || current.len() > 4096 {
        return Err("captured generated manifest bounds exceeded".into());
    }
    for (path, hash) in current {
        safe_relative(path, "captured source hash path")?;
        if path.len() > 4096
            || hash.len() != 64
            || !hash
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err("captured source hash malformed".into());
        }
    }
    let parsed = parse_request(text)?;
    let payload = parsed
        .as_object()
        .ok_or("captured manifest must be an object")?;
    if payload.get("schema_version").and_then(Value::as_u64) != Some(1) {
        return Err("captured manifest schema is not1".into());
    }
    let engine = payload
        .get("mutation_engine")
        .and_then(Value::as_str)
        .ok_or("captured engine missing")?;
    if !crate::mutation_manifest::GENERATED_ENGINES.contains(&engine) {
        return Err("captured manifest must use an automatic engine".into());
    }
    let path = safe_relative(path, "captured campaign manifest")?;
    let contract = crate::mutation_manifest::generated_campaign_contract_with(
        payload,
        text.as_bytes().to_vec(),
        &path,
        engine,
        |relative| current.get(relative).cloned(),
    )?;
    serde_json::to_value(contract).map_err(|error| error.to_string())
}

/// Registry semantics over already held raw Git blobs, including all registry.d fragments.
pub fn load_registry_captured(
    text: &str,
    fragments: &BTreeMap<String, String>,
) -> Result<Value, String> {
    if text.len() > 2 * 1024 * 1024 || fragments.len() > 4096 {
        return Err("captured registry bounds exceeded".into());
    }
    let mut total = text.len();
    let mut manifests = Vec::new();
    for (name, text) in fragments {
        total = total
            .checked_add(text.len())
            .ok_or("registry byte overflow")?;
        if name.len() > 4096 || text.len() > 2 * 1024 * 1024 || total > 32 * 1024 * 1024 {
            return Err("captured registry exceeds read budget".into());
        }
        let value = parse_request(text)?;
        let manifest = value
            .get("manifest")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or("captured registry fragment requires manifest")?;
        manifests.push(safe_relative(manifest, "registry fragment manifest")?);
    }
    let value = parse_request(text)?;
    let payload = value
        .as_object()
        .ok_or("captured registry must be an object")?
        .clone();
    let patterns = canonical_test_patterns();
    let (payload, manifests) =
        crate::mutation_manifest::registry_contract(payload, Some(&patterns), || Ok(manifests))?;
    Ok(json!({"payload":payload,"manifests":manifests}))
}
pub fn canonical_test_patterns() -> Vec<String> {
    CANONICAL_TEST_PATTERNS
        .iter()
        .map(|value| (*value).into())
        .collect()
}
const CANONICAL_TEST_PATTERNS: &[&str] = &[
    "**/test_*.py",
    "**/*_test.py",
    "**/*.test.js",
    "**/*.test.jsx",
    "**/*.test.ts",
    "**/*.test.tsx",
    "**/*.spec.js",
    "**/*.spec.jsx",
    "**/*.spec.ts",
    "**/*.spec.tsx",
    "**/*Test.java",
    "**/test_*.c",
    "**/*_test.c",
    "**/test_*.cc",
    "**/*_test.cc",
    "**/test_*.cpp",
    "**/*_test.cpp",
    "**/test_*.cxx",
    "**/*_test.cxx",
];

#[path = "mutation_cargo_value.rs"]
pub mod cargo_value;
#[path = "mutation_cargo_value_receipt.rs"]
pub mod cargo_value_receipt;
/// Full existing contract rules over held actual file bytes; no ambient source reads.
pub fn load_campaign_captured(
    path: &str,
    files: &BTreeMap<String, Vec<u8>>,
    selected: &[String],
) -> Result<Value, String> {
    crate::mutation_manifest::load_contract_captured(path, files, selected)
}

pub fn load_campaign_captured_with_cargo(
    path: &str,
    files: &BTreeMap<String, Vec<u8>>,
    selected: &[String],
    inventory: &crate::mutation_cargo_inventory::Inventory,
) -> Result<Value, String> {
    crate::mutation_manifest::load_contract_captured_with_cargo(path, files, selected, inventory)
}

#[path = "mutation_evidence_capture.rs"]
mod captured;
pub use captured::{
    verify_captured, verify_captured_native, verify_captured_native_with_supporting,
    verify_captured_native_with_supporting_profiles, CapturedAttempt, CapturedReceipt,
    CapturedVerification, WorkspaceBindings,
};

pub fn normalize_path(path: &str) -> Result<String, String> {
    if path.len() > 4096 {
        return Err("mutation path exceeds4096 bytes".into());
    }
    safe_relative(path, "captured mutation path")
}

/// Compiled historical anchor spelling, independent of current project configuration.
pub fn legacy_anchor_registry_path() -> String {
    crate::mutation_receipt::default_anchor_registry_path()
}

#[cfg(test)]
#[path = "mutation_capture_tests.rs"]
mod capture_tests;

/// Check a held, caller-verified lineage payload against recorded runner components.
pub fn captured_lineage_accepts(recorded: &Value, lineage: &Value) -> bool {
    crate::mutation_receipt::lineage_payload_accepts(Some(recorded), lineage)
}
