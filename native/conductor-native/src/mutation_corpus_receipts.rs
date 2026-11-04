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
    mut source_hash: impl FnMut(&str) -> Result<Option<String>>,
    mut lineage_accepts: impl FnMut(&Value) -> Result<bool>,
) -> Result<Option<String>> {
    ensure!(
        runner_components.is_object(),
        "current runner components must be an object"
    );
    let object = receipt.as_object().context("receipt must be an object")?;
    let status = object.get("status").unwrap_or(&Value::Null);
    if !matches!(status.as_str(), Some("PASS" | "RATCHET_HELD")) {
        return Ok(Some(format!("status={}", python_status(status))));
    }
    if native_runner::declared(object) {
        let recorded = match native_runner::components(object) {
            Ok(recorded) => recorded,
            Err(reason) => return Ok(Some(reason)),
        };
        let mut current = true;
        for (path, expected) in recorded {
            current &= source_hash(path)?.as_deref() == expected.as_str();
        }
        if !current && !lineage_accepts(&Value::Object(recorded.clone()))? {
            return Ok(Some(
                "native runner components match neither this tree nor any lineage entry".into(),
            ));
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
