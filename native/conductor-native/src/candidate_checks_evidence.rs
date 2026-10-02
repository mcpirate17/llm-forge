use super::{array, string, Finding};
use crate::candidate_policy::glob_match;
use regex::Regex;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

static EVIDENCE_PATH: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)(?:bench|perf|throughput|complexity|memory)").unwrap());
static NATIVE_TEXT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)(?:numpy|torch\.|numba|triton|native|vectori[sz])").unwrap());
static RESULT_TRIGGER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)(?:promot|baseline|metric|score|receipt)").unwrap());
static NUMERICAL_TRIGGER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:dtype|device|numerical|stability|finite|nan|inf)").unwrap()
});
static NUMERICAL_TEST: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)(?:dtype|device|isfinite|nan|raises|error)").unwrap());

pub fn performance_evidence(payload: &Value) -> Result<Value, String> {
    let hot = array(payload, "hot_changes")?;
    if hot.is_empty() {
        return Ok(json!({"findings": []}));
    }
    let mut findings = Vec::new();
    let (covered, errors) = numerical_coverage(payload, hot)?;
    if hot.iter().any(|row| {
        row["path"]
            .as_str()
            .is_none_or(|path| !covered.contains(path))
    }) {
        let hot_paths: Vec<&str> = hot
            .iter()
            .map(|row| string(row, "path"))
            .collect::<Result<_, _>>()?;
        findings.push(Finding::new("performance-evidence", "missing-performance-budget", "high",
            "hot-path change lacks validated numerical performance evidence bound to its current source bytes")
            .evidence(json!({"hot_paths": hot_paths,"receipt_errors":errors})));
    }
    for row in hot {
        let classes = array(row, "classes")?;
        let python = classes.iter().any(|class| class == "python");
        let native = classes.iter().any(|class| class == "native");
        if !python || native {
            continue;
        }
        if let Some(text) = row["text"].as_str() {
            if text.contains("performance-critical") && !NATIVE_TEXT.is_match(text) {
                findings.push(Finding::new("performance-evidence", "python-only-hotpath", "high",
                    "declared performance-critical path has no vectorized, compiled, or native execution route")
                    .path(string(row, "path")?));
            }
        }
    }
    Ok(json!({"findings": findings}))
}

fn numerical_coverage(
    payload: &Value,
    hot: &[Value],
) -> Result<(BTreeSet<String>, Vec<String>), String> {
    let mut covered = BTreeSet::new();
    let mut errors = Vec::new();
    let receipts = payload["performance_receipts"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    for row in receipts {
        if let Some(error) = row["error"].as_str() {
            errors.push(error.into());
            continue;
        }
        let result =
            serde_json::from_value::<crate::performance_receipt::Evidence>(row["receipt"].clone())
                .map_err(|error| error.to_string())
                .and_then(|evidence| {
                    let receipt = &evidence.current;
                    let sources: BTreeMap<String, String> = hot
                        .iter()
                        .filter_map(|change| {
                            let path = change["path"].as_str()?;
                            let hash = change["sha256"].as_str()?;
                            receipt
                                .identity
                                .source_files
                                .contains_key(path)
                                .then(|| (path.into(), hash.into()))
                        })
                        .collect();
                    if sources.is_empty() {
                        return Err("receipt covers no changed hot-path source".into());
                    }
                    let budget = payload["max_regression_percent"]
                        .as_f64()
                        .unwrap_or(crate::performance_receipt::MAX_GATE_REGRESSION_PERCENT);
                    evidence.validate(&sources, payload["expected_host"].as_str(), budget)?;
                    Ok(sources.into_keys().collect::<Vec<_>>())
                });
        match result {
            Ok(paths) => covered.extend(paths),
            Err(error) => errors.push(error),
        }
    }
    Ok((covered, errors))
}

pub fn performance_selection(payload: &Value) -> Result<Value, String> {
    let changes = array(payload, "changes")?;
    let globs = array(payload, "hot_globs")?;
    let mut hot_paths = Vec::new();
    let mut evidence_paths = Vec::new();
    for row in changes {
        let path = string(row, "path")?;
        let classes = array(row, "classes")?;
        if globs
            .iter()
            .filter_map(Value::as_str)
            .any(|pattern| glob_match(path, pattern))
            && !classes.iter().any(|class| class == "test")
        {
            hot_paths.push(path);
        }
        if EVIDENCE_PATH.is_match(path) {
            evidence_paths.push(path);
        }
    }
    Ok(json!({"hot_paths": hot_paths, "evidence_paths": evidence_paths}))
}

pub fn research_integrity(payload: &Value) -> Result<Value, String> {
    let changed = string(payload, "changed_text")?;
    let combined = string(payload, "combined_casefold")?;
    let tests = string(payload, "test_text")?;
    let changed_paths = array(payload, "changed_paths")?;
    let mut findings = Vec::new();
    if RESULT_TRIGGER.is_match(changed) {
        let missing: Vec<&str> = ["baseline", "seed", "config", "fingerprint"]
            .into_iter()
            .filter(|token| !combined.contains(token))
            .collect();
        if !missing.is_empty() {
            findings.push(
                Finding::new(
                    "research-integrity",
                    "incomplete-result-provenance",
                    "high",
                    format!(
                        "research decision path lacks exact identity/provenance fields: {}",
                        missing.join(", ")
                    ),
                )
                .evidence(json!({"changed_paths": changed_paths})),
            );
        }
    }
    if NUMERICAL_TRIGGER.is_match(changed) && !NUMERICAL_TEST.is_match(tests) {
        findings.push(Finding::new("research-integrity", "missing-numerical-device-tests", "high",
            "numerical/device-sensitive research change lacks selected dtype/device/finiteness tests"));
    }
    Ok(json!({"findings": findings}))
}
