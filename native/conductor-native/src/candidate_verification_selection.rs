//! Pure nodeid admission, test-selection, and waiver activation decisions.

use super::finding;
use serde_json::{json, Map, Value};
use std::collections::{BTreeSet, HashSet};

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}

fn has_class(change: &Value, class: &str) -> bool {
    change["classes"]
        .as_array()
        .is_some_and(|classes| classes.iter().any(|item| item == class))
}

pub(super) fn gated_nodeids(request: &Value) -> Result<Value, String> {
    let entries = request["entries"]
        .as_array()
        .ok_or("gated entries must be a list")?;
    let grandfathered = request["grandfathered"]
        .as_object()
        .ok_or("grandfathered nodeids must be an object")?;
    let mut gated = Map::new();
    for change in entries {
        if !has_class(change, "test") {
            continue;
        }
        let path = change["path"].as_str().ok_or("gated test requires path")?;
        if path.ends_with(".patch") {
            continue;
        }
        if !path.ends_with(".py") {
            let added = change["old_mode"] == "000000"
                || change["old_oid"] == "0000000000000000000000000000000000000000";
            if added && !grandfathered.contains_key(path) {
                gated.insert(path.into(), json!([path]));
            }
            continue;
        }
        let definitions = change["definitions"]
            .as_object()
            .ok_or_else(|| format!("candidate definitions missing for {path}"))?;
        let base = change["base"].as_object();
        let excluded: HashSet<_> = grandfathered
            .get(path)
            .map(strings)
            .unwrap_or_default()
            .into_iter()
            .collect();
        let mut labels: Vec<_> = definitions
            .keys()
            .filter(|label| base.is_none_or(|base| base.get(*label) != definitions.get(*label)))
            .filter(|label| !excluded.contains(label.as_str()))
            .map(|label| format!("{path}::{label}"))
            .collect();
        labels.sort();
        if !labels.is_empty() {
            gated.insert(path.into(), json!(labels));
        }
    }
    Ok(Value::Object(gated))
}

pub(super) fn plan(request: &Value) -> Result<Value, String> {
    let changes = request["changes"]
        .as_array()
        .ok_or("candidate changes must be a list")?;
    let mut sources = Vec::new();
    let mut changed_tests = BTreeSet::new();
    let mut high_risk = false;
    for change in changes {
        if change["deleted"] == true {
            continue;
        }
        let path = change["path"]
            .as_str()
            .ok_or("candidate change requires path")?;
        let test = has_class(change, "test");
        if !test && (has_class(change, "python") || has_class(change, "native")) {
            sources.push(path.to_owned());
        }
        if test && path.ends_with(".py") {
            changed_tests.insert(path.to_owned());
        }
        high_risk |= !test && change["risk"] == "high";
    }
    Ok(json!({"sources": sources, "changed_tests": changed_tests, "high_risk": high_risk}))
}

pub(super) fn decide(request: &Value) -> Result<Value, String> {
    let sources = strings(&request["sources"]);
    let mut tests: BTreeSet<_> = strings(&request["graph_tests"]).into_iter().collect();
    tests.extend(strings(&request["convention_tests"]));
    tests.extend(strings(&request["changed_tests"]));
    let native = request["native_tests"]
        .as_object()
        .ok_or("native tests must be an object")?;
    let mut graph = request["graph"]
        .as_object()
        .cloned()
        .ok_or("selection graph must be an object")?;
    let native_files: BTreeSet<_> = native.values().flat_map(strings).collect();
    let native_count: usize = native
        .values()
        .map(|files| files.as_array().map_or(0, Vec::len))
        .sum();
    graph.insert("native_test_files".into(), json!(native_count));
    let mut findings = Vec::new();
    if let Some(error) = request["graph_error"].as_str() {
        findings.push(finding(
            "test-evidence",
            "graph-evidence-incomplete",
            "critical",
            format!("dependency/call-graph test selection failed closed: {error}"),
            None,
            None,
            None,
        ));
    }
    let uncovered: Vec<_> = sources
        .into_iter()
        .filter(|source| !native.contains_key(source))
        .collect();
    if !uncovered.is_empty() && tests.is_empty() {
        findings.push(finding(
            "test-evidence",
            "no-targeted-tests",
            "high",
            "changed production code has no graph-selected or convention-matched tests".into(),
            None,
            None,
            Some(json!({"source_paths": uncovered})),
        ));
    }
    let evidence_tests: BTreeSet<_> = tests.union(&native_files).cloned().collect();
    if request["high_risk"] == true
        && !evidence_tests.is_empty()
        && request["property_evidence"] != true
    {
        findings.push(finding(
            "test-evidence",
            "missing-property-or-mutation-evidence",
            "high",
            "high-risk logic lacks property/parameterized/mutation-style test evidence".into(),
            None,
            None,
            None,
        ));
    }
    Ok(json!({"tests": tests, "graph": graph, "findings": findings,
              "evidence_tests": evidence_tests}))
}

pub(super) fn waiver_states(request: &Value) -> Result<Value, String> {
    let waivers = request["waivers"]
        .as_array()
        .ok_or("mutation waivers must be a list")?;
    let base = request["base"].as_str();
    let mut states = Vec::with_capacity(waivers.len());
    for waiver in waivers {
        let id = waiver["id"].as_str().ok_or("mutation waiver requires id")?;
        let path = waiver["path"]
            .as_str()
            .ok_or("mutation waiver requires path")?;
        let mut active = base == waiver["integration_base"].as_str();
        let mut reason = String::new();
        if !active {
            reason = "candidate base commit is not the pinned integration base".into();
        } else if waiver["file_ok"] != true {
            active = false;
            reason = "test file missing or drifted from pinned sha256".into();
        } else if let Some(source) = waiver["sources"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|source| source["ok"] != true)
        {
            active = false;
            let source_path = source["path"].as_str().unwrap_or("");
            reason = format!("pinned source {source_path} missing or drifted from pinned sha256");
        }
        states.push(json!({"id": id, "path": path, "active": active, "reason": reason}));
    }
    Ok(json!(states))
}
