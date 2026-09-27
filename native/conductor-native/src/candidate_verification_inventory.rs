//! Validate the anchored grandfather inventory after Git and digest proofs.

use serde_json::{json, Map, Value};
use std::collections::{BTreeSet, HashSet};

fn repr(value: &str) -> String {
    format!("'{}'", value.replace('\\', "\\\\").replace('\'', "\\'"))
}

fn repr_list(values: &[String]) -> String {
    format!(
        "[{}]",
        values
            .iter()
            .map(|item| repr(item))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn unsafe_path(value: &str) -> bool {
    !value.ends_with(".py")
        || value.starts_with('/')
        || value.contains('\\')
        || value.chars().any(|ch| (ch as u32) < 32 || ch as u32 == 127)
        || value.chars().any(|ch| "*?[".contains(ch))
        || value
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
}

pub(super) fn path_unsafe(request: &Value) -> Result<Value, String> {
    Ok(json!(request.as_str().is_some_and(unsafe_path)))
}

fn labels(value: &Value) -> BTreeSet<String> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}

fn divergence_text(parsed: &Map<String, Value>, derived: &Map<String, Value>) -> String {
    let mut missing: Vec<_> = derived
        .keys()
        .filter(|path| !parsed.contains_key(*path))
        .cloned()
        .collect();
    let mut extra: Vec<_> = parsed
        .keys()
        .filter(|path| !derived.contains_key(*path))
        .cloned()
        .collect();
    let mut drifted: Vec<_> = parsed
        .keys()
        .filter(|path| {
            derived
                .get(*path)
                .is_some_and(|value| labels(value) != labels(&parsed[*path]))
        })
        .cloned()
        .collect();
    missing.sort();
    extra.sort();
    drifted.sort();
    format!(
        "missing-from-inventory={} ({}), not-in-anchor-tree={} ({}), label-drift={} ({})",
        repr_list(&missing[..missing.len().min(3)]),
        missing.len(),
        repr_list(&extra[..extra.len().min(3)]),
        extra.len(),
        repr_list(&drifted[..drifted.len().min(3)]),
        drifted.len()
    )
}

pub(super) fn divergence(request: &Value) -> Result<Value, String> {
    let parsed = request["parsed"]
        .as_object()
        .ok_or("parsed inventory must be an object")?;
    let derived = request["derived"]
        .as_object()
        .ok_or("derived inventory must be an object")?;
    Ok(json!(divergence_text(parsed, derived)))
}

fn valid_envelope(request: &Value) -> Result<&Map<String, Value>, String> {
    let payload = &request["payload"];
    let tests = payload.get("tests").and_then(Value::as_object);
    if tests.is_none_or(Map::is_empty)
        || payload["schema"] != request["expected_schema"]
        || payload["anchor_commit"] != request["anchor_commit"]
        || payload["milestone"] != request["milestone"]
    {
        return Err("grandfather inventory fails schema validation".into());
    }
    Ok(tests.expect("checked above"))
}

fn parsed_inventory(tests: &Map<String, Value>) -> Result<Map<String, Value>, String> {
    let mut parsed = Map::new();
    let mut seen_nodeids = HashSet::new();
    for (path, value) in tests {
        if unsafe_path(path) {
            return Err(format!(
                "grandfather inventory entry path is unsafe: {}",
                repr(path)
            ));
        }
        let Some(rows) = value.as_array().filter(|rows| !rows.is_empty()) else {
            return Err(format!(
                "grandfather inventory entry is malformed: {}",
                repr(path)
            ));
        };
        if rows
            .iter()
            .any(|row| row.as_str().is_none_or(str::is_empty))
        {
            return Err(format!(
                "grandfather inventory entry is malformed: {}",
                repr(path)
            ));
        }
        let labels = rows.iter().filter_map(Value::as_str).collect::<Vec<_>>();
        let unique = labels.iter().copied().collect::<HashSet<_>>();
        if unique.len() != labels.len() {
            return Err(format!(
                "grandfather inventory has duplicate nodeids in {path}"
            ));
        }
        let mut full = labels
            .iter()
            .map(|label| format!("{path}::{label}"))
            .collect::<Vec<_>>();
        full.sort();
        for nodeid in full {
            if !seen_nodeids.insert(nodeid.clone()) {
                return Err(format!(
                    "grandfather inventory has duplicate nodeids across paths: {nodeid}"
                ));
            }
        }
        parsed.insert(path.clone(), value.clone());
    }
    Ok(parsed)
}

pub(super) fn validate(request: &Value) -> Result<Value, String> {
    let tests = valid_envelope(request)?;
    let parsed = parsed_inventory(tests)?;
    let derived = request["derived"]
        .as_object()
        .ok_or("derived inventory must be an object")?;
    if parsed.len() != derived.len()
        || parsed.iter().any(|(path, value)| {
            derived
                .get(path)
                .is_none_or(|other| labels(value) != labels(other))
        })
    {
        return Err(format!(
            "grandfather inventory does not match the inventory derived from the anchored tree: {}",
            divergence_text(&parsed, derived)
        ));
    }
    let present: HashSet<&str> = request["present_paths"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let dead: HashSet<&str> = request["dead_paths"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let effective: Map<_, _> = parsed
        .into_iter()
        .filter(|(path, _)| present.contains(path.as_str()) && !dead.contains(path.as_str()))
        .collect();
    Ok(Value::Object(effective))
}
