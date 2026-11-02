//! Pure seven-dimensional corpus baseline comparison. This does not audit or attest a corpus.
use anyhow::{ensure, Context, Result};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet};

pub const BASELINE_KEYS: [&str; 7] = [
    "stale_mutations",
    "unloadable_manifests",
    "host_pinned_interpreters",
    "uncovered_campaigns",
    "campaigns_without_value_analysis",
    "tests_that_kill_nothing",
    "uncovered_changed_files",
];
const MAX_IDS: usize = 1_000_000;
const MAX_ID_BYTES: usize = 4096;
const MAX_TOTAL_ID_BYTES: usize = 64 * 1024 * 1024;
fn baseline_sets(value: &Value) -> Result<BTreeMap<&'static str, BTreeSet<String>>> {
    let object = value
        .as_object()
        .context("reproducibility baseline must be an object")?;
    let mut total = 0usize;
    let mut bytes = 0usize;
    let mut result = BTreeMap::new();
    for key in BASELINE_KEYS {
        let mut set = BTreeSet::new();
        if let Some(rows) = object.get(key) {
            let rows = rows
                .as_array()
                .with_context(|| format!("{key} must be an array of ids"))?;
            total = total
                .checked_add(rows.len())
                .context("baseline count overflow")?;
            ensure!(total <= MAX_IDS, "baseline exceeds identifier budget");
            for row in rows {
                let id = row
                    .as_str()
                    .with_context(|| format!("{key} must contain string ids"))?;
                ensure!(id.len() <= MAX_ID_BYTES, "baseline id exceeds length limit");
                bytes = bytes
                    .checked_add(id.len())
                    .context("baseline byte count overflow")?;
                ensure!(
                    bytes <= MAX_TOTAL_ID_BYTES,
                    "baseline exceeds identifier byte budget"
                );
                set.insert(id.to_owned());
            }
        }
        result.insert(key, set);
    }
    Ok(result)
}
fn rows<'a>(payload: &'a Value, key: &str) -> Result<&'a Vec<Value>> {
    payload
        .get(key)
        .and_then(Value::as_array)
        .with_context(|| format!("corpus observation missing array {key}"))
}
fn field<'a>(row: &'a Value, key: &str) -> Result<&'a str> {
    let value = row
        .get(key)
        .and_then(Value::as_str)
        .with_context(|| format!("corpus observation missing string {key}"))?;
    ensure!(
        value.len() <= MAX_ID_BYTES,
        "corpus observation id exceeds limit"
    );
    Ok(value)
}
/// Convert the seven actual patch/reproducibility result arrays into comparable IDs.
/// This transformation does not obtain or authenticate those observations.
pub fn findings(
    patches: &Value,
    reproducibility: &Value,
) -> Result<BTreeMap<String, BTreeSet<String>>> {
    let dimensions = [
        (
            "stale_mutations",
            patches,
            "stale",
            "campaign_id",
            Some("mutation_id"),
        ),
        (
            "unloadable_manifests",
            patches,
            "unloadable",
            "manifest",
            None,
        ),
        (
            "host_pinned_interpreters",
            reproducibility,
            "interpreters",
            "campaign_id",
            None,
        ),
        (
            "uncovered_campaigns",
            reproducibility,
            "evidence",
            "campaign_id",
            None,
        ),
        (
            "campaigns_without_value_analysis",
            reproducibility,
            "unmeasured",
            "campaign_id",
            None,
        ),
        (
            "tests_that_kill_nothing",
            reproducibility,
            "inert_tests",
            "campaign_id",
            Some("nodeid"),
        ),
        (
            "uncovered_changed_files",
            reproducibility,
            "uncovered_files",
            "file",
            None,
        ),
    ];
    let mut result = BTreeMap::new();
    let mut count = 0usize;
    let mut bytes = 0usize;
    for (dimension, payload, key, first, second) in dimensions {
        let mut set = BTreeSet::new();
        for row in rows(payload, key)? {
            count = count
                .checked_add(1)
                .context("corpus observation count overflow")?;
            ensure!(count <= MAX_IDS, "corpus observation count exceeds limit");
            let left = field(row, first)?;
            let value = if let Some(second) = second {
                format!("{left}::{}", field(row, second)?)
            } else {
                left.to_owned()
            };
            ensure!(
                value.len() <= MAX_ID_BYTES,
                "composite corpus id exceeds limit"
            );
            bytes = bytes
                .checked_add(value.len())
                .context("corpus observation byte overflow")?;
            ensure!(
                bytes <= MAX_TOTAL_ID_BYTES,
                "corpus observation byte limit exceeded"
            );
            set.insert(value);
        }
        result.insert(dimension.to_owned(), set);
    }
    Ok(result)
}

/// Compare already-observed corpus findings with a recorded baseline.
/// `None` represents an absent baseline; no corpus is read, executed or authenticated here.
pub fn compare(
    found: &BTreeMap<String, BTreeSet<String>>,
    baseline: Option<&Value>,
) -> Result<Value> {
    ensure!(
        found.len() == BASELINE_KEYS.len()
            && BASELINE_KEYS.iter().all(|key| found.contains_key(*key)),
        "all seven observed corpus dimensions are required"
    );
    let empty = json!({});
    let recorded = baseline_sets(baseline.unwrap_or(&empty))?;
    let mut output = Map::new();
    let mut regressed = false;
    let mut stale = false;
    let mut patch_delta = false;
    let mut total = 0usize;
    let mut bytes = 0usize;
    for key in BASELINE_KEYS {
        let actual = &found[key];
        total = total
            .checked_add(actual.len())
            .context("finding count overflow")?;
        ensure!(
            total <= MAX_IDS && actual.iter().all(|id| id.len() <= MAX_ID_BYTES),
            "finding identifier budget exceeded"
        );
        for id in actual {
            bytes = bytes
                .checked_add(id.len())
                .context("finding byte count overflow")?;
            ensure!(
                bytes <= MAX_TOTAL_ID_BYTES,
                "findings exceed identifier byte budget"
            );
        }
        let baseline = &recorded[key];
        let new: Vec<_> = actual.difference(baseline).cloned().collect();
        let resolved: Vec<_> = baseline.difference(actual).cloned().collect();
        regressed |= !new.is_empty();
        stale |= !resolved.is_empty();
        if matches!(key, "stale_mutations" | "unloadable_manifests") {
            patch_delta |= !new.is_empty() || !resolved.is_empty();
        }
        output.insert(format!("new_{key}"), json!(new));
        output.insert(format!("resolved_{key}"), json!(resolved));
    }
    let status = if regressed {
        "REGRESSED"
    } else if stale {
        "BASELINE_STALE"
    } else {
        "CLEAN"
    };
    let exit_code = if patch_delta {
        6
    } else if status == "CLEAN" {
        0
    } else {
        7
    };
    output.insert("status".into(), json!(status));
    output.insert("exit_code".into(), json!(exit_code));
    Ok(Value::Object(output))
}
#[cfg(test)]
#[path = "mutation_corpus_delta_tests.rs"]
mod tests;
