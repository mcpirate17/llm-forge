//! Exact-path mutation waivers and base-bound value waivers.

use super::candidate_policy_value::*;
use serde_json::{json, Value};
use std::collections::HashSet;

const WAIVER_KEYS: &[&str] = &[
    "id",
    "path",
    "owner",
    "justification",
    "expires",
    "milestone",
    "integration_base",
    "source_anchor",
    "sha256",
    "binding_clause",
    "sources",
];
const REQUIRED_WAIVER_KEYS: &[&str] = &[
    "id",
    "path",
    "owner",
    "justification",
    "expires",
    "milestone",
    "integration_base",
    "source_anchor",
    "sha256",
    "binding_clause",
];
const VALUE_KEYS: &[&str] = &[
    "integration_base",
    "nodeids",
    "reason",
    "approved_by",
    "approved_on",
    "expires",
];
const BINDING: &str = "Any future edit to this test file or any pinned source file, or revival of its lane, voids this waiver and requires a mutation campaign before renewal.";
const BASE: &str = "d3697f22c2cb974dbae2d2dc4847c99d0de92224";
const ANCHOR: &str = "61343f575215dd222a74fc2c060d0328692ded5e";
const MILESTONE: &str = "w7-trident-linear-integration";

fn digest(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn oid(s: &str) -> bool {
    s.len() == 40
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn exact_path(path: &str) -> Option<Vec<&str>> {
    let parts: Vec<_> = path.split('/').collect();
    if path.starts_with('/')
        || path.contains('\\')
        || path.chars().any(|c| c.is_control() || "*?[".contains(c))
        || parts.len() < 2
        || parts
            .iter()
            .any(|p| p.is_empty() || *p == "." || *p == "..")
        || !path.ends_with(".py")
    {
        return None;
    }
    Some(parts)
}

fn test_shaped(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    path.split('/').any(|p| p == "test")
        || name.starts_with("test_")
        || [
            "_test.py",
            "_test.c",
            "_test.cc",
            "_test.cpp",
            "_test.cxx",
            ".test.js",
            ".test.jsx",
            ".test.ts",
            ".test.tsx",
            ".spec.js",
            ".spec.jsx",
            ".spec.ts",
            ".spec.tsx",
            "Test.java",
        ]
        .iter()
        .any(|s| name.ends_with(s))
}

fn sources(raw: &Value, waiver_id: &str) -> Result<Vec<Value>> {
    let Some(entries) = raw.as_array().filter(|entries| !entries.is_empty()) else {
        return Err(format!("mutation waiver '{waiver_id}' requires sources: a non-empty array of {{path = ..., sha256 = ...}} tables pinning its production inputs"));
    };
    let mut seen = HashSet::new();
    let mut out = Vec::with_capacity(entries.len());
    for entry in entries {
        let Some(map) = entry
            .as_object()
            .filter(|map| exact_keys(map, &["path", "sha256"]))
        else {
            return Err(format!(
                "mutation waiver '{waiver_id}' source entries need exactly path and sha256 keys"
            ));
        };
        let path = py_string(value(map, "path"));
        let valid = exact_path(&path).is_some_and(|parts| {
            !parts.last().unwrap_or(&"").starts_with("test_")
                && !parts[..parts.len() - 1].contains(&"tests")
        });
        if !valid {
            return Err(format!("mutation waiver '{waiver_id}' source paths must be exact repo-relative non-test .py files: '{path}'"));
        }
        if test_shaped(&path) {
            return Err(format!("mutation waiver '{waiver_id}' source path is test-shaped (tests/ segment, test_ prefix, or test suffix) and cannot be pinned as production source: '{path}'"));
        }
        let sha = py_string(value(map, "sha256"));
        if !digest(&sha) {
            return Err(format!(
                "mutation waiver '{waiver_id}' source sha256 must be 64 lowercase hex digits"
            ));
        }
        if !seen.insert(path.clone()) {
            return Err(format!(
                "mutation waiver '{waiver_id}' pins duplicate source paths"
            ));
        }
        out.push(json!({"path": path, "sha256": sha}));
    }
    Ok(out)
}

fn mutation_waiver(entry: &Value) -> Result<Value> {
    let map = table(entry, "each mutation_waivers entry must be a table")?;
    unknown(map, WAIVER_KEYS, "mutation waiver has unknown keys")?;
    missing(
        map,
        REQUIRED_WAIVER_KEYS,
        "mutation waiver is missing required keys",
    )?;
    let id = value(map, "id")
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .ok_or("mutation waiver id must be a non-empty string")?;
    let path = py_string(value(map, "path"));
    let valid = exact_path(&path).is_some_and(|parts| {
        parts.last().unwrap_or(&"").starts_with("test_")
            && parts[..parts.len() - 1].contains(&"tests")
    });
    if !valid {
        return Err(format!("mutation waiver path must be one exact repo-relative test file path (test_*.py) under a real tests/ directory with no glob metacharacters or traversal: '{path}'"));
    }
    let owner = py_string(value(map, "owner")).trim().to_owned();
    let justification = py_string(value(map, "justification")).trim().to_owned();
    if owner.len() < 2 || justification.len() < 20 {
        return Err(format!(
            "mutation waiver '{id}' owner/justification is not specific enough"
        ));
    }
    if py_string(value(map, "milestone")) != MILESTONE {
        return Err(format!(
            "mutation waiver '{id}' pins an unknown milestone; expected '{MILESTONE}'"
        ));
    }
    if py_string(value(map, "integration_base")) != BASE {
        return Err(format!(
            "mutation waiver '{id}' pins an unexpected integration base"
        ));
    }
    if py_string(value(map, "source_anchor")) != ANCHOR {
        return Err(format!(
            "mutation waiver '{id}' pins an unexpected source anchor"
        ));
    }
    let sha = py_string(value(map, "sha256"));
    if !digest(&sha) {
        return Err(format!(
            "mutation waiver '{id}' sha256 must be 64 lowercase hex digits"
        ));
    }
    if py_string(value(map, "binding_clause")) != BINDING {
        return Err(format!("mutation waiver '{id}' binding clause must match the canonical void-on-edit clause verbatim"));
    }
    let expires = date(value(map, "expires"), "mutation_waivers.expires")?;
    Ok(
        json!({"waiver_id": id, "path": path, "owner": owner, "justification": justification,
        "expires": expires, "milestone": MILESTONE, "integration_base": BASE, "source_anchor": ANCHOR,
        "sha256": sha, "binding_clause": BINDING, "sources": sources(value(map, "sources"), id)?}),
    )
}

pub(super) fn mutation_waivers(raw: &Value) -> Result<Vec<Value>> {
    if raw.is_null() {
        return Ok(vec![]);
    }
    let entries = raw
        .as_array()
        .ok_or("mutation_waivers must be an array of tables")?;
    let waivers: Vec<_> = entries.iter().map(mutation_waiver).collect::<Result<_>>()?;
    let ids: Vec<_> = waivers
        .iter()
        .filter_map(|w| w["waiver_id"].as_str())
        .collect();
    let duplicated: Vec<String> = ids
        .iter()
        .filter(|id| ids.iter().filter(|other| other == id).count() > 1)
        .map(|id| (*id).to_owned())
        .collect();
    if !duplicated.is_empty() {
        let mut names = duplicated;
        names.sort();
        names.dedup();
        return Err(format!(
            "mutation waiver identifiers must be unique; duplicated: {}",
            names.join(", ")
        ));
    }
    let paths: HashSet<_> = waivers.iter().filter_map(|w| w["path"].as_str()).collect();
    if paths.len() != waivers.len() {
        return Err("mutation waiver paths must be unique; split overlapping lanes into separate exact-path entries".into());
    }
    Ok(waivers)
}

fn nodeids(raw: &Value) -> Result<Vec<String>> {
    let Some(entries) = raw.as_array().filter(|entries| !entries.is_empty()) else {
        return Err("value_waivers.nodeids must be a non-empty array of nodeids".into());
    };
    let mut out = Vec::with_capacity(entries.len());
    for entry in entries {
        let Some(nodeid) = entry
            .as_str()
            .filter(|s| s.contains("::") && *s == s.trim())
        else {
            return Err(format!(
                "value_waivers.nodeids entries must be exact pytest nodeids (path::name): {}",
                repr(entry)
            ));
        };
        if nodeid.chars().any(|c| "*?[".contains(c)) {
            return Err(format!(
                "value_waivers.nodeids are exact, never patterns: '{nodeid}'"
            ));
        }
        out.push(nodeid.to_owned());
    }
    if out.iter().collect::<HashSet<_>>().len() != out.len() {
        return Err("value_waivers.nodeids must not repeat a nodeid".into());
    }
    Ok(out)
}

fn value_waiver(entry: &Value) -> Result<Value> {
    let map = table(entry, "each value_waivers entry must be a table")?;
    unknown(map, VALUE_KEYS, "value waiver has unknown keys")?;
    missing(map, &VALUE_KEYS[..5], "value waiver is missing keys")?;
    let base = value(map, "integration_base");
    if !base.as_str().is_some_and(oid) {
        return Err(format!("value_waivers.integration_base must be the full 40-hex commit oid of the integration base it binds to: {}", repr(base)));
    }
    for key in ["reason", "approved_by"] {
        if value(map, key).as_str().is_none_or(|s| s.trim().is_empty()) {
            return Err(format!("value_waivers.{key} must be a non-empty string"));
        }
    }
    let expiry = if value(map, "expires").is_null() {
        None
    } else {
        Some(date(value(map, "expires"), "value_waivers.expires")?)
    };
    Ok(
        json!({"integration_base": base, "nodeids": nodeids(value(map, "nodeids"))?,
        "reason": value(map, "reason").as_str().unwrap_or("").trim(),
        "approved_by": value(map, "approved_by").as_str().unwrap_or("").trim(),
        "approved_on": date(value(map, "approved_on"), "value_waivers.approved_on")?, "expires": expiry}),
    )
}

pub fn parse_value_waivers(raw: &Value) -> Result<Vec<Value>> {
    if raw.is_null() {
        return Ok(vec![]);
    }
    let entries = raw
        .as_array()
        .ok_or("value_waivers must be an array of tables")?;
    let waivers: Vec<_> = entries.iter().map(value_waiver).collect::<Result<_>>()?;
    let mut seen = HashSet::new();
    for waiver in &waivers {
        let ns = waiver["nodeids"].as_array().expect("normalized nodeids");
        let overlap: Vec<_> = ns
            .iter()
            .filter_map(Value::as_str)
            .filter(|n| !seen.insert((*n).to_owned()))
            .map(str::to_owned)
            .collect();
        if !overlap.is_empty() {
            return Err(format!(
                "value waivers overlap on nodeids: {}",
                repr_list(&overlap)
            ));
        }
    }
    Ok(waivers)
}
