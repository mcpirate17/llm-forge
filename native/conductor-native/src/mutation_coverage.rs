// PyO3 0.29's generated argument conversion trips this Rust 1.93 lint even
// though the handwritten functions do not perform a redundant conversion.
#![allow(clippy::useless_conversion)]

use std::collections::{BTreeSet, HashSet};
use std::env;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

#[cfg(feature = "python")]
use pyo3::exceptions::PyValueError;
#[cfg(feature = "python")]
use pyo3::prelude::*;
use serde::Serialize;
use serde_json::{json, Value};

const VALIDATOR_SIDE_KINDS: [&str; 3] = ["decode_error", "schema_error", "manifest_load_error"];

fn missing_rows(result: &Value) -> &[Value] {
    result
        .get("missing_evidence")
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

fn rejection_rows(row: &Value) -> &[Value] {
    row.get("receipt_rejections")
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

fn field<'a>(value: &'a Value, name: &str) -> &'a str {
    value.get(name).and_then(Value::as_str).unwrap_or("")
}

fn validator_side(kind: &str) -> bool {
    VALIDATOR_SIDE_KINDS.contains(&kind)
}

pub fn observed_evidence_kinds(result: &Value) -> BTreeSet<String> {
    let mut kinds = BTreeSet::new();
    if let Some(counts) = result.get("rejection_counts").and_then(Value::as_object) {
        for (kind, count) in counts {
            if count.as_i64().is_some_and(|value| value != 0) {
                kinds.insert(kind.clone());
            }
        }
    }
    for row in missing_rows(result) {
        kinds.insert(
            row.get("reason_kind")
                .and_then(Value::as_str)
                .unwrap_or("not_pass")
                .to_owned(),
        );
        for rejection in rejection_rows(row) {
            kinds.insert(
                rejection
                    .get("kind")
                    .and_then(Value::as_str)
                    .unwrap_or("schema_error")
                    .to_owned(),
            );
        }
    }
    kinds
}

pub fn evidence_exit_code(result: &Value) -> i32 {
    if observed_evidence_kinds(result)
        .iter()
        .any(|kind| validator_side(kind))
    {
        5
    } else if !missing_rows(result).is_empty() {
        6
    } else {
        0
    }
}

pub fn canary_verdict(mut report: Value) -> Result<Value, String> {
    let counts = report
        .get("rejection_counts")
        .and_then(Value::as_object)
        .ok_or("coverage report rejection_counts must be an object")?;
    let offending = VALIDATOR_SIDE_KINDS
        .iter()
        .copied()
        .filter(|kind| {
            counts
                .get(*kind)
                .and_then(Value::as_i64)
                .is_some_and(|count| count != 0)
        })
        .collect::<Vec<_>>();
    let mut offending = offending;
    offending.sort_unstable();
    let mut offenders = Vec::new();
    for row in missing_rows(&report) {
        for rejection in rejection_rows(row) {
            if validator_side(field(rejection, "kind")) {
                offenders.push(json!({
                    "path": field(row, "path"),
                    "receipt": field(rejection, "receipt"),
                    "kind": field(rejection, "kind"),
                    "detail": field(rejection, "detail"),
                }));
            }
        }
    }
    if let Some(malformed) = report.get("malformed_receipts").and_then(Value::as_array) {
        for receipt in malformed {
            offenders.push(json!({"receipt": receipt, "kind": "decode_error", "detail": receipt}));
        }
    }
    let canary = json!({
        "status": if offending.is_empty() { "PASS" } else { "FAIL" },
        "offending_kinds": offending,
        "offending_receipts": offenders,
    });
    report
        .as_object_mut()
        .ok_or("coverage report must be an object")?
        .insert("canary".to_owned(), canary);
    Ok(report)
}

#[derive(Serialize)]
struct GithubOutput {
    annotations: Vec<String>,
    summary: String,
}

pub fn github_output(result: &Value) -> Value {
    let mut annotations = Vec::new();
    let mut rows = Vec::new();
    for row in missing_rows(result) {
        let path = field(row, "path");
        let reason = field(row, "reason");
        annotations.push(format!("::warning file={path}::{reason}"));
        let mut rejection_kinds = BTreeSet::new();
        for rejection in rejection_rows(row) {
            let kind = field(rejection, "kind");
            rejection_kinds.insert(kind.to_owned());
            if validator_side(kind) {
                annotations.push(format!(
                    "::error::{path}: {}: {}",
                    field(rejection, "receipt"),
                    field(rejection, "detail")
                ));
            }
        }
        let mut kind = row
            .get("reason_kind")
            .and_then(Value::as_str)
            .unwrap_or("not_pass")
            .to_owned();
        if !rejection_kinds.is_empty() {
            kind.push_str(&format!(
                " ({})",
                rejection_kinds.into_iter().collect::<Vec<_>>().join(", ")
            ));
        }
        let campaigns = row
            .get("campaigns")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .filter(|joined| !joined.is_empty())
            .unwrap_or_else(|| "—".to_owned());
        rows.push(format!("| `{path}` | {campaigns} | {reason} | {kind} |"));
    }
    let summary = if rows.is_empty() {
        String::new()
    } else {
        let mut lines = vec![
            "".to_owned(),
            "### Mutation evidence (changed tests)".to_owned(),
            "".to_owned(),
            "| path | campaign | status | kind |".to_owned(),
            "|---|---|---|---|".to_owned(),
        ];
        lines.extend(rows);
        format!("{}\n", lines.join("\n"))
    };
    serde_json::to_value(GithubOutput {
        annotations,
        summary,
    })
    .expect("serializable output")
}

#[cfg(feature = "python")]
fn value_error(message: impl std::fmt::Display) -> PyErr {
    PyValueError::new_err(message.to_string())
}

#[cfg(any(feature = "python", test))]
fn normalize_mutation_path(value: &str, label: &str) -> Result<String, String> {
    let text = value.replace('\\', "/");
    if text.trim().is_empty() {
        return Err(format!("{label} must be a non-empty string"));
    }
    if text.starts_with('/') || text.starts_with("./") {
        return Err(format!(
            "{label} must be a normalized repository-relative path"
        ));
    }

    let mut parts = Vec::new();
    for part in text.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                return Err(format!(
                    "{label} must be a normalized repository-relative path"
                ));
            }
            _ => parts.push(part),
        }
    }
    if parts.is_empty() {
        Ok(".".to_owned())
    } else {
        Ok(parts.join("/"))
    }
}

#[cfg(feature = "python")]
#[pyfunction]
fn normalize_mutation_path_native(value: &str, label: &str) -> PyResult<String> {
    normalize_mutation_path(value, label).map_err(value_error)
}

fn lexical_absolute(path: &Path) -> Result<PathBuf, String> {
    let source = if path.is_absolute() {
        path.to_path_buf()
    } else {
        env::current_dir()
            .map_err(|error| format!("cannot resolve current directory: {error}"))?
            .join(path)
    };
    let mut normalized = PathBuf::new();
    for component in source.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(Path::new("/")),
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(part) => normalized.push(part),
        }
    }
    Ok(normalized)
}

fn resolved_existing_or_lexical(path: &Path) -> Result<PathBuf, String> {
    match fs::canonicalize(path) {
        Ok(resolved) => Ok(resolved),
        Err(_) => lexical_absolute(path),
    }
}

fn registry_patterns(
    repo_root: &str,
    registry_path: &str,
    canonical_patterns: &[String],
) -> Result<Vec<String>, String> {
    let root = resolved_existing_or_lexical(Path::new(repo_root))?;
    let registry = resolved_existing_or_lexical(Path::new(registry_path))?;
    if registry.strip_prefix(&root).is_err() {
        return Err("mutation registry must be inside the repository".to_owned());
    }
    let text = fs::read_to_string(&registry).map_err(|error| {
        format!(
            "cannot load mutation registry {}: {error}",
            registry.display()
        )
    })?;
    let payload: Value = serde_json::from_str(&text).map_err(|error| {
        format!(
            "cannot load mutation registry {}: {error}",
            registry.display()
        )
    })?;
    let object = payload
        .as_object()
        .ok_or_else(|| "registry must be a JSON object".to_owned())?;
    let raw_patterns = object.get("test_patterns").and_then(Value::as_array);
    let Some(raw_patterns) = raw_patterns else {
        return Err("registry.test_patterns must be a list of non-empty strings".to_owned());
    };
    if raw_patterns.is_empty() {
        return Err("registry.test_patterns must be a list of non-empty strings".to_owned());
    }
    let mut patterns = Vec::with_capacity(raw_patterns.len());
    for raw in raw_patterns {
        let Some(pattern) = raw.as_str() else {
            return Err("registry.test_patterns must be a list of non-empty strings".to_owned());
        };
        if pattern.is_empty() {
            return Err("registry.test_patterns must be a list of non-empty strings".to_owned());
        }
        patterns.push(pattern.to_owned());
    }
    if patterns != canonical_patterns {
        return Err("registry.test_patterns must match the canonical inventory".to_owned());
    }
    Ok(patterns)
}

#[cfg(feature = "python")]
#[pyfunction]
fn mutation_registry_patterns_native(
    repo_root: &str,
    registry_path: &str,
    canonical_patterns: Vec<String>,
) -> PyResult<Vec<String>> {
    registry_patterns(repo_root, registry_path, &canonical_patterns).map_err(value_error)
}

fn wildcard_segment_matches(candidate: &str, pattern: &str) -> bool {
    let candidate: Vec<char> = candidate.chars().collect();
    let pattern: Vec<char> = pattern.chars().collect();
    let mut row = vec![false; candidate.len() + 1];
    row[0] = true;
    for token in pattern {
        let mut next = vec![false; candidate.len() + 1];
        match token {
            '*' => {
                next[0] = row[0];
                for index in 1..=candidate.len() {
                    next[index] = row[index] || next[index - 1];
                }
            }
            '?' => {
                next[1..].copy_from_slice(&row[..candidate.len()]);
            }
            literal => {
                for index in 1..=candidate.len() {
                    next[index] = row[index - 1] && candidate[index - 1] == literal;
                }
            }
        }
        row = next;
    }
    row[candidate.len()]
}

fn glob_segments_match(path: &[&str], pattern: &[&str]) -> bool {
    if pattern.is_empty() {
        return path.is_empty();
    }
    if pattern[0] == "**" {
        // pathlib's leading ``**/`` requires at least one directory component.
        return (1..=path.len()).any(|count| glob_segments_match(&path[count..], &pattern[1..]));
    }
    if path.is_empty() || !wildcard_segment_matches(path[0], pattern[0]) {
        return false;
    }
    glob_segments_match(&path[1..], &pattern[1..])
}

fn is_mutation_test_path(path: &str, patterns: &[String]) -> bool {
    let normalized = path.replace('\\', "/");
    let path_parts: Vec<&str> = normalized
        .split('/')
        .filter(|part| !part.is_empty() && *part != ".")
        .collect();
    patterns.iter().any(|raw_pattern| {
        let normalized_pattern = raw_pattern.replace('\\', "/");
        let pattern_parts: Vec<&str> = normalized_pattern
            .split('/')
            .filter(|part| !part.is_empty() && *part != ".")
            .collect();
        if pattern_parts.is_empty() {
            return false;
        }
        if pattern_parts.len() == 1 {
            return path_parts
                .last()
                .is_some_and(|name| wildcard_segment_matches(name, pattern_parts[0]));
        }
        if pattern_parts.first() == Some(&"**") {
            return glob_segments_match(&path_parts, &pattern_parts);
        }
        if pattern_parts.len() > path_parts.len() {
            return false;
        }
        glob_segments_match(
            &path_parts[path_parts.len() - pattern_parts.len()..],
            &pattern_parts,
        )
    })
}

#[cfg(feature = "python")]
#[pyfunction]
fn is_mutation_test_path_native(path: &str, patterns: Vec<String>) -> bool {
    is_mutation_test_path(path, &patterns)
}

/// Read the attribute path out of a `#[...]` line: `#[tokio::test]` is
/// `tokio::test`, `#[cfg(test)]` is `cfg`.
fn rust_attribute_path(trimmed: &str) -> Option<&str> {
    let rest = trimmed.strip_prefix("#[")?;
    let end = rest.find(['(', ']'])?;
    Some(rest[..end].trim())
}

/// Does this Rust source declare tests?
///
/// Rust puts unit tests in the module they test, so no filename glob can find
/// them: `**/test_*.rs` matches nothing real, and an inventory built from
/// globs alone reports zero Rust test files while the repository has dozens.
/// Deciding it needs the file's contents, not its name.
///
/// The rule is the attribute path, the same one `mutation_manifest`'s nodeid
/// reader applies: an attribute whose final segment is `test` declares a test,
/// so `#[test]` and `#[tokio::test]` both count, while `#[cfg(test)]` and
/// `#[cfg_attr(test, ..)]` gate code rather than declare one and do not.
/// Requiring `#[` at the START of the trimmed line is also what excludes a
/// commented-out test: `// #[test]` does not begin an attribute. A separate
/// comment guard reads as protection but is unreachable, and a mutation run
/// proved it -- deleting it changed no outcome.
fn declares_rust_tests(source: &str) -> bool {
    source.lines().any(|line| {
        rust_attribute_path(line.trim())
            .is_some_and(|path| path.rsplit("::").next().unwrap_or(path) == "test")
    })
}

/// A `.rs` file is a test surface when it declares tests. An unreadable file
/// is not one: the inventory reports what it can see, and a path git lists
/// that the filesystem cannot open is a different failure.
fn is_rust_test_surface(repo_root: &Path, relative: &str) -> bool {
    relative.ends_with(".rs")
        && fs::read_to_string(repo_root.join(relative))
            .is_ok_and(|source| declares_rust_tests(&source))
}

/// Is this path a mutation test surface?
///
/// Two questions, because neither language answers for the other: the registry's
/// globs decide it for every named test file, and the file's contents decide it
/// for Rust. Asking only the first is what left the inventory blind to a tree of
/// unit tests; asking only the second would drop every Python and JavaScript
/// test on the floor.
fn is_inventory_surface(repo_root: &Path, normalized: &str, patterns: &[String]) -> bool {
    is_mutation_test_path(normalized, patterns) || is_rust_test_surface(repo_root, normalized)
}

#[cfg(feature = "python")]
#[pyfunction]
fn mutation_rust_test_surface_native(repo_root: &str, path: &str) -> bool {
    is_rust_test_surface(Path::new(repo_root), path)
}

fn git_paths(repo_root: &str, args: &[String]) -> Result<Vec<String>, String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo_root)
        .output()
        .map_err(|error| format!("git {} failed: {error}", args.join(" ")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        let detail = if stderr.is_empty() { stdout } else { stderr };
        return Err(format!("git {} failed: {detail}", args.join(" ")));
    }
    let stdout = String::from_utf8(output.stdout)
        .map_err(|error| format!("git {} produced non-UTF-8 output: {error}", args.join(" ")))?;
    Ok(stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| line.replace('\\', "/"))
        .collect())
}

#[cfg(feature = "python")]
#[pyfunction]
fn mutation_git_paths_native(repo_root: &str, args: Vec<String>) -> PyResult<Vec<String>> {
    git_paths(repo_root, &args).map_err(value_error)
}

fn should_skip_mutation_path(path: &str, skip_directory_names: &HashSet<&str>) -> bool {
    let normalized = path.replace('\\', "/");
    let parts: Vec<&str> = normalized
        .split('/')
        .filter(|part| !part.is_empty() && *part != ".")
        .collect();
    parts.iter().any(|part| skip_directory_names.contains(part))
        || (parts.contains(&"research") && parts.contains(&"cache"))
}

#[cfg(feature = "python")]
#[pyfunction]
fn should_skip_mutation_path_native(path: &str, skip_directory_names: Vec<String>) -> bool {
    let skip: HashSet<&str> = skip_directory_names.iter().map(String::as_str).collect();
    should_skip_mutation_path(path, &skip)
}

/// The inventory's git side: raw paths by mode, with plain-String errors so
/// the unit tests can assert on them without an initialized interpreter (a
/// PyErr only exists once Python is up; building one in a bare test process
/// panics, which made the error-path test order-dependent on whichever
/// earlier test happened to initialize Python first).
fn inventory_git_paths(
    repo_root: &str,
    mode: &str,
    include_untracked: bool,
    base: Option<&str>,
) -> Result<Vec<String>, String> {
    let mut raw_paths = match mode {
        "all" => git_paths(repo_root, &["ls-files".to_owned()])?,
        "changed" => git_paths(
            repo_root,
            &[
                "diff".to_owned(),
                "--name-only".to_owned(),
                "HEAD".to_owned(),
            ],
        )?,
        // Diff against a named base ref with merge-base semantics, the shape CI
        // needs: a clean checkout has no working-tree diff at all, so "changed"
        // (git diff HEAD) would inventory nothing on a runner. The base must be
        // named and must resolve -- git's own failure names the ref -- and
        // untracked files stay out unless the caller asked for them.
        "changed-from" => {
            let base = base.ok_or_else(|| {
                "changed-from inventory mode requires a base ref (--base)".to_owned()
            })?;
            git_paths(
                repo_root,
                &[
                    "diff".to_owned(),
                    "--name-only".to_owned(),
                    "--diff-filter=ACMR".to_owned(),
                    format!("{base}...HEAD"),
                ],
            )?
        }
        _ => {
            return Err(format!(
                "mutation inventory mode must be 'all', 'changed' or 'changed-from', got {mode:?}"
            ));
        }
    };
    if include_untracked || mode == "changed" {
        raw_paths.extend(git_paths(
            repo_root,
            &[
                "ls-files".to_owned(),
                "--others".to_owned(),
                "--exclude-standard".to_owned(),
            ],
        )?);
    }
    Ok(raw_paths)
}

pub fn test_inventory(
    repo_root: &str,
    registry_path: &str,
    canonical_patterns: &[String],
    skip_directory_names: &[String],
    mode: &str,
    include_untracked: bool,
    base: Option<&str>,
) -> Result<Vec<String>, String> {
    let patterns = registry_patterns(repo_root, registry_path, canonical_patterns)?;
    let raw_paths = inventory_git_paths(repo_root, mode, include_untracked, base)?;

    let skip: HashSet<&str> = skip_directory_names.iter().map(String::as_str).collect();
    let mut seen = HashSet::with_capacity(raw_paths.len());
    let mut selected = Vec::new();
    for raw in raw_paths {
        let path = raw.replace('\\', "/");
        let normalized = path
            .split('/')
            .filter(|part| !part.is_empty() && *part != ".")
            .collect::<Vec<_>>()
            .join("/");
        if !seen.insert(normalized.clone())
            || should_skip_mutation_path(&normalized, &skip)
            || !is_inventory_surface(Path::new(repo_root), &normalized, &patterns)
        {
            continue;
        }
        selected.push(normalized);
    }
    selected.sort_unstable();
    Ok(selected)
}

#[cfg(feature = "python")]
#[pyfunction]
#[pyo3(signature = (repo_root, registry_path, canonical_patterns, skip_directory_names, mode, include_untracked, base=None))]
fn mutation_test_inventory_native(
    repo_root: &str,
    registry_path: &str,
    canonical_patterns: Vec<String>,
    skip_directory_names: Vec<String>,
    mode: &str,
    include_untracked: bool,
    base: Option<String>,
) -> PyResult<Vec<String>> {
    test_inventory(
        repo_root,
        registry_path,
        &canonical_patterns,
        &skip_directory_names,
        mode,
        include_untracked,
        base.as_deref(),
    )
    .map_err(value_error)
}

#[cfg(feature = "python")]
#[pyfunction]
fn mutation_evidence_exit_code_native(result_json: &str) -> PyResult<i32> {
    let result = serde_json::from_str(result_json).map_err(value_error)?;
    Ok(evidence_exit_code(&result))
}

#[cfg(feature = "python")]
#[pyfunction]
fn mutation_canary_verdict_native(report_json: &str) -> PyResult<String> {
    let report = serde_json::from_str(report_json).map_err(value_error)?;
    let verdict = canary_verdict(report).map_err(value_error)?;
    serde_json::to_string(&verdict).map_err(value_error)
}

#[cfg(feature = "python")]
#[pyfunction]
fn mutation_github_output_native(result_json: &str) -> PyResult<String> {
    let result = serde_json::from_str(result_json).map_err(value_error)?;
    serde_json::to_string(&github_output(&result)).map_err(value_error)
}

#[cfg(feature = "python")]
pub fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(normalize_mutation_path_native, module)?)?;
    module.add_function(wrap_pyfunction!(mutation_registry_patterns_native, module)?)?;
    module.add_function(wrap_pyfunction!(is_mutation_test_path_native, module)?)?;
    module.add_function(wrap_pyfunction!(mutation_git_paths_native, module)?)?;
    module.add_function(wrap_pyfunction!(should_skip_mutation_path_native, module)?)?;
    module.add_function(wrap_pyfunction!(mutation_rust_test_surface_native, module)?)?;
    module.add_function(wrap_pyfunction!(mutation_test_inventory_native, module)?)?;
    module.add_function(wrap_pyfunction!(
        mutation_evidence_exit_code_native,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(mutation_canary_verdict_native, module)?)?;
    module.add_function(wrap_pyfunction!(mutation_github_output_native, module)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        canary_verdict, declares_rust_tests, evidence_exit_code, git_paths, github_output,
        is_inventory_surface, is_mutation_test_path, is_rust_test_surface, normalize_mutation_path,
        observed_evidence_kinds, registry_patterns, rust_attribute_path, should_skip_mutation_path,
        test_inventory,
    };
    use serde_json::{json, Value};
    use std::collections::HashSet;

    fn missing(path: &str, kind: &str, rejections: Value) -> Value {
        json!({
            "path": path,
            "reason": "no current complete PASS receipt",
            "reason_kind": kind,
            "campaigns": ["c1", "c2"],
            "receipt_rejections": rejections,
        })
    }

    #[test]
    fn evidence_exit_codes_separate_debt_from_validator_defects() {
        let debt = json!({
            "missing_evidence": [missing("test_new.py", "no_campaign", json!([])),
                missing("test_old.py", "not_pass", json!([
                    {"receipt": "r1.json", "kind": "superseded", "detail": "old"},
                    {"receipt": "r0.json", "kind": "runner_map_mismatch", "detail": "old"}
                ]))],
            "rejection_counts": {"no_campaign": 1, "not_pass": 1, "superseded": 1,
                "runner_map_mismatch": 1},
        });
        assert_eq!(evidence_exit_code(&debt), 6);
        let defect = json!({
            "missing_evidence": [missing("test_new.py", "not_pass", json!([
                {"receipt": "r1.json", "kind": "decode_error", "detail": "bad base64"}
            ]))],
            "rejection_counts": {"not_pass": 1, "decode_error": 1},
        });
        assert_eq!(evidence_exit_code(&defect), 5);
        let covered = json!({"missing_evidence": [], "rejection_counts": {"superseded": 2}});
        assert_eq!(evidence_exit_code(&covered), 0);
        let corrupt_sibling =
            json!({"missing_evidence": [], "rejection_counts": {"schema_error": 1}});
        assert_eq!(evidence_exit_code(&corrupt_sibling), 5);
    }

    #[test]
    fn row_kinds_and_legacy_defaults_are_fail_closed() {
        let row_only = json!({"missing_evidence": [missing("test_decode.py", "decode_error", json!([]))],
            "rejection_counts": {}});
        assert_eq!(evidence_exit_code(&row_only), 5);
        let mut legacy = json!({"missing_evidence": [{"path": "test_legacy.py", "receipt_rejections": []}],
            "rejection_counts": {}});
        assert_eq!(
            observed_evidence_kinds(&legacy)
                .into_iter()
                .collect::<Vec<_>>(),
            ["not_pass"]
        );
        assert_eq!(evidence_exit_code(&legacy), 6);
        legacy["missing_evidence"][0]["receipt_rejections"] =
            json!([{"receipt": "r9.json", "detail": "bad shape"}]);
        assert_eq!(evidence_exit_code(&legacy), 5);
    }

    #[test]
    fn github_output_keeps_annotations_order_and_legacy_table_defaults() {
        let result = json!({"missing_evidence": [
            missing("test_new.py", "not_pass", json!([
                {"receipt": "r1.json", "kind": "decode_error", "detail": "bad base64"},
                {"receipt": "r0.json", "kind": "superseded", "detail": "old"}
            ])),
            {"path": "test_legacy.py", "reason": "no campaign", "campaigns": [], "receipt_rejections": []},
            {"path": "test_plain.py", "reason": "no campaign", "reason_kind": "no_campaign",
                "campaigns": [], "receipt_rejections": []}
        ]});
        let output = github_output(&result);
        assert_eq!(
            output["annotations"],
            json!([
                "::warning file=test_new.py::no current complete PASS receipt",
                "::error::test_new.py: r1.json: bad base64",
                "::warning file=test_legacy.py::no campaign",
                "::warning file=test_plain.py::no campaign",
            ])
        );
        let summary = output["summary"].as_str().unwrap();
        assert!(summary.contains("| path | campaign | status | kind |"));
        assert!(summary.contains("| `test_new.py` | c1, c2 | no current complete PASS receipt | not_pass (decode_error, superseded) |"));
        assert!(summary.contains("| `test_legacy.py` | — | no campaign | not_pass |"));
        assert!(summary.contains("| `test_plain.py` | — | no campaign | no_campaign |"));
        assert_eq!(
            github_output(&json!({"missing_evidence": []}))["summary"],
            ""
        );
    }

    #[test]
    fn canary_names_only_validator_offenders_and_keeps_debt() {
        let clean = json!({
            "status": "FAIL", "missing_evidence": [missing("test_debt.py", "no_campaign", json!([]))],
            "rejection_counts": {"no_campaign": 9, "not_pass": 3, "superseded": 5},
            "malformed_receipts": [],
        });
        let clean_verdict = canary_verdict(clean).unwrap();
        assert_eq!(
            clean_verdict["canary"],
            json!({"status": "PASS", "offending_kinds": [],
            "offending_receipts": []})
        );
        let unreadable = json!({
            "status": "FAIL", "missing_evidence": [missing("test_broken.py", "not_pass", json!([
                {"receipt": "r1.json", "kind": "decode_error", "detail": "frame error"},
                {"receipt": "r0.json", "kind": "superseded", "detail": "old"}
            ]))],
            "rejection_counts": {"no_campaign": 9, "decode_error": 1},
            "malformed_receipts": ["receipts/gone.json: invalid JSON"],
        });
        let verdict = canary_verdict(unreadable).unwrap();
        assert_eq!(
            verdict["canary"],
            json!({
                "status": "FAIL", "offending_kinds": ["decode_error"],
                "offending_receipts": [
                    {"path": "test_broken.py", "receipt": "r1.json", "kind": "decode_error", "detail": "frame error"},
                    {"receipt": "receipts/gone.json: invalid JSON", "kind": "decode_error",
                        "detail": "receipts/gone.json: invalid JSON"}
                ]
            })
        );
    }

    #[test]
    fn path_globs_normalization_and_skip_sets_preserve_inventory_rules() {
        let patterns = vec!["**/test_*.py".to_owned(), "**/*.spec.js".to_owned()];
        assert!(is_mutation_test_path(
            "research/tests/test_foo.py",
            &patterns
        ));
        assert!(is_mutation_test_path(
            "aria_designer/e2e/designer.spec.js",
            &patterns
        ));
        assert!(!is_mutation_test_path("research/tools/foo.py", &patterns));
        assert_eq!(
            normalize_mutation_path("src\\tests//test_x.py", "path").unwrap(),
            "src/tests/test_x.py"
        );
        for unsafe_path in ["../escape.py", "/abs.py", "./test_x.py"] {
            assert!(normalize_mutation_path(unsafe_path, "path")
                .unwrap_err()
                .contains("normalized"));
        }
        assert!(normalize_mutation_path("   ", "path")
            .unwrap_err()
            .contains("non-empty"));
        let skip = HashSet::from([".venv", "__pycache__"]);
        assert!(should_skip_mutation_path(
            "research/cache/foo/test_x.py",
            &skip
        ));
        assert!(should_skip_mutation_path(".venv/lib/test_x.py", &skip));
        assert!(!should_skip_mutation_path(
            "research/tests/test_x.py",
            &skip
        ));
    }

    #[test]
    fn registry_rejects_outside_malformed_and_noncanonical_patterns() {
        let repo = std::env::temp_dir().join(format!(
            "conductor-native-registry-validation-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&repo);
        std::fs::create_dir_all(&repo).unwrap();
        let registry = repo.join("registry.json");
        let outside = repo.with_extension("outside.json");
        std::fs::write(&outside, "{}").unwrap();
        let root = repo.to_str().unwrap();
        let canonical = vec!["**/test_*.py".to_owned()];
        assert!(
            registry_patterns(root, outside.to_str().unwrap(), &canonical)
                .unwrap_err()
                .contains("inside the repository")
        );
        for (contents, fragment) in [
            ("not json", "cannot load"),
            ("[]", "JSON object"),
            (r#"{"test_patterns": []}"#, "test_patterns"),
            (
                r#"{"test_patterns": ["never-a-test"]}"#,
                "canonical inventory",
            ),
        ] {
            std::fs::write(&registry, contents).unwrap();
            assert!(
                registry_patterns(root, registry.to_str().unwrap(), &canonical)
                    .unwrap_err()
                    .contains(fragment)
            );
        }
        std::fs::remove_file(outside).unwrap();
        std::fs::remove_dir_all(repo).unwrap();
    }

    #[test]
    fn git_errors_and_inventory_include_content_declared_rust_tests() {
        let repo = std::env::temp_dir().join(format!(
            "conductor-native-inventory-git-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&repo);
        std::fs::create_dir_all(repo.join("crate/src")).unwrap();
        let root = repo.to_str().unwrap();
        assert!(git_paths(root, &["status".to_owned()])
            .unwrap_err()
            .contains("git"));
        git_paths(root, &["init".to_owned(), "--quiet".to_owned()]).unwrap();
        let registry = repo.join("registry.json");
        std::fs::write(&registry, r#"{"test_patterns": ["**/test_*.py"]}"#).unwrap();
        std::fs::write(
            repo.join("crate/src/lib.rs"),
            "#[cfg(test)]\nmod tests {\n#[test]\nfn one() {}\n}\n",
        )
        .unwrap();
        std::fs::write(repo.join("crate/src/plumbing.rs"), "pub fn two() {}\n").unwrap();
        let inventory = test_inventory(
            root,
            registry.to_str().unwrap(),
            &["**/test_*.py".to_owned()],
            &[],
            "all",
            true,
            None,
        )
        .unwrap();
        assert_eq!(inventory, vec!["crate/src/lib.rs".to_owned()]);
        std::fs::remove_dir_all(repo).unwrap();
    }

    #[test]
    fn attribute_path_stops_at_the_first_delimiter() {
        assert_eq!(rust_attribute_path("#[test]"), Some("test"));
        assert_eq!(rust_attribute_path("#[tokio::test]"), Some("tokio::test"));
        assert_eq!(rust_attribute_path("#[cfg(test)]"), Some("cfg"));
        assert_eq!(rust_attribute_path("#[ cfg_attr (test)]"), Some("cfg_attr"));
        assert_eq!(rust_attribute_path("fn test() {}"), None);
        assert_eq!(rust_attribute_path("#[unterminated"), None);
    }

    #[test]
    fn a_test_attribute_marks_the_file() {
        assert!(declares_rust_tests("#[test]\nfn one() {}\n"));
        assert!(declares_rust_tests(
            "    #[tokio::test]\n    async fn two() {}\n"
        ));
    }

    #[test]
    fn gating_attributes_are_not_test_declarations() {
        // The whole point of the attribute-path rule: `#[cfg(test)]` marks the
        // module a test lives in, and a file can carry one with every test
        // since removed.
        assert!(!declares_rust_tests("#[cfg(test)]\nmod tests {}\n"));
        assert!(!declares_rust_tests(
            "#[cfg_attr(test, derive(Debug))]\nstruct S;\n"
        ));
    }

    #[test]
    fn a_commented_out_test_does_not_count() {
        assert!(!declares_rust_tests("// #[test]\n// fn gone() {}\n"));
        assert!(!declares_rust_tests(
            "/// #[tokio::test]\n/// async fn gone() {}\n"
        ));
        // The attribute has to OPEN the line; trailing prose after a real one
        // is still a declaration.
        assert!(declares_rust_tests(
            "#[test] // still a test\nfn one() {}\n"
        ));
    }

    #[test]
    fn plain_rust_source_declares_nothing() {
        assert!(!declares_rust_tests("pub fn one() -> u8 {\n    1\n}\n"));
        // Ordinary Rust is full of attributes; carrying one is not declaring a
        // test, or every crate in the tree would be a test surface.
        assert!(!declares_rust_tests(
            "#[derive(Debug)]\npub struct S;\n\n#[inline]\npub fn two() -> u8 {\n    2\n}\n"
        ));
        assert!(!declares_rust_tests(""));
    }

    #[test]
    fn only_rust_paths_are_rust_surfaces() {
        let dir = std::env::temp_dir().join(format!(
            "conductor-native-rust-surface-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        std::fs::write(dir.join("with.rs"), "#[test]\nfn one() {}\n").expect("write");
        std::fs::write(dir.join("without.rs"), "pub fn one() {}\n").expect("write");
        std::fs::write(dir.join("with.py"), "#[test]\n").expect("write");

        assert!(is_rust_test_surface(&dir, "with.rs"));
        assert!(!is_rust_test_surface(&dir, "without.rs"));
        // A `.py` file carrying the same bytes is not a Rust surface, and an
        // absent path is not one either.
        assert!(!is_rust_test_surface(&dir, "with.py"));
        assert!(!is_rust_test_surface(&dir, "absent.rs"));

        std::fs::remove_dir_all(&dir).expect("cleanup");
    }

    #[test]
    fn a_surface_is_admitted_by_either_name_or_contents() {
        let dir = std::env::temp_dir().join(format!(
            "conductor-native-inventory-surface-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        std::fs::write(dir.join("lib.rs"), "#[test]\nfn one() {}\n").expect("write");
        std::fs::write(dir.join("plumbing.rs"), "pub fn one() {}\n").expect("write");
        let patterns = vec!["test_*.py".to_owned()];

        // Named like a test but absent from disk: the glob still admits it.
        assert!(is_inventory_surface(&dir, "test_thing.py", &patterns));
        // No glob matches a Rust unit test; only its contents do.
        assert!(is_inventory_surface(&dir, "lib.rs", &patterns));
        assert!(!is_inventory_surface(&dir, "plumbing.rs", &patterns));

        std::fs::remove_dir_all(&dir).expect("cleanup");
    }

    mod changed_from {
        use super::super::{git_paths, inventory_git_paths};

        fn run_git(dir: &std::path::Path, args: &[&str]) -> Vec<String> {
            let owned: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
            git_paths(&dir.display().to_string(), &owned).expect("git command")
        }

        fn inventory(
            dir: &std::path::Path,
            mode: &str,
            include_untracked: bool,
            base: Option<&str>,
        ) -> Result<Vec<String>, String> {
            // The pure core, not the pyfunction: its errors are plain Strings,
            // assertable without an initialized interpreter.
            inventory_git_paths(&dir.display().to_string(), mode, include_untracked, base)
        }

        /// Two commits: a file only the base has and a file only the branch
        /// has, plus an untracked file. `changed-from` must return exactly the
        /// branch's file -- the base-only file is not in the merge-base diff,
        /// and untracked files stay out unless asked for.
        #[test]
        fn only_the_branch_side_of_the_merge_base_diff_is_returned() {
            let dir = std::env::temp_dir().join(format!(
                "conductor-native-changed-from-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("temp dir");
            run_git(&dir, &["init", "--quiet"]);
            std::fs::write(
                dir.join("registry.json"),
                r#"{"test_patterns": ["test_*.py"]}"#,
            )
            .expect("write registry");
            std::fs::write(dir.join("test_base_only.py"), "def test_base(): pass\n")
                .expect("write base file");
            run_git(
                &dir,
                &["-c", "user.email=t@t", "-c", "user.name=t", "add", "-A"],
            );
            run_git(
                &dir,
                &[
                    "-c",
                    "user.email=t@t",
                    "-c",
                    "user.name=t",
                    "commit",
                    "--quiet",
                    "-m",
                    "base",
                ],
            );
            let base = run_git(&dir, &["rev-parse", "HEAD"])
                .pop()
                .expect("base sha");
            run_git(&dir, &["checkout", "--quiet", "-b", "branch"]);
            std::fs::write(dir.join("test_branch_only.py"), "def test_branch(): pass\n")
                .expect("write branch file");
            run_git(
                &dir,
                &["-c", "user.email=t@t", "-c", "user.name=t", "add", "-A"],
            );
            run_git(
                &dir,
                &[
                    "-c",
                    "user.email=t@t",
                    "-c",
                    "user.name=t",
                    "commit",
                    "--quiet",
                    "-m",
                    "branch",
                ],
            );
            std::fs::write(dir.join("test_untracked.py"), "def test_new(): pass\n")
                .expect("write untracked file");

            let changed_from = inventory(&dir, "changed-from", false, Some(&base))
                .expect("changed-from inventory");
            assert_eq!(changed_from, vec!["test_branch_only.py".to_owned()]);

            let with_untracked = inventory(&dir, "changed-from", true, Some(&base))
                .expect("changed-from with untracked");
            assert_eq!(
                with_untracked,
                vec![
                    "test_branch_only.py".to_owned(),
                    "test_untracked.py".to_owned(),
                ]
            );

            // The other modes keep their shape: positional callers that pass no
            // base still work, and "changed" still sees the working tree.
            let changed = inventory(&dir, "changed", true, None).expect("changed inventory");
            assert!(changed.contains(&"test_untracked.py".to_owned()));

            std::fs::remove_dir_all(&dir).expect("cleanup");
        }

        #[test]
        fn a_missing_or_unresolvable_base_errors_loud() {
            let dir = std::env::temp_dir().join(format!(
                "conductor-native-changed-from-err-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("temp dir");
            run_git(&dir, &["init", "--quiet"]);
            std::fs::write(
                dir.join("registry.json"),
                r#"{"test_patterns": ["test_*.py"]}"#,
            )
            .expect("write registry");

            let missing = inventory(&dir, "changed-from", false, None)
                .expect_err("changed-from without a base must fail");
            assert!(missing.contains("requires a base ref"));
            let unresolved = inventory(&dir, "changed-from", false, Some("no-such-ref"))
                .expect_err("changed-from with an unknown ref must fail");
            assert!(unresolved.contains("no-such-ref"));

            std::fs::remove_dir_all(&dir).expect("cleanup");
        }
    }
}
