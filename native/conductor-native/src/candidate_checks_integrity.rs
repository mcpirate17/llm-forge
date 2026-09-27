use super::{array, string, Finding};
use crate::candidate_policy::glob_match;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::Path;

const CHECK: &str = "candidate-integrity";

#[derive(Deserialize)]
struct Entry {
    path: String,
    mode: String,
    folded: String,
    repr: String,
}

#[derive(Deserialize)]
struct Change {
    path: String,
    old_path: Option<String>,
    status: String,
    new_mode: String,
    new_oid: String,
    classes: Vec<String>,
    deleted: bool,
    exists: Option<bool>,
    target: Option<String>,
    size: Option<u64>,
}

fn entries(raw: &[Value]) -> Result<Vec<Entry>, String> {
    raw.iter()
        .cloned()
        .map(|row| serde_json::from_value(row).map_err(|error| error.to_string()))
        .collect()
}

pub fn tree_integrity(raw: &[Value]) -> Result<Vec<Finding>, String> {
    let entries = entries(raw)?;
    let mut findings = Vec::new();
    let mut folded_paths: HashMap<&str, &Entry> = HashMap::new();
    for entry in &entries {
        if let Some(other) = folded_paths.get(entry.folded.as_str()) {
            if other.path != entry.path {
                findings.push(
                    Finding::new(
                        CHECK,
                        "case-collision",
                        "critical",
                        format!(
                            "case-colliding tree paths are not portable: {} and {}",
                            other.repr, entry.repr
                        ),
                    )
                    .path(&entry.path),
                );
            }
        }
        folded_paths.insert(&entry.folded, entry);
        if !matches!(
            entry.mode.as_str(),
            "100644" | "100755" | "120000" | "160000"
        ) {
            findings.push(
                Finding::new(
                    CHECK,
                    "unsupported-git-mode",
                    "critical",
                    format!("unsupported Git mode {} in candidate tree", entry.mode),
                )
                .path(&entry.path),
            );
        }
    }
    Ok(findings)
}

fn protected_findings(change: &Change, patterns: &[String]) -> Vec<Finding> {
    let old = change.old_path.as_deref().unwrap_or(&change.path);
    let was_protected = patterns.iter().any(|pattern| glob_match(old, pattern));
    let stays_protected = patterns
        .iter()
        .any(|pattern| glob_match(&change.path, pattern));
    let mut findings = Vec::new();
    if was_protected && (change.deleted || !stays_protected) {
        findings.push(
            Finding::new(
                CHECK,
                "protected-delete-or-move",
                "critical",
                format!("protected artifact is deleted or moved out of protection: {old}"),
            )
            .path(old)
            .evidence(
                json!({"destination": if change.deleted { None } else { Some(&change.path) }}),
            ),
        );
    }
    if was_protected && change.status.starts_with('M') {
        findings.push(
            Finding::new(
                CHECK,
                "protected-overwrite",
                "high",
                "protected artifact overwrite requires a bound regeneration receipt",
            )
            .path(&change.path),
        );
    }
    findings
}

fn materialized_findings(
    change: &Change,
    mode: Option<&str>,
    max_file: u64,
    max_binary: u64,
) -> Vec<Finding> {
    if change.deleted {
        return Vec::new();
    }
    if mode.is_none() || change.exists != Some(true) {
        return vec![Finding::new(
            CHECK,
            "incomplete-snapshot",
            "critical",
            "candidate path was not materialized from its Git object",
        )
        .path(&change.path)];
    }
    if mode == Some("120000") {
        return vec![Finding::new(
            CHECK,
            "symlink-admission",
            "medium",
            "tracked symlink requires explicit review; target is confined to the snapshot",
        )
        .path(&change.path)
        .evidence(json!({"target": change.target}))];
    }
    let size = change.size.unwrap_or(0);
    let binary = change.classes.iter().any(|class| class == "binary");
    let limit = if binary { max_binary } else { max_file };
    let mut findings = Vec::new();
    if size > limit {
        findings.push(
            Finding::new(
                CHECK,
                "oversized-artifact",
                "high",
                format!("candidate file is {size} bytes; policy limit is {limit}"),
            )
            .path(&change.path)
            .evidence(json!({"size_bytes": size, "limit_bytes": limit})),
        );
    }
    if binary && change.status.starts_with('A') {
        findings.push(
            Finding::new(
                CHECK,
                "binary-admission",
                "high",
                "new binary/model artifact is forbidden without an owned narrow exception",
            )
            .path(&change.path),
        );
    }
    findings
}

pub fn candidate_integrity(payload: &Value) -> Result<Value, String> {
    let raw_entries = array(payload, "entries")?;
    let entries = entries(raw_entries)?;
    let changes: Vec<Change> = array(payload, "changes")?
        .iter()
        .cloned()
        .map(|row| serde_json::from_value(row).map_err(|error| error.to_string()))
        .collect::<Result<_, _>>()?;
    let patterns: Vec<String> = serde_json::from_value(payload["protected_globs"].clone())
        .map_err(|error| error.to_string())?;
    let max_file = payload["max_file_bytes"]
        .as_u64()
        .ok_or("max_file_bytes must be an integer")?;
    let max_binary = payload["max_binary_bytes"]
        .as_u64()
        .ok_or("max_binary_bytes must be an integer")?;
    let mut findings = tree_integrity(raw_entries)?;
    if changes.is_empty() && string(payload, "surface")? == "ci" {
        findings.push(
            Finding::new(
                CHECK,
                "empty-ci-range",
                "critical",
                "CI candidate range is empty; refusing a no-op governance pass.",
            )
            .help("Resolve and pass an explicit merge-base-to-candidate range."),
        );
    }
    let modes: HashMap<_, _> = entries
        .iter()
        .map(|entry| (entry.path.as_str(), entry.mode.as_str()))
        .collect();
    for change in &changes {
        if change.new_mode == "160000" {
            findings.push(
                Finding::new(
                    CHECK,
                    "submodule-admission",
                    "high",
                    "new or modified submodule/gitlink requires a narrow owned exception",
                )
                .path(&change.path)
                .evidence(json!({"gitlink_oid": change.new_oid})),
            );
        }
        findings.extend(protected_findings(change, &patterns));
        findings.extend(materialized_findings(
            change,
            modes.get(change.path.as_str()).copied(),
            max_file,
            max_binary,
        ));
    }
    Ok(
        json!({"findings": findings, "metrics": {"tree_entries": entries.len(), "changes": changes.len()}}),
    )
}

pub fn dependency_integrity(payload: &Value) -> Result<Value, String> {
    let paths: HashSet<&str> = array(payload, "paths")?
        .iter()
        .filter_map(Value::as_str)
        .collect();
    let snapshot = Path::new(string(payload, "snapshot")?);
    let mut findings = Vec::new();
    for (manifest, lock) in [
        ("pyproject.toml", "uv.lock"),
        ("package.json", "package-lock.json"),
        ("Cargo.toml", "Cargo.lock"),
    ] {
        let suffix = format!("/{manifest}");
        let mut touched_manifests: Vec<&str> = paths
            .iter()
            .copied()
            .filter(|path| *path == manifest || path.ends_with(&suffix))
            .collect();
        touched_manifests.sort_unstable();
        for touched in touched_manifests {
            let prefix = &touched[..touched.len() - manifest.len()];
            let expected = format!("{prefix}{lock}");
            if !paths.contains(expected.as_str()) && !snapshot.join(&expected).is_file() {
                findings.push(
                    Finding::new(
                        "dependency-integrity",
                        "missing-lockfile",
                        "critical",
                        format!("dependency manifest has no candidate lockfile: {expected}"),
                    )
                    .path(touched),
                );
            }
        }
    }
    Ok(json!({"findings": findings, "metrics": {"dependency_files": paths.len()}}))
}

pub fn files_for_policy(payload: &Value) -> Result<Value, String> {
    let included: HashSet<&str> = array(payload, "classes")?
        .iter()
        .filter_map(Value::as_str)
        .collect();
    let excluded: HashSet<&str> = array(payload, "exclude_classes")?
        .iter()
        .filter_map(Value::as_str)
        .collect();
    let deletions = payload["run_on_deletions"]
        .as_bool()
        .ok_or("run_on_deletions must be boolean")?;
    let mut files = Vec::new();
    for change in array(payload, "changes")? {
        let classes = array(change, "classes")?;
        let selected = included.is_empty()
            || classes
                .iter()
                .filter_map(Value::as_str)
                .any(|class| included.contains(class));
        let blocked = classes
            .iter()
            .filter_map(Value::as_str)
            .any(|class| excluded.contains(class));
        if (deletions || change["new_mode"] != "000000") && selected && !blocked {
            files.push(string(change, "path")?);
        }
    }
    files.sort_unstable();
    files.dedup();
    Ok(json!({"files": files}))
}
