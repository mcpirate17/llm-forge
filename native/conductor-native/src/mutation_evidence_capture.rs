//! Pure full receipt selection over held caller facts; anchor proof is native Rust I/O injection.
use super::*;
use crate::mutation_cargo_supporting_binding::uses_snapshot_inputs;
use crate::mutation_receipt::{self as rules, Rejection};
use sha2::{Digest, Sha256};
#[derive(Clone)]
pub struct CapturedReceipt {
    pub path: String,
    pub text: String,
}
pub struct CapturedAttempt {
    pub campaign_id: String,
    pub manifest: String,
    pub receipt: String,
    pub sha256: String,
    pub expanded: Value,
    pub valid: bool,
    pub bindings_current: bool,
    pub errors: Value,
}
pub struct CapturedVerification {
    pub result: Value,
    pub attempts: Vec<CapturedAttempt>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Runtime {
    runner: RunnerState,
    binding_sha256: BTreeMap<String, String>,
    lineage: Option<Value>,
}
/// Independently captured workspace identities and configured receipt namespace.
pub struct WorkspaceBindings<'a> {
    pub identities: &'a BTreeMap<
        String,
        Result<crate::mutation_cargo_supporting_binding::SupportingInputs, String>,
    >,
    pub receipt_root: Option<&'a str>,
}
struct NativeBindings<'a> {
    fingerprint: Option<&'a str>,
    supporting:
        Result<Option<&'a crate::mutation_cargo_supporting_binding::SupportingInputs>, &'a str>,
    installed: Option<&'a crate::mutation_native_runner::InstalledIdentity>,
    workspace: Option<&'a WorkspaceBindings<'a>>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    candidate_paths: Vec<String>,
    campaigns: Vec<CampaignContract>,
    runtime: Option<Runtime>,
    broken_campaigns: Vec<BrokenCampaign>,
    malformed_receipts: Vec<String>,
}
type Cache = HashMap<(String, String), Vec<Rejection>>;
const LIMIT: usize = 32 * 1024 * 1024;
/// The callback is coordinator code over a held immutable Source/anchor, never
/// JSON claiming an anchor is valid. No ambient reader or interpreter is used.
pub fn verify_captured(
    request: &Value,
    raw: &[CapturedReceipt],
    anchor: impl FnMut(&str, &[u8], &Value) -> Result<(), String>,
) -> Result<CapturedVerification, String> {
    verify_captured_native(request, raw, anchor, None, None)
}

/// Native inputs are trusted coordinator facts held outside the request document.
pub fn verify_captured_native(
    request: &Value,
    raw: &[CapturedReceipt],
    anchor: impl FnMut(&str, &[u8], &Value) -> Result<(), String>,
    native_fingerprint: Option<&str>,
    installed: Option<&crate::mutation_native_runner::InstalledIdentity>,
) -> Result<CapturedVerification, String> {
    verify_captured_native_with_supporting(
        request,
        raw,
        anchor,
        native_fingerprint,
        installed,
        Ok(None),
    )
}

pub fn verify_captured_native_with_supporting(
    request: &Value,
    raw: &[CapturedReceipt],
    anchor: impl FnMut(&str, &[u8], &Value) -> Result<(), String>,
    native_fingerprint: Option<&str>,
    installed: Option<&crate::mutation_native_runner::InstalledIdentity>,
    supporting: Result<Option<&crate::mutation_cargo_supporting_binding::SupportingInputs>, &str>,
) -> Result<CapturedVerification, String> {
    verify_captured_native_with_supporting_profiles(
        request,
        raw,
        anchor,
        native_fingerprint,
        installed,
        supporting,
        None,
    )
}

/// Workspace identities come from independently captured inputs, keyed by verified campaign ID.
pub fn verify_captured_native_with_supporting_profiles(
    request: &Value,
    raw: &[CapturedReceipt],
    mut anchor: impl FnMut(&str, &[u8], &Value) -> Result<(), String>,
    native_fingerprint: Option<&str>,
    installed: Option<&crate::mutation_native_runner::InstalledIdentity>,
    supporting: Result<Option<&crate::mutation_cargo_supporting_binding::SupportingInputs>, &str>,
    workspace: Option<&WorkspaceBindings<'_>>,
) -> Result<CapturedVerification, String> {
    admit(request)?;
    let request: Request =
        serde_json::from_value(request.clone()).map_err(|error| error.to_string())?;
    if request.candidate_paths.len() > 4096 || request.campaigns.len() > 4096 || raw.len() > 4096 {
        return Err("captured mutation inventory exceeds4096".into());
    }
    if let Some(runtime) = &request.runtime {
        runtime_guard(runtime)?;
    }
    let paths = inventory(&request)?;
    let native = NativeBindings {
        fingerprint: native_fingerprint,
        supporting,
        installed,
        workspace,
    };
    let (receipts, mut malformed) = receipts(raw)?;
    malformed.extend(request.malformed_receipts.iter().cloned());
    let mut cache = Cache::new();
    let mut attempts = Vec::new();
    let mut work = 0usize;
    for campaign in &request.campaigns {
        if !paths.iter().any(|path| campaign.covers_test(path)) {
            continue;
        }
        let contract = serde_json::to_value(campaign).map_err(|error| error.to_string())?;
        for receipt in receipts
            .iter()
            .filter(|receipt| receipt.value["campaign_id"] == campaign.campaign_id)
        {
            if cache.len() >= 4096 {
                return Err("captured receipt validations exceed4096".into());
            }
            let decoded = rules::decode_captured(&receipt.value);
            let size = match &decoded {
                Ok(value) => inputs::measure(value)?,
                Err(_) => 0,
            };
            work = work
                .saturating_add(size)
                .saturating_add(receipt.bytes.len());
            if work > LIMIT {
                return Err("captured validation byte amplification exceeds32MiB".into());
            }
            let errors = match &decoded {
                Ok(value) => validate(
                    value,
                    campaign,
                    &contract,
                    receipt,
                    request.runtime.as_ref(),
                    &mut anchor,
                    &native,
                )?,
                Err(error) => vec![error.clone()],
            };
            if errors.len() > 4096 {
                return Err("captured receipt diagnostic limit exceeded".into());
            }
            let valid = errors.is_empty();
            attempts.push(CapturedAttempt {
                campaign_id: campaign.campaign_id.clone(),
                manifest: campaign.manifest.clone(),
                receipt: receipt.relative.clone(),
                sha256: format!("{:x}", Sha256::digest(&receipt.bytes)),
                expanded: decoded.unwrap_or_else(|_| receipt.value.clone()),
                valid,
                bindings_current: summary_current(
                    receipt,
                    campaign,
                    request.runtime.as_ref(),
                    &native,
                ),
                errors: error_rows(&errors),
            });
            cache.insert(
                (campaign.manifest.clone(), receipt.relative.clone()),
                errors,
            );
        }
    }
    let result = rank(
        &paths,
        &request.campaigns,
        &receipts,
        &cache,
        &request.broken_campaigns,
        malformed,
    )?;
    Ok(CapturedVerification { result, attempts })
}
fn receipts(raw: &[CapturedReceipt]) -> Result<(Vec<Receipt>, Vec<String>), String> {
    let mut receipts = Vec::new();
    let mut malformed = Vec::new();
    let mut total = 0usize;
    let mut names = BTreeSet::new();
    for row in raw {
        if safe_relative(&row.path, "captured receipt")? != row.path
            || !names.insert(row.path.clone())
        {
            return Err("captured receipt paths must be unique and canonical".into());
        }
        total = total.saturating_add(row.text.len());
        if row.path.len() > 4096 || row.text.len() > LIMIT || total > LIMIT {
            return Err("captured receipt byte budget exceeded".into());
        }
        let value = match parse_request(&row.text) {
            Ok(value) if value.is_object() => value,
            Ok(_) => {
                malformed.push(format!("{}: receipt must be an object", row.path));
                continue;
            }
            Err(error) => {
                malformed.push(format!("{}: {error}", row.path));
                continue;
            }
        };
        receipts.push(Receipt {
            path: Path::new("/").join(&row.path),
            relative: row.path.clone(),
            name: Path::new(&row.path)
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or("receipt filename unavailable")?
                .into(),
            value,
            bytes: row.text.as_bytes().to_vec(),
            parsed_bytes_available: true,
        });
    }
    Ok((receipts, malformed))
}
fn validate(
    value: &Value,
    campaign: &CampaignContract,
    contract: &Value,
    receipt: &Receipt,
    runtime: Option<&Runtime>,
    anchor: &mut impl FnMut(&str, &[u8], &Value) -> Result<(), String>,
    native: &NativeBindings<'_>,
) -> Result<Vec<Rejection>, String> {
    let payload = value
        .as_object()
        .ok_or("expanded captured receipt is not an object")?;
    if campaign.generated {
        let is_native = crate::mutation_native_runner::declared(payload);
        let workspace = uses_snapshot_inputs(campaign.execution_profile.as_deref());
        if runtime.is_none() && !is_native {
            return Ok(vec![missing_runner()]);
        }
        if is_native && workspace {
            let authorized = native.workspace.is_some_and(|bindings| {
                bindings.receipt_root.is_some_and(|root| {
                    crate::mutation_snapshot_inputs::within_namespace(&receipt.relative, root)
                })
            });
            if !authorized {
                return Ok(vec![Rejection {
                    kind: crate::mutation_receipt::RejectionKind::ScopeError,
                    detail: "workspace receipt root unavailable or receipt outside configured root"
                        .into(),
                }]);
            }
        }
        let empty = RunnerState {
            components: None,
            error: None,
            mutation_testing_sha256: None,
        };
        return Ok(rules::captured_generated_errors_with_installed(
            payload,
            campaign,
            runtime.map_or(&empty, |r| &r.runner),
            |path| {
                if path == crate::mutation_native_runner::FINGERPRINT_KEY {
                    native
                        .fingerprint
                        .map(str::to_string)
                        .or_else(|| runtime.and_then(|r| r.binding_sha256.get(path).cloned()))
                } else {
                    runtime.and_then(|r| r.binding_sha256.get(path).cloned())
                }
            },
            |recorded| {
                runtime
                    .and_then(|r| r.lineage.as_ref())
                    .is_some_and(|lineage| rules::lineage_payload_accepts(recorded, lineage))
            },
            native.installed,
            if workspace {
                match native
                    .workspace
                    .and_then(|items| items.identities.get(&campaign.campaign_id))
                {
                    Some(Ok(identity)) => Ok(Some(identity)),
                    Some(Err(error)) => Err(error.as_str()),
                    None => Err("independently captured workspace inputs unavailable"),
                }
            } else {
                native.supporting
            },
        ));
    }
    let anchored = if payload["schema_version"] == "llm.mutation-testing.receipt.v2" {
        anchor(&receipt.relative, &receipt.bytes, contract)
            .err()
            .map(|detail| Rejection {
                kind: RejectionKind::SchemaError,
                detail,
            })
            .into_iter()
            .collect()
    } else {
        Vec::new()
    };
    Ok(rules::reviewed_errors_with(
        payload,
        campaign,
        || anchored,
        || match runtime {
            None => vec![missing_runner()],
            Some(runtime) => rules::runner_errors_with(payload, &runtime.runner, |recorded| {
                runtime
                    .lineage
                    .as_ref()
                    .is_some_and(|lineage| rules::lineage_payload_accepts(recorded, lineage))
            }),
        },
    ))
}
fn missing_runner() -> Rejection {
    Rejection {
        kind: RejectionKind::RunnerMapMismatch,
        detail: "operator-pinned mutation runner bundle unavailable".into(),
    }
}
fn summary_current(
    receipt: &Receipt,
    campaign: &CampaignContract,
    runtime: Option<&Runtime>,
    native: &NativeBindings<'_>,
) -> bool {
    let runner_current = if let Some(payload) = receipt
        .value
        .as_object()
        .filter(|p| crate::mutation_native_runner::declared(p))
    {
        crate::mutation_native_runner::errors_with_installed(
            payload,
            |path| {
                if path == crate::mutation_native_runner::FINGERPRINT_KEY {
                    native
                        .fingerprint
                        .map(str::to_string)
                        .or_else(|| runtime.and_then(|r| r.binding_sha256.get(path).cloned()))
                } else {
                    None
                }
            },
            |_| false,
            native.installed,
        )
        .is_empty()
    } else {
        runtime.is_some_and(|runtime| {
            Some(&receipt.value["runner_components_sha256"]) == runtime.runner.components.as_ref()
                && receipt.value["runner_sha256"].as_str()
                    == runtime.runner.mutation_testing_sha256.as_deref()
        })
    };
    !campaign.source_drifted
        && receipt.value["manifest_sha256"] == campaign.manifest_sha256
        && receipt.value["source_sha256"] == campaign.source_sha256
        && receipt.value["test_sha256"] == campaign.test_sha256
        && runner_current
}
fn error_rows(errors: &[Rejection]) -> Value {
    json!(errors
        .iter()
        .map(|error| json!({"kind":error.kind.as_str(),"detail":error.detail}))
        .collect::<Vec<_>>())
}

fn rank(
    paths: &BTreeSet<String>,
    campaigns: &[CampaignContract],
    receipts: &[Receipt],
    cache: &Cache,
    broken: &[BrokenCampaign],
    malformed: Vec<String>,
) -> Result<Value, String> {
    let mut index: HashMap<&str, Vec<&Receipt>> = HashMap::new();
    for receipt in receipts {
        if let Some(id) = receipt.value["campaign_id"].as_str() {
            index.entry(id).or_default().push(receipt);
        }
    }
    reserve(paths, campaigns, &index, cache, broken)?;
    let mut evidence = Vec::new();
    let mut missing = Vec::new();
    let mut counts = BTreeMap::new();
    for path in paths {
        let pair = evaluate_test_path_with(
            path,
            campaigns,
            &index,
            broken,
            &mut counts,
            &mut |receipt, campaign| {
                cache
                    .get(&(campaign.manifest.clone(), receipt.relative.clone()))
                    .expect("admitted captured validation")
                    .clone()
            },
        );
        match pair {
            (Some(row), None) => evidence.push(row),
            (None, Some(row)) => missing.push(row),
            _ => return Err("captured verifier produced inconsistent verdict".into()),
        }
    }
    if !malformed.is_empty() {
        *counts.entry("decode_error".into()).or_insert(0) += malformed.len();
    }
    let result = json!({"schema_version":"llm.mutation-testing.evidence-check.v2","status":if missing.is_empty(){"PASS"}else{"FAIL"},
        "enforcement":"changed_tests","checked_test_paths":paths,"evidence":evidence,"missing_evidence":missing,
        "rejection_counts":counts,"malformed_receipts":malformed,"broken_campaigns":broken});
    admit(&result)?;
    Ok(result)
}
fn reserve(
    paths: &BTreeSet<String>,
    campaigns: &[CampaignContract],
    index: &HashMap<&str, Vec<&Receipt>>,
    cache: &Cache,
    broken: &[BrokenCampaign],
) -> Result<(), String> {
    let mut bytes = 0usize;
    for path in paths {
        let mut matched = false;
        for campaign in campaigns
            .iter()
            .filter(|campaign| campaign.covers_test(path))
        {
            matched = true;
            if let Some(error) = campaign_scope_error(campaign, path) {
                bytes = bytes.saturating_add(error.len() + 256);
                continue;
            }
            for receipt in index
                .get(campaign.campaign_id.as_str())
                .into_iter()
                .flatten()
            {
                let errors = cache
                    .get(&(campaign.manifest.clone(), receipt.relative.clone()))
                    .ok_or("captured receipt validation missing")?;
                for error in errors {
                    bytes = bytes.saturating_add(error.detail.len() + receipt.relative.len() + 256);
                }
            }
        }
        if !matched {
            for row in broken {
                bytes = bytes.saturating_add(row.error.len() + row.manifest.len() + 256);
            }
        }
        if bytes > LIMIT {
            return Err("mutation peer rejection amplification exceeds32MiB".into());
        }
    }
    Ok(())
}
fn runtime_guard(runtime: &Runtime) -> Result<(), String> {
    let hash = |value: &str| {
        value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    };
    if runtime.binding_sha256.values().any(|value| !hash(value)) {
        return Err("captured runner hashes malformed or unavailable".into());
    }
    if runtime.runner.components.is_none() {
        if runtime
            .runner
            .error
            .as_deref()
            .is_some_and(|error| !error.trim().is_empty())
            && runtime.runner.mutation_testing_sha256.is_none()
            && runtime.lineage.is_none()
        {
            return Ok(());
        }
        return Err("captured runner diagnostic is malformed".into());
    }
    let components = runtime
        .runner
        .components
        .as_ref()
        .and_then(Value::as_object)
        .filter(|map| !map.is_empty())
        .ok_or("captured runner component map unavailable")?;
    if components
        .values()
        .any(|value| value.as_str().is_none_or(|value| !hash(value)))
        || runtime
            .runner
            .mutation_testing_sha256
            .as_deref()
            .is_none_or(|value| !hash(value))
    {
        return Err("captured runner hashes malformed or unavailable".into());
    }
    Ok(())
}

fn inventory(request: &Request) -> Result<BTreeSet<String>, String> {
    let mut ids = BTreeSet::new();
    for campaign in &request.campaigns {
        if !ids.insert(&campaign.manifest) {
            return Err("duplicate captured campaign manifests".into());
        }
    }
    let paths: BTreeSet<_> = request.candidate_paths.iter().cloned().collect();
    if paths.len() != request.candidate_paths.len() {
        return Err("duplicate captured candidate paths".into());
    }
    let declared: usize = request
        .campaigns
        .iter()
        .map(|campaign| campaign.ranked_test_paths.len().max(1))
        .sum();
    if paths.len().saturating_mul(declared) > 10_000_000 {
        return Err("captured mutation selection work exceeded".into());
    }
    Ok(paths)
}

#[cfg(test)]
#[path = "mutation_evidence_diagnostic_tests.rs"]
mod diagnostic_tests;
