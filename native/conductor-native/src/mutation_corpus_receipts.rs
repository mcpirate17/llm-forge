//! Corpus reproducibility observations over caller-held campaign, runner and receipt facts.
//! This helper does not acquire/authenticate inputs, execute mutants, or issue PASS evidence.
use crate::mutation_native_runner as native_runner;
use anyhow::{ensure, Context, Result};
use serde_json::{json, Value};
use std::collections::BTreeSet;

const MAX_ROWS: usize = 100_000;
const MAX_PATH: usize = 4096;
#[derive(Debug)]
pub struct Campaign<'a> {
    pub campaign_id: &'a str,
    pub generated: bool,
    pub ordinary_cargo: bool,
    /// Independently loaded campaign fields, never derived from a receipt.
    pub ordinary_contract: Option<&'a Value>,
    pub declares_value_analysis: bool,
    pub source_symbols: &'a BTreeSet<String>,
}
fn python_status(value: &Value) -> String {
    match value {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::String(value) => value.clone(),
        other => other.to_string(),
    }
}
/// Whether one receipt is usable for corpus reproducibility; this is not mutation PASS.
/// The callbacks must independently bind current tree hashes and pinned lineage.
pub fn rejection(
    campaign: &Campaign<'_>,
    receipt: &Value,
    runner_components: &Value,
    source_hash: impl FnMut(&str) -> Result<Option<String>>,
    lineage_accepts: impl FnMut(&Value) -> Result<bool>,
) -> Result<Option<String>> {
    rejection_with_installed(
        campaign,
        receipt,
        runner_components,
        source_hash,
        lineage_accepts,
        None,
    )
}

/// Installed identity must come from an independently held operator pin, after
/// the caller proves the entire native source inventory is absent.
pub fn rejection_with_installed(
    campaign: &Campaign<'_>,
    receipt: &Value,
    runner_components: &Value,
    source_hash: impl FnMut(&str) -> Result<Option<String>>,
    lineage_accepts: impl FnMut(&Value) -> Result<bool>,
    installed: Option<&native_runner::InstalledIdentity>,
) -> Result<Option<String>> {
    rejection_with_supporting(
        campaign,
        receipt,
        runner_components,
        source_hash,
        lineage_accepts,
        installed,
        None,
    )
}

fn expand_ordinary(receipt: &Value, ordinary: bool) -> Result<Option<Value>, String> {
    let raw = receipt.as_object().ok_or("receipt must be an object")?;
    if !ordinary {
        return Ok(None);
    }
    let expanded = crate::receipt_slim::expand_receipt(receipt)
        .map_err(|error| format!("ordinary Cargo receipt detail invalid: {error}"))?;
    let object = expanded
        .as_object()
        .ok_or("expanded ordinary Cargo receipt is not an object")?;
    if raw.get("runner_kind") != object.get("runner_kind") {
        return Err("ordinary Cargo receipt detail changes runner identity".into());
    }
    Ok(native_runner::declared(raw).then_some(expanded))
}

fn ordinary_binding_error(
    object: &serde_json::Map<String, Value>,
    campaign: &Campaign<'_>,
) -> Option<String> {
    let Some(contract) = campaign.ordinary_contract.and_then(Value::as_object) else {
        return Some("independent ordinary Cargo campaign binding unavailable".into());
    };
    for field in [
        "campaign_id",
        "manifest",
        "manifest_sha256",
        "source_sha256",
        "test_sha256",
    ] {
        if contract.get(field).is_none() || object.get(field) != contract.get(field) {
            return Some(format!(
                "ordinary Cargo receipt {field} differs from campaign"
            ));
        }
    }
    None
}

fn ordinary_supporting_error(
    object: &serde_json::Map<String, Value>,
    campaign: &Campaign<'_>,
    supporting: Option<&crate::mutation_cargo_supporting_binding::SupportingInputs>,
) -> Option<String> {
    if let Some(reason) = ordinary_binding_error(object, campaign) {
        return Some(reason);
    }
    let profile = match campaign
        .ordinary_contract
        .and_then(|value| value.get("execution_profile"))
    {
        None | Some(Value::Null) => None,
        Some(Value::String(value)) => Some(value.as_str()),
        Some(_) => return Some("invalid verified execution profile".into()),
    };
    crate::mutation_cargo_supporting_binding::compare_profile(
        object.get("supporting_inputs"),
        supporting,
        profile,
    )
    .err()
}

pub fn rejection_with_supporting(
    campaign: &Campaign<'_>,
    receipt: &Value,
    runner_components: &Value,
    mut source_hash: impl FnMut(&str) -> Result<Option<String>>,
    mut lineage_accepts: impl FnMut(&Value) -> Result<bool>,
    installed: Option<&native_runner::InstalledIdentity>,
    supporting: Option<&crate::mutation_cargo_supporting_binding::SupportingInputs>,
) -> Result<Option<String>> {
    ensure!(
        runner_components.is_object(),
        "current runner components must be an object"
    );
    let raw = receipt.as_object().context("receipt must be an object")?;
    let expanded = match expand_ordinary(receipt, campaign.ordinary_cargo) {
        Ok(value) => value,
        Err(error) => return Ok(Some(error)),
    };
    let object = expanded.as_ref().and_then(Value::as_object).unwrap_or(raw);
    let status = object.get("status").unwrap_or(&Value::Null);
    if !matches!(status.as_str(), Some("PASS" | "RATCHET_HELD")) {
        return Ok(Some(format!("status={}", python_status(status))));
    }
    if native_runner::declared(object) {
        if campaign.ordinary_cargo {
            if let Some(reason) = ordinary_supporting_error(object, campaign, supporting) {
                return Ok(Some(reason));
            }
        }
        if let Some(reason) = native_runner::owned_cleanup_error(object) {
            return Ok(Some(reason));
        }
        let current = source_hash(native_runner::FINGERPRINT_KEY)?;
        let errors =
            native_runner::errors_with_installed(object, |_| current.clone(), |_| false, installed);
        if let Some(reason) = errors.into_iter().next() {
            return Ok(Some(reason));
        }
    } else {
        let Some(recorded) = object
            .get("runner_components_sha256")
            .and_then(Value::as_object)
        else {
            return Ok(Some("no runner component map".into()));
        };
        let recorded_value = Value::Object(recorded.clone());
        if &recorded_value != runner_components && !lineage_accepts(&recorded_value)? {
            return Ok(Some(
                "runner components match neither this runner nor any lineage entry".into(),
            ));
        }
    }
    let hashes = match object.get("source_sha256") {
        None | Some(Value::Null) => None,
        Some(Value::Object(rows)) => Some(rows),
        _ => anyhow::bail!("receipt source_sha256 must be an object"),
    };
    let mut stale = Vec::new();
    if let Some(hashes) = hashes {
        ensure!(
            hashes.len() <= MAX_ROWS,
            "receipt source pin count exceeds limit"
        );
        for (path, expected) in hashes {
            ensure!(
                path.len() <= MAX_PATH,
                "receipt source pin path exceeds limit"
            );
            if campaign.source_symbols.contains(path) {
                continue;
            }
            let expected = expected
                .as_str()
                .context("receipt source pin must be a string")?;
            if source_hash(path)?.as_deref() != Some(expected) {
                stale.push(path.clone());
            }
        }
    }
    stale.sort();
    if stale.is_empty() {
        Ok(None)
    } else {
        Ok(Some(format!(
            "source hashes differ from this tree: {stale:?}"
        )))
    }
}
/// Select the first newest acceptable receipt by `generated_at`, preserving Python's tie rule.
pub fn newest_acceptable(
    receipts: &[Value],
    mut reject: impl FnMut(&Value) -> Result<Option<String>>,
) -> Result<Option<&Value>> {
    ensure!(receipts.len() <= MAX_ROWS, "receipt count exceeds limit");
    let mut best: Option<(&str, &Value)> = None;
    for receipt in receipts {
        if reject(receipt)?.is_some() {
            continue;
        }
        let date = match receipt.get("generated_at") {
            None | Some(Value::Null) => "",
            Some(Value::String(date)) => date.as_str(),
            _ => anyhow::bail!("receipt generated_at must be a string or null"),
        };
        ensure!(date.len() <= MAX_PATH, "receipt timestamp exceeds limit");
        if best.as_ref().is_none_or(|(previous, _)| date > *previous) {
            best = Some((date, receipt));
        }
    }
    Ok(best.map(|(_, receipt)| receipt))
}
/// Return `unmeasured` and `inert_tests` rows over the chosen acceptable receipt.
pub fn value_findings(
    campaign: &Campaign<'_>,
    receipt: Option<&Value>,
) -> Result<(Vec<Value>, Vec<Value>)> {
    ensure!(
        !campaign.campaign_id.is_empty() && campaign.campaign_id.len() <= MAX_PATH,
        "invalid campaign id"
    );
    let value = match receipt {
        None => Value::Null,
        Some(payload) if payload.get("test_value").is_some() => payload["test_value"].clone(),
        Some(payload) if payload.get("detail").is_some() => {
            crate::receipt_slim::expand_receipt(payload)
                .map_err(anyhow::Error::msg)?
                .get("test_value")
                .cloned()
                .unwrap_or(Value::Null)
        }
        Some(_) => Value::Null,
    };
    let row = |reason, detail| json!({"campaign_id":campaign.campaign_id,"reason":reason,"detail":detail});
    if value.is_null() {
        return Ok((
            if !campaign.generated && !campaign.declares_value_analysis {
                vec![row(
                    "NO_VALUE_ANALYSIS",
                    "nothing measures which of its tests detect anything",
                )]
            } else {
                vec![]
            },
            vec![],
        ));
    }
    let Some(tests) = value.get("tests").and_then(Value::as_array) else {
        return Ok((
            vec![row(
                "INVALID_VALUE_ANALYSIS",
                "receipt test_value must contain a tests list",
            )],
            vec![],
        ));
    };
    ensure!(
        tests.len() <= MAX_ROWS,
        "receipt value test count exceeds limit"
    );
    let mut inert = Vec::new();
    for test in tests {
        let (Some(nodeid), Some(classification)) = (
            test.get("nodeid").and_then(Value::as_str),
            test.get("classification").and_then(Value::as_str),
        ) else {
            return Ok((
                vec![row(
                    "INVALID_VALUE_ANALYSIS",
                    "receipt test_value rows require nodeid and classification",
                )],
                inert,
            ));
        };
        ensure!(
            nodeid.len() <= MAX_PATH && classification.len() <= MAX_PATH,
            "receipt value field exceeds limit"
        );
        if classification == "DELETE_CANDIDATE" {
            inert.push(json!({"campaign_id":campaign.campaign_id,"nodeid":nodeid,"reason":"KILLS_NOTHING"}));
        }
    }
    Ok((vec![], inert))
}
#[cfg(test)]
#[path = "mutation_corpus_receipts_tests.rs"]
mod tests;
