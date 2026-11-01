use super::{array, string, Finding};
use crate::candidate_policy::{GlobWorkspace, PreparedGlob};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::Path;

const CHECK: &str = "candidate-integrity";
const MAX_ROWS: usize = 100_000;
const MAX_PATH: usize = 4096;
const MAX_TEXT: usize = 128 * 1024 * 1024;
const MAX_GLOBS: usize = 4096;
const MAX_GLOB_CHARS: usize = 4096;
const MAX_MATCH_WORK: usize = 100_000_000;

#[derive(Deserialize)]
struct Entry<'a> {
    path: &'a str,
    mode: &'a str,
    folded: &'a str,
    repr: &'a str,
}

#[derive(Deserialize)]
struct Change<'a> {
    path: &'a str,
    #[serde(borrow)]
    old_path: Option<&'a str>,
    status: &'a str,
    new_mode: &'a str,
    new_oid: &'a str,
    #[serde(borrow)]
    classes: Vec<&'a str>,
    deleted: bool,
    exists: Option<bool>,
    #[serde(borrow)]
    target: Option<&'a str>,
    size: Option<u64>,
}

fn entries<'a>(raw: &'a [Value], budget: &mut usize) -> Result<Vec<Entry<'a>>, String> {
    if raw.len() > MAX_ROWS {
        return Err("candidate integrity entry count exceeds 100000".into());
    }
    let mut entries = Vec::with_capacity(raw.len());
    for row in raw {
        let entry = Entry::deserialize(row).map_err(|error| error.to_string())?;
        path_budget(entry.path, budget)?;
        text_budget(&[entry.mode, entry.folded, entry.repr], budget)?;
        entries.push(entry);
    }
    Ok(entries)
}

pub fn tree_integrity(raw: &[Value]) -> Result<Vec<Finding>, String> {
    let entries = entries(raw, &mut 0)?;
    tree_findings(&entries)
}

fn tree_findings(entries: &[Entry<'_>]) -> Result<Vec<Finding>, String> {
    let mut findings = Vec::new();
    let mut folded_paths: HashMap<&str, &Entry<'_>> = HashMap::new();
    let mut output_budget = 0usize;
    for entry in entries {
        if let Some(other) = folded_paths.get(entry.folded) {
            if other.path != entry.path {
                text_budget(&[other.repr, entry.repr, entry.path], &mut output_budget)?;
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
                    .path(entry.path),
                );
            }
        }
        folded_paths.insert(entry.folded, entry);
        if !matches!(entry.mode, "100644" | "100755" | "120000" | "160000") {
            text_budget(&[entry.mode, entry.path], &mut output_budget)?;
            findings.push(
                Finding::new(
                    CHECK,
                    "unsupported-git-mode",
                    "critical",
                    format!("unsupported Git mode {} in candidate tree", entry.mode),
                )
                .path(entry.path),
            );
        }
    }
    Ok(findings)
}

fn protected_findings(
    change: &Change<'_>,
    patterns: &[PreparedGlob],
    work: &mut GlobWorkspace,
) -> Vec<Finding> {
    let old = change.old_path.unwrap_or(change.path);
    let was_protected = patterns.iter().any(|pattern| work.matches(old, pattern));
    let stays_protected = patterns
        .iter()
        .any(|pattern| work.matches(change.path, pattern));
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
                json!({"destination": if change.deleted { None } else { Some(change.path) }}),
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
            .path(change.path),
        );
    }
    findings
}

fn materialized_findings(
    change: &Change<'_>,
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
        .path(change.path)];
    }
    if mode == Some("120000") {
        return vec![Finding::new(
            CHECK,
            "symlink-admission",
            "medium",
            "tracked symlink requires explicit review; target is confined to the snapshot",
        )
        .path(change.path)
        .evidence(json!({"target": change.target}))];
    }
    let size = change.size.unwrap_or(0);
    let binary = change.classes.contains(&"binary");
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
            .path(change.path)
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
            .path(change.path),
        );
    }
    findings
}

fn text_budget(values: &[&str], budget: &mut usize) -> Result<(), String> {
    for value in values {
        *budget = budget
            .checked_add(value.len())
            .ok_or("candidate text budget overflow")?;
        if *budget > MAX_TEXT {
            return Err("candidate integrity text exceeds 128 MiB".into());
        }
    }
    Ok(())
}
fn path_budget(value: &str, budget: &mut usize) -> Result<(), String> {
    if value.len() > MAX_PATH {
        return Err("candidate integrity path exceeds 4096 bytes".into());
    }
    text_budget(&[value], budget)
}
fn changes<'a>(raw: &'a [Value], budget: &mut usize) -> Result<Vec<Change<'a>>, String> {
    if raw.len() > MAX_ROWS {
        return Err("candidate integrity change count exceeds 100000".into());
    }
    let mut class_count = 0usize;
    let mut changes = Vec::with_capacity(raw.len());
    for row in raw {
        let classes = row["classes"]
            .as_array()
            .ok_or("candidate change classes must be an array")?;
        class_count = class_count
            .checked_add(classes.len())
            .ok_or("candidate class count overflow")?;
        if class_count > MAX_ROWS {
            return Err("candidate class count exceeds 100000".into());
        }
        let change = Change::deserialize(row).map_err(|error| error.to_string())?;
        path_budget(change.path, budget)?;
        if let Some(path) = change.old_path {
            path_budget(path, budget)?;
        }
        if let Some(path) = change.target {
            path_budget(path, budget)?;
        }
        text_budget(&[change.status, change.new_mode, change.new_oid], budget)?;
        text_budget(&change.classes, budget)?;
        changes.push(change);
    }
    Ok(changes)
}
fn prepared_patterns(raw: &[Value], changes: &[Change<'_>]) -> Result<Vec<PreparedGlob>, String> {
    if raw.len() > MAX_GLOBS {
        return Err("protected glob count exceeds 4096".into());
    }
    let mut patterns = Vec::with_capacity(raw.len());
    let mut weight = 0usize;
    for value in raw {
        let pattern = value.as_str().ok_or("protected glob must be a string")?;
        let chars = pattern.chars().count();
        if chars > MAX_GLOB_CHARS {
            return Err("protected glob exceeds 4096 characters".into());
        }
        weight = weight
            .checked_add(chars + 1)
            .ok_or("protected glob work overflow")?;
        patterns.push(pattern);
    }
    let mut path_weight = 0usize;
    for change in changes {
        for path in [change.old_path.unwrap_or(change.path), change.path] {
            path_weight = path_weight
                .checked_add(path.chars().count() + 1)
                .ok_or("protected glob work overflow")?;
        }
    }
    if path_weight
        .checked_mul(weight)
        .ok_or("protected glob work overflow")?
        > MAX_MATCH_WORK
    {
        return Err("protected glob work exceeds 100000000".into());
    }
    Ok(patterns.into_iter().map(PreparedGlob::new).collect())
}

pub fn candidate_integrity(payload: &Value) -> Result<Value, String> {
    let mut budget = 0;
    let entries = entries(array(payload, "entries")?, &mut budget)?;
    let changes = changes(array(payload, "changes")?, &mut budget)?;
    let patterns = prepared_patterns(array(payload, "protected_globs")?, &changes)?;
    let max_file = payload["max_file_bytes"]
        .as_u64()
        .ok_or("max_file_bytes must be an integer")?;
    let max_binary = payload["max_binary_bytes"]
        .as_u64()
        .ok_or("max_binary_bytes must be an integer")?;
    let mut findings = tree_findings(&entries)?;
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
        .map(|entry| (entry.path, entry.mode))
        .collect();
    let mut work = GlobWorkspace::default();
    for change in &changes {
        if change.new_mode == "160000" {
            findings.push(
                Finding::new(
                    CHECK,
                    "submodule-admission",
                    "high",
                    "new or modified submodule/gitlink requires a narrow owned exception",
                )
                .path(change.path)
                .evidence(json!({"gitlink_oid": change.new_oid})),
            );
        }
        findings.extend(protected_findings(change, &patterns, &mut work));
        findings.extend(materialized_findings(
            change,
            modes.get(change.path).copied(),
            max_file,
            max_binary,
        ));
    }
    Ok(
        json!({"findings": findings, "metrics": {"tree_entries": entries.len(), "changes": changes.len()}}),
    )
}

pub fn dependency_integrity(payload: &Value) -> Result<Value, String> {
    let snapshot = Path::new(string(payload, "snapshot")?);
    dependency_integrity_with(payload, |path| {
        match std::fs::metadata(snapshot.join(path)) {
            Ok(metadata) => Ok(metadata.is_file()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(format!("cannot inspect candidate lockfile {path}: {error}")),
        }
    })
}

/// The caller supplies snapshot-confined metadata lookup; changed paths alone
/// are never sufficient evidence that the candidate contains a lock file.
pub fn dependency_integrity_with(
    payload: &Value,
    mut is_file: impl FnMut(&str) -> Result<bool, String>,
) -> Result<Value, String> {
    let raw = array(payload, "paths")?;
    if raw.len() > MAX_ROWS {
        return Err("dependency path count exceeds 100000".into());
    }
    let paths = string_set(raw)?;
    let mut budget = 0;
    for path in &paths {
        path_budget(path, &mut budget)?;
        if path.starts_with('/') || path.split('/').any(|p| matches!(p, "" | "." | "..")) {
            return Err("dependency path must be a confined relative path".into());
        }
    }
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
            if !is_file(&expected)? {
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
    Ok(json!({"files": select_files(array(payload, "changes")?, payload)?}))
}

pub fn select_files<'a>(changes: &'a [Value], policy: &Value) -> Result<Vec<&'a str>, String> {
    let included = string_set(array(policy, "classes")?)?;
    let excluded = string_set(array(policy, "exclude_classes")?)?;
    let deletions = policy["run_on_deletions"]
        .as_bool()
        .ok_or("run_on_deletions must be boolean")?;
    let mut files = Vec::new();
    for change in changes {
        let classes = array(change, "classes")?;
        if classes.iter().any(|value| !value.is_string()) {
            return Err("candidate field must contain only strings".into());
        }
        let mode = string(change, "new_mode")?;
        let path = string(change, "path")?;
        let selected = included.is_empty()
            || classes
                .iter()
                .any(|class| included.contains(class.as_str().unwrap()));
        let blocked = classes
            .iter()
            .any(|class| excluded.contains(class.as_str().unwrap()));
        if (deletions || mode != "000000") && selected && !blocked {
            files.push(path);
        }
    }
    files.sort_unstable();
    files.dedup();
    Ok(files)
}

fn string_set(values: &[Value]) -> Result<HashSet<&str>, String> {
    values
        .iter()
        .map(|value| {
            value
                .as_str()
                .ok_or_else(|| "candidate field must contain only strings".to_owned())
        })
        .collect()
}

#[cfg(test)]
#[path = "candidate_checks_integrity_tests.rs"]
mod tests;
