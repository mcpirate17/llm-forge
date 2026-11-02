//! Candidate duplicate-body policy over a complete caller-supplied peer inventory.
//! No filesystem reads, interpreter, ambient repository state or unbound AST input.
use crate::candidate_python::Document;
use crate::duplicate_fingerprints::{analyze_document, FingerprintPolicy};
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashMap};

const MAX_FILES: usize = 4096;
const MAX_TEXT: usize = 1024 * 1024;
const MAX_TOTAL: usize = 32 * 1024 * 1024;
const MAX_NODES: usize = 1_000_000;
const MAX_CHANGED_LINES: usize = 65_536;
const MAX_FINDINGS: usize = 4096;
const MAX_OUTPUT: usize = 32 * 1024 * 1024;
type Location = (String, String, usize);

struct Input<'a> {
    path: &'a str,
    text: &'a str,
    lines: BTreeSet<usize>,
}
struct Group {
    locations: Vec<Location>,
}

fn array<'a>(value: &'a Value, key: &str) -> Result<&'a [Value], String> {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .ok_or_else(|| format!("duplicate-function-bodies requires array {key}"))
}
fn path(value: &Value) -> Result<&str, String> {
    let path = value
        .as_str()
        .ok_or("duplicate-function-bodies requires string path")?;
    if path.is_empty()
        || path.len() > 4096
        || path.contains(['\0', '\\'])
        || path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err("duplicate-function-bodies requires bounded relative paths".into());
    }
    Ok(path)
}

fn inputs(payload: &Value) -> Result<(Vec<Input<'_>>, Vec<String>), String> {
    let files = array(payload, "files")?;
    let changed_values = array(payload, "changed")?;
    if files.len() > MAX_FILES || changed_values.len() > MAX_FILES {
        return Err("duplicate-function-bodies file count exceeds 4096".into());
    }
    let mut changed = BTreeSet::new();
    for value in changed_values {
        changed.insert(path(value)?.to_owned());
    }
    let mut seen = BTreeSet::new();
    let mut total = 0usize;
    let mut total_lines = 0usize;
    let mut records = Vec::with_capacity(files.len());
    for file in files {
        let name = path(
            file.get("path")
                .ok_or("duplicate-body file requires path")?,
        )?;
        if !seen.insert(name) {
            return Err(format!("duplicate-body repeated inventory path: {name}"));
        }
        let text = file
            .get("text")
            .and_then(Value::as_str)
            .ok_or("duplicate-body file requires string text")?;
        total = total
            .checked_add(text.len())
            .ok_or("duplicate-body text size overflow")?;
        if text.len() > MAX_TEXT || total > MAX_TOTAL {
            return Err("duplicate-body text exceeds 1 MiB/file or 32 MiB aggregate".into());
        }
        let raw_lines = array(file, "changed_lines")?;
        total_lines = total_lines
            .checked_add(raw_lines.len())
            .ok_or("duplicate-body changed line count overflow")?;
        if total_lines > MAX_CHANGED_LINES {
            return Err("duplicate-body changed line count exceeds limit".into());
        }
        let mut lines = BTreeSet::new();
        for line in raw_lines {
            let n = line
                .as_u64()
                .and_then(|n| usize::try_from(n).ok())
                .filter(|n| *n > 0)
                .ok_or("duplicate-body changed lines must be positive integers")?;
            lines.insert(n);
        }
        if !lines.is_empty() && !changed.contains(name) {
            return Err(format!(
                "duplicate-body changed lines outside changed inventory: {name}"
            ));
        }
        records.push(Input {
            path: name,
            text,
            lines,
        });
    }
    if changed.iter().any(|name| !seen.contains(name.as_str())) {
        return Err("duplicate-body changed inventory is missing source".into());
    }
    Ok((records, changed.into_iter().collect()))
}

// Lexical and AST admission belongs to the shared source-bound document. All
// supplied peers also share an aggregate node budget before fingerprint walking.
fn admit_source<'a>(source: &'a str, nodes_left: &mut usize) -> Result<Document<'a>, String> {
    let document = Document::parse(source)
        .map_err(|error| format!("incomplete duplicate-body peer parse: {error}"))?;
    *nodes_left = nodes_left
        .checked_sub(document.nodes())
        .ok_or("duplicate-body AST exceeds aggregate nodes 1000000")?;
    Ok(document)
}

fn collect(records: &[Input<'_>]) -> Result<(Vec<Group>, BTreeSet<Location>, usize), String> {
    let mut groups: Vec<Group> = Vec::new();
    let mut by_digest: HashMap<String, usize> = HashMap::new();
    let mut changed = BTreeSet::new();
    let mut count = 0;
    let mut nodes_left = MAX_NODES;
    for record in records {
        let document = admit_source(record.text, &mut nodes_left)
            .map_err(|error| format!("{}: {error}", record.path))?;
        let file = analyze_document(
            record.path.to_owned(),
            &document,
            FingerprintPolicy::Candidate,
        );
        for function in file.functions {
            let location = (file.path.clone(), function.name, function.lineno);
            if record
                .lines
                .range(function.lineno..=function.end_lineno)
                .next()
                .is_some()
            {
                changed.insert(location.clone());
            }
            let next = groups.len();
            let index = *by_digest.entry(function.digest).or_insert_with(|| {
                groups.push(Group {
                    locations: Vec::new(),
                });
                next
            });
            groups[index].locations.push(location);
            count += 1;
        }
    }
    Ok((groups, changed, count))
}

// Account for the entire quadratic peer evidence before constructing any finding.
// Escaped tuple sizes also bound the first-five textual message; slack covers keys.
fn admit_findings(
    groups: &mut [Group],
    changed: &BTreeSet<Location>,
    files: &[String],
) -> Result<(), String> {
    let mut bytes = crate::candidate_cargo_audit::serialized_size(&files, MAX_OUTPUT)? + 1024;
    let mut findings = 0usize;
    for group in groups {
        group.locations.sort();
        if group.locations.len() < 2 {
            continue;
        }
        let sizes = group
            .locations
            .iter()
            .map(|loc| {
                serde_json::to_vec(loc)
                    .map(|v| v.len() + 1)
                    .map_err(|e| e.to_string())
            })
            .collect::<Result<Vec<_>, _>>()?;
        let peer_bytes = sizes.iter().sum::<usize>();
        for (index, location) in group.locations.iter().enumerate() {
            if !changed.contains(location) {
                continue;
            }
            findings += 1;
            let message_bytes = sizes
                .iter()
                .enumerate()
                .filter(|(i, _)| *i != index)
                .take(5)
                .map(|(_, size)| *size)
                .sum::<usize>()
                * 2;
            bytes = bytes
                .checked_add(1024 + sizes[index] * 2 + peer_bytes + message_bytes)
                .ok_or("duplicate-body peer evidence size overflow")?;
            if findings > MAX_FINDINGS || bytes > MAX_OUTPUT {
                return Err(
                    "duplicate-body findings exceed 4096 or peer evidence exceeds 32 MiB".into(),
                );
            }
        }
    }
    Ok(())
}

fn findings(groups: &[Group], changed: &BTreeSet<Location>) -> Vec<Value> {
    let mut result = Vec::new();
    let mut emitted = BTreeSet::new();
    for group in groups {
        if group.locations.len() < 2 {
            continue;
        }
        for location in &group.locations {
            if !changed.contains(location) || !emitted.insert(location) {
                continue;
            }
            let peers = group
                .locations
                .iter()
                .filter(|peer| *peer != location)
                .collect::<Vec<_>>();
            let message = peers
                .iter()
                .take(5)
                .map(|(path, name, line)| format!("{path}:{line} ({name})"))
                .collect::<Vec<_>>()
                .join(", ");
            result.push(json!({
                "check_id":"duplicate-function-bodies", "rule_id":"copied-function-body",
                "severity":"high", "path":location.0, "line":location.2,
                "message":format!("changed function {} duplicates {message}", location.1),
                "evidence":{"peers":peers}
            }));
        }
    }
    result
}

/// Preserve supplied peer order for digest-group insertion and breadth-first
/// function order within files; peers are sorted by (path, name, line).
/// Any malformed peer makes the whole evaluation explicitly incomplete (Err).
pub fn evaluate(payload: &Value) -> Result<Value, String> {
    let (records, files) = inputs(payload)?;
    let (mut groups, changed, count) = collect(&records)?;
    admit_findings(&mut groups, &changed, &files)?;
    let output = json!({
        "findings":findings(&groups, &changed), "files":files,
        "metrics":{"candidate_function_bodies":count, "files_expected":records.len(),
            "files_read":records.len(), "files_skipped":0, "skipped_files":{}}
    });
    crate::candidate_cargo_audit::serialized_size(&output, MAX_OUTPUT)
        .map_err(|_| "duplicate-body serialized output exceeds 32 MiB".to_owned())?;
    Ok(output)
}

#[cfg(test)]
#[path = "candidate_duplicate_policy_tests.rs"]
mod tests;
