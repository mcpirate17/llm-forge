//! Fail-closed mutation-evidence row decisions and value-admission planning.

use super::finding;
use serde_json::{json, Map, Value};
use std::collections::{BTreeSet, HashMap, HashSet};
use unicode_general_category::{get_general_category, GeneralCategory};

const MISSING_HELP: &str = "A changed test requires a current registered PASS receipt before it can land. The only permitted route is an automatic engine -- `make mutation-generate MUTATION_GENERATE_ARGS='--only SRC'` then `make mutation-engine-run MUTATION_CAMPAIGN=...`. Hand-authored mutants, manifests, patches and receipts are forbidden, and the tooling that produced them has been removed.";
const VALUE_HELP: &str = "Mutation waivers exempt only legacy receipt debt; new test definitions still need a value-classified PASS receipt.";
const ADMISSION_HELP: &str = "Bind the new test to a critical/high active-source contract, record batch-level per-test attribution, and retain it as CORE or explicitly justified INTENTIONAL_REDUNDANCY.";

fn py_string_repr(text: &str) -> String {
    let quote = if text.contains('\'') && !text.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::with_capacity(text.len() + 2);
    out.push(quote);
    for ch in text.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if ch == quote => {
                out.push('\\');
                out.push(ch);
            }
            ch if ch != ' '
                && matches!(
                    get_general_category(ch),
                    GeneralCategory::Control
                        | GeneralCategory::Format
                        | GeneralCategory::PrivateUse
                        | GeneralCategory::Surrogate
                        | GeneralCategory::Unassigned
                        | GeneralCategory::SpaceSeparator
                        | GeneralCategory::LineSeparator
                        | GeneralCategory::ParagraphSeparator
                ) =>
            {
                let point = ch as u32;
                if point <= 0xff {
                    out.push_str(&format!("\\x{point:02x}"));
                } else if point <= 0xffff {
                    out.push_str(&format!("\\u{point:04x}"));
                } else {
                    out.push_str(&format!("\\U{point:08x}"));
                }
            }
            ch => out.push(ch),
        }
    }
    out.push(quote);
    out
}

fn py_repr(value: &Value) -> String {
    match value {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::String(text) => py_string_repr(text),
        Value::Array(rows) => format!(
            "[{}]",
            rows.iter().map(py_repr).collect::<Vec<_>>().join(", ")
        ),
        Value::Object(rows) => format!(
            "{{{}}}",
            rows.iter()
                .map(|(key, value)| { format!("{}: {}", py_string_repr(key), py_repr(value)) })
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Value::Number(_) => value.to_string(),
    }
}

fn py_str(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        _ => py_repr(value),
    }
}

fn malformed_container(field: &str) -> Value {
    finding("mutation-evidence", "malformed-evidence-container", "critical",
        format!("{field}: evidence container is malformed (not a list); its rows cannot be evaluated and admission cannot be proven"),
        None, None, None)
}

pub(super) fn receipt_findings(request: &Value) -> Result<Value, String> {
    let payload = &request["payload"];
    let waived: HashSet<_> = request["waived"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let mut findings = Vec::new();
    if let Some(rows) = payload.get("missing_evidence").and_then(Value::as_array) {
        for missing in rows {
            let Some(row) = missing.as_object() else {
                findings.push(finding(
                    "mutation-evidence",
                    "malformed-mutation-receipt",
                    "critical",
                    format!(
                        "malformed mutation receipt row (not an object): {}",
                        py_repr(missing)
                    ),
                    None,
                    None,
                    None,
                ));
                continue;
            };
            let path = row.get("path").map(py_str).unwrap_or_default();
            if waived.contains(path.as_str()) {
                continue;
            }
            let reason = row
                .get("reason")
                .map(py_str)
                .unwrap_or_else(|| "missing mutation evidence".into());
            findings.push(finding("mutation-evidence", "missing-mutation-receipt", "critical",
                format!("{path}: {reason} -- current automatic PASS evidence is required"),
                (!path.is_empty()).then_some(path.as_str()), Some(MISSING_HELP),
                Some(json!({"receipt_rejections": row.get("receipt_rejections").cloned().unwrap_or_else(|| json!([]))}))));
        }
    } else if payload.get("missing_evidence").is_some() {
        findings.push(malformed_container("missing_evidence"));
    }
    if let Some(rows) = payload.get("malformed_receipts").and_then(Value::as_array) {
        for row in rows {
            findings.push(finding(
                "mutation-evidence",
                "malformed-mutation-receipt",
                "critical",
                format!("malformed mutation receipt: {}", py_str(row)),
                None,
                None,
                None,
            ));
        }
    } else if payload.get("malformed_receipts").is_some() {
        findings.push(malformed_container("malformed_receipts"));
    }
    Ok(json!(findings))
}

pub(super) fn index(request: &Value) -> Result<Value, String> {
    let rows = request["evidence_rows"]
        .as_array()
        .ok_or("evidence rows must be a list")?;
    let mut index = Map::new();
    let mut positions = HashMap::new();
    let mut findings = Vec::new();
    for (offset, row) in rows.iter().enumerate() {
        let position = offset + 1;
        let Some(path) = row
            .get("path")
            .and_then(Value::as_str)
            .filter(|_| row.is_object())
        else {
            findings.push(finding("mutation-evidence", "malformed-evidence-row", "critical",
                format!("evidence row {position} is malformed (needs an object with a string 'path'); the row cannot be evaluated: {}", py_repr(row)),
                None, None, None));
            continue;
        };
        if let Some(previous) = positions.get(path) {
            findings.push(finding("mutation-evidence", "duplicate-evidence-row", "critical",
                format!("{path}: evidence rows {previous} and {position} claim the same path; evidence identities must be unique and every row for this path is rejected"),
                None, None, None));
            continue;
        }
        index.insert(path.into(), row.clone());
        positions.insert(path.to_owned(), position);
    }
    Ok(json!({"index": index, "findings": findings}))
}

fn unavailable(path: &str, nodeids: &[String], detail: &str) -> Value {
    finding(
        "mutation-evidence",
        "test-value-receipt-unavailable",
        "critical",
        format!(
            "{path}: {detail}; value admission cannot be evaluated for {}",
            nodeids.join(", ")
        ),
        Some(path),
        None,
        None,
    )
}

fn nodeid_map(request: &Value) -> Result<&Map<String, Value>, String> {
    request["new_nodeids"]
        .as_object()
        .ok_or("new nodeids must be an object".into())
}

pub(super) fn admission_plan(request: &Value) -> Result<Value, String> {
    let gated = nodeid_map(request)?;
    let Some(rows) = request["payload"].get("evidence").and_then(Value::as_array) else {
        if request["payload"].get("evidence").is_none() {
            return admission_plan(&json!({"payload": {"evidence": []}, "new_nodeids": gated}));
        }
        let steps: Vec<_> = gated.iter().map(|(path, value)| {
            json!({"finding": unavailable(path, &strings(value), "payload evidence envelope is malformed (not a list)")})
        }).collect();
        return Ok(json!({"prefix_findings": [], "steps": steps}));
    };
    let indexed = index(&json!({"evidence_rows": rows}))?;
    let prefix = indexed["findings"].as_array().cloned().unwrap_or_default();
    let evidence = indexed["index"].as_object().expect("index result");
    let mut steps = Vec::new();
    for (path, value) in gated {
        let nodeids = strings(value);
        let Some(row) = evidence.get(path) else {
            steps.push(json!({"finding": finding("mutation-evidence", "new-test-value-not-admitted", "critical",
                format!("{path}: new test definition(s) lack admitted value evidence: {}", nodeids.join(", ")),
                Some(path), Some(VALUE_HELP), None)}));
            continue;
        };
        let Some(receipt) = row.get("receipt").and_then(Value::as_str) else {
            steps.push(json!({"finding": unavailable(path, &nodeids, "evidence row lacks a string receipt name")}));
            continue;
        };
        steps.push(json!({"task": {"path": path, "nodeids": nodeids, "receipt": receipt, "evidence": row}}));
    }
    Ok(json!({"prefix_findings": prefix, "steps": steps}))
}

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}

pub(super) fn admission_findings(request: &Value) -> Result<Value, String> {
    let path = request["path"]
        .as_str()
        .ok_or("admission path must be a string")?;
    let nodeids = strings(&request["nodeids"]);
    let errors = strings(&request["errors"]);
    let mut findings = Vec::new();
    for error in errors {
        let named = nodeids
            .iter()
            .find(|nodeid| error.contains(&py_repr(&json!(nodeid))));
        let evidence = named.map(|nodeid| json!({"nodeid": nodeid}));
        findings.push(finding(
            "mutation-evidence",
            "new-test-value-not-admitted",
            "critical",
            format!("{path}: {error}"),
            Some(path),
            Some(ADMISSION_HELP),
            evidence,
        ));
    }
    Ok(json!(findings))
}

pub(super) fn required_paths(request: &Value) -> Result<Value, String> {
    let paths = strings(&request["test_paths"]);
    if request["gated_nodeids"].is_null() {
        return Ok(json!(paths));
    }
    let gated = request["gated_nodeids"]
        .as_object()
        .ok_or("gated nodeids must be an object")?;
    Ok(json!(paths
        .into_iter()
        .filter(|path| gated
            .get(path)
            .is_some_and(|rows| { rows.as_array().is_some_and(|rows| !rows.is_empty()) }))
        .collect::<Vec<_>>()))
}

pub(super) fn metrics(request: &Value) -> Result<Value, String> {
    let payload = &request["payload"];
    let gated = nodeid_map(request)?;
    let states = request["waiver_states"]
        .as_array()
        .ok_or("waiver states must be a list")?;
    let applied: BTreeSet<_> = states
        .iter()
        .filter(|state| state["active"] == true)
        .filter_map(|state| state["path"].as_str())
        .collect();
    let mut sorted_states = states.clone();
    sorted_states.sort_by_key(|state| state["path"].as_str().unwrap_or("").to_owned());
    let nodeids: Vec<_> = gated.values().flat_map(strings).collect();
    let count = |field: &str| {
        payload
            .get(field)
            .and_then(Value::as_array)
            .map_or(0, Vec::len)
    };
    Ok(json!({
        "checked_test_paths": payload.get("checked_test_paths").cloned().unwrap_or_else(|| json!([])),
        "covered_tests": count("evidence"), "missing_tests": count("missing_evidence"),
        "value_gated_nodeids": nodeids, "mutation_waiver_applied": applied,
        "mutation_waiver_states": sorted_states
    }))
}
