//! Mutation campaign manifest parsing and deterministic validation.

use std::collections::{BTreeSet, HashMap};
use std::env;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use ruff_python_ast as ast;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct RankedTestContract {
    pub(crate) rank: i64,
    pub(crate) nodeid: String,
    pub(crate) contract: String,
    pub(crate) rationale: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct PlannedMutationContract {
    pub(crate) id: String,
    pub(crate) target_path: String,
    pub(crate) contract: String,
    pub(crate) description: String,
    pub(crate) expected_killers: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct MutationContract {
    pub(crate) id: String,
    pub(crate) patch_file: String,
    pub(crate) patch_sha256: String,
    pub(crate) allowed_paths: Vec<String>,
    pub(crate) expected_killers: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct ValueContract {
    pub(crate) expected_nodeids: Vec<String>,
    pub(crate) expected_repetitions: u64,
}

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct CampaignContract {
    pub(crate) campaign_id: String,
    pub(crate) title: String,
    pub(crate) language: String,
    pub(crate) mutation_engine: String,
    pub(crate) expected_mutations: usize,
    pub(crate) manifest: String,
    pub(crate) manifest_sha256: String,
    pub(crate) source_sha256: Value,
    pub(crate) source_symbols: Value,
    pub(crate) test_scopes: Map<String, Value>,
    pub(crate) ranked_tests: Vec<RankedTestContract>,
    pub(crate) ranked_test_paths: Vec<String>,
    pub(crate) planned_mutations: Vec<PlannedMutationContract>,
    pub(crate) mutations: Vec<MutationContract>,
    pub(crate) test_argv: Vec<String>,
    pub(crate) timeout_seconds: u64,
    pub(crate) blocked_process_substrings: Vec<String>,
    pub(crate) poll_seconds: u64,
    pub(crate) environment: Map<String, Value>,
    pub(crate) host_read_dependencies: Vec<String>,
    pub(crate) value_analysis_payload: Option<Value>,
    pub(crate) value_analysis: Option<ValueContract>,
    pub(crate) source_drifted: bool,
    /// True when the mutants come from an engine rather than from a committed patch list.
    ///
    /// The two kinds of campaign prove different things and cannot share an acceptance rule.
    /// A patch campaign names its mutants in the manifest, so the gate can demand that every
    /// one of them died and that the score is exactly 1.0. A generated campaign takes whatever
    /// the engine finds in the source tree, and 1.0 is not a reachable target there -- real
    /// suites leave equivalent mutants that no test can kill. Demanding it anyway would mean
    /// no generated campaign could ever land, which is how a corpus nobody chose gets locked
    /// out in favour of one somebody did.
    #[serde(default)]
    pub(crate) generated: bool,
    /// The survivor ids this campaign has already accepted, for a generated campaign.
    ///
    /// This is the ratchet. A run passes when it introduces no survivor outside this set,
    /// which is a stricter question than a percentage: closing one gap while opening another
    /// leaves the score untouched and fails here.
    #[serde(default)]
    pub(crate) survivor_baseline: Vec<String>,
    /// Digests of the test files a generated campaign ran, keyed by repository-relative path.
    ///
    /// A patch campaign lists its tests in `source_sha256` because it mutates them as source.
    /// A generated campaign mutates only the subject, so the tests need their own map -- without
    /// it the receipt would still be accepted after the test file was edited, which is evidence
    /// about a test that no longer exists.
    #[serde(default)]
    pub(crate) test_sha256: Value,
}

/// Engines that take their mutants from the source tree instead of from a committed patch list.
pub(crate) const GENERATED_ENGINES: [&str; 3] = ["cargo-mutants", "fest", "mull"];

impl CampaignContract {
    /// Whether this campaign's evidence can speak for `test_path`.
    ///
    /// The two schemas record the tests they ran in different places, so asking the contract
    /// keeps the caller from having to know which kind of campaign it is holding.
    pub(crate) fn covers_test(&self, test_path: &str) -> bool {
        if self.generated {
            return self
                .test_sha256
                .as_object()
                .is_some_and(|tests| tests.contains_key(test_path));
        }
        self.source_sha256
            .as_object()
            .is_some_and(|sources| sources.contains_key(test_path))
            && self.ranked_test_paths.iter().any(|path| path == test_path)
    }
}

#[derive(Debug, Deserialize)]
struct CampaignLoadRequest {
    repo_root: String,
    manifest_path: String,
    #[serde(default)]
    python_test_nodeids: HashMap<String, Vec<String>>,
    #[serde(default)]
    symbol_hashes: HashMap<String, HashMap<String, String>>,
}

#[derive(Debug, Deserialize)]
struct RegistryLoadRequest {
    repo_root: String,
    registry_path: String,
    canonical_test_patterns: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct SourceDriftRequest {
    repo_root: String,
    source_sha256: Map<String, Value>,
    source_symbols: Map<String, Value>,
    symbol_hashes: HashMap<String, HashMap<String, String>>,
}

type RepinManifest = (
    String,
    HashMap<String, String>,
    HashMap<String, HashMap<String, String>>,
);

#[derive(Debug, Deserialize)]
struct InspectRequest {
    campaign: CampaignContract,
    source_drift: Vec<Value>,
    manifest_hash_drift: bool,
    blocking_processes: Vec<Value>,
    value_analysis: Value,
}

pub(crate) fn lexical_absolute(path: &Path) -> Result<PathBuf, String> {
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

pub(crate) fn safe_relative(value: &str, label: &str) -> Result<String, String> {
    if value.trim().is_empty() {
        return Err(format!("{label} must be a non-empty string"));
    }
    let text = value.replace('\\', "/");
    if text.starts_with('/') || text.starts_with("./") {
        return Err(format!(
            "{label} must be a normalized repository-relative path"
        ));
    }
    let mut parts = Vec::new();
    for part in text.split('/') {
        match part {
            ".." => {
                return Err(format!(
                    "{label} must be a normalized repository-relative path"
                ));
            }
            "" | "." => {}
            _ => parts.push(part),
        }
    }
    if parts.is_empty() {
        Ok(".".to_owned())
    } else {
        Ok(parts.join("/"))
    }
}

pub(crate) fn python_repr(value: Option<&Value>) -> String {
    match value.unwrap_or(&Value::Null) {
        Value::Null => "None".to_owned(),
        Value::Bool(true) => "True".to_owned(),
        Value::Bool(false) => "False".to_owned(),
        Value::Number(number) => number.to_string(),
        Value::String(text) => {
            let escaped = text
                .replace('\\', "\\\\")
                .replace('\'', "\\'")
                .replace('\n', "\\n")
                .replace('\r', "\\r")
                .replace('\t', "\\t");
            format!("'{escaped}'")
        }
        other => other.to_string(),
    }
}

pub(crate) fn python_str_or_empty(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) | Some(Value::Bool(false)) => String::new(),
        Some(Value::String(text)) => text.clone(),
        Some(Value::Bool(true)) => "True".to_owned(),
        Some(Value::Number(number)) => number.to_string(),
        Some(other) => other.to_string(),
    }
}

pub(crate) fn read_json_object(
    path: &Path,
    label: &str,
) -> Result<(Vec<u8>, Map<String, Value>), String> {
    let bytes = fs::read(path)
        .map_err(|error| format!("cannot load {label} {}: {error}", path.display()))?;
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("cannot load {label} {}: {error}", path.display()))?;
    let Value::Object(payload) = value else {
        return Err(format!("{label} must be a JSON object"));
    };
    Ok((bytes, payload))
}

fn required_string(payload: &Map<String, Value>, key: &str, label: &str) -> Result<String, String> {
    payload
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
        .ok_or_else(|| format!("{label} must be a non-empty string"))
}

pub(crate) fn string_list(value: Option<&Value>, label: &str) -> Result<Vec<String>, String> {
    let Some(rows) = value.and_then(Value::as_array) else {
        return Err(format!("{label} must be a list of non-empty strings"));
    };
    let mut result = Vec::with_capacity(rows.len());
    for row in rows {
        let Some(text) = row.as_str().filter(|text| !text.is_empty()) else {
            return Err(format!("{label} must be a list of non-empty strings"));
        };
        result.push(text.to_owned());
    }
    Ok(result)
}

fn object<'a>(value: Option<&'a Value>, label: &str) -> Result<&'a Map<String, Value>, String> {
    value
        .and_then(Value::as_object)
        .ok_or_else(|| format!("{label} must be a JSON object"))
}

fn sha256_file(path: &Path) -> Option<String> {
    fs::read(path)
        .ok()
        .map(|bytes| format!("{:x}", Sha256::digest(bytes)))
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn patch_paths(path: &Path) -> Result<Vec<String>, String> {
    let text = fs::read_to_string(path)
        .map_err(|error| format!("cannot read mutation patch {}: {error}", path.display()))?;
    let mut paths = Vec::new();
    let mut diff_paths = Vec::new();
    let mut old_paths = Vec::new();
    for line in text.lines() {
        if line.starts_with("rename from ")
            || line.starts_with("rename to ")
            || line.starts_with("copy from ")
            || line.starts_with("copy to ")
        {
            return Err("mutation patches may not rename or copy files".to_owned());
        }
        if line.starts_with("GIT binary patch") || line.starts_with("Binary files ") {
            return Err("mutation patches must be textual unified diffs".to_owned());
        }
        if let Some(rest) = line.strip_prefix("diff --git ") {
            let fields: Vec<&str> = rest.split_whitespace().collect();
            if fields.len() != 2 || !fields[0].starts_with("a/") || !fields[1].starts_with("b/") {
                return Err(format!("unsupported mutation diff header: {line:?}"));
            }
            let old = safe_relative(&fields[0][2..], "mutation diff old path")?;
            let new = safe_relative(&fields[1][2..], "mutation diff new path")?;
            if old != new {
                return Err("mutation patches may not rename files".to_owned());
            }
            diff_paths.push(new);
        } else if let Some(rest) = line.strip_prefix("--- ") {
            let raw = rest.split('\t').next().unwrap_or_default();
            if raw == "/dev/null" {
                return Err("mutation patches may not create or delete files".to_owned());
            }
            let Some(raw) = raw.strip_prefix("a/") else {
                return Err(format!("unsupported mutation patch path: {raw:?}"));
            };
            old_paths.push(safe_relative(raw, "mutation patch path")?);
        } else if let Some(rest) = line.strip_prefix("+++ ") {
            let raw = rest.split('\t').next().unwrap_or_default();
            if raw == "/dev/null" {
                return Err("mutation patches may not create or delete files".to_owned());
            }
            let Some(raw) = raw.strip_prefix("b/") else {
                return Err(format!("unsupported mutation patch path: {raw:?}"));
            };
            paths.push(safe_relative(raw, "mutation patch path")?);
        }
    }
    if paths.is_empty() {
        return Err(format!(
            "mutation patch contains no modified paths: {}",
            path.display()
        ));
    }
    diff_paths.sort();
    paths.sort();
    old_paths.sort();
    if diff_paths != paths {
        return Err("mutation patch diff headers do not match modified paths".to_owned());
    }
    if old_paths != paths {
        return Err("mutation patch old/new paths do not match".to_owned());
    }
    paths.dedup();
    Ok(paths)
}

fn python_list(values: impl IntoIterator<Item = String>) -> String {
    let rows: Vec<String> = values
        .into_iter()
        .map(|value| python_repr(Some(&Value::String(value))))
        .collect();
    format!("[{}]", rows.join(", "))
}

pub(crate) fn load_registry_manifest_paths(
    root: &Path,
    registry_path: &Path,
    canonical_patterns: Option<&[String]>,
) -> Result<(Map<String, Value>, Vec<String>), String> {
    let resolved = fs::canonicalize(registry_path).unwrap_or_else(|_| registry_path.to_path_buf());
    let resolved_root = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    if resolved.strip_prefix(&resolved_root).is_err() {
        return Err("mutation registry must be inside the repository".to_owned());
    }
    let (_, payload) = read_json_object(&resolved, "mutation registry")?;
    if payload.get("schema_version").and_then(Value::as_i64) != Some(1) {
        return Err(format!(
            "unsupported registry schema_version={}; expected 1",
            python_repr(payload.get("schema_version"))
        ));
    }
    if payload.get("enforcement").and_then(Value::as_str) != Some("changed_tests") {
        return Err("registry enforcement must be 'changed_tests'".to_owned());
    }
    let patterns = string_list(payload.get("test_patterns"), "registry.test_patterns")?;
    if canonical_patterns.is_some_and(|expected| patterns != expected) {
        return Err("registry.test_patterns must match the canonical inventory".to_owned());
    }
    string_list(
        payload.get("receipt_directories"),
        "registry.receipt_directories",
    )?;
    let Some(rows) = payload.get("campaigns").and_then(Value::as_array) else {
        return Err("registry.campaigns must be a list".to_owned());
    };
    let mut manifests = Vec::with_capacity(rows.len());
    for (index, row) in rows.iter().enumerate() {
        let row = row
            .as_object()
            .ok_or_else(|| format!("registry.campaigns[{index}] must be a JSON object"))?;
        let manifest = required_string(
            row,
            "manifest",
            &format!("registry.campaigns[{index}].manifest"),
        )?;
        manifests.push(safe_relative(
            &manifest,
            &format!("registry.campaigns[{index}].manifest"),
        )?);
    }
    manifests.extend(load_registry_fragments(&resolved)?);
    let mut seen = BTreeSet::new();
    manifests.retain(|manifest| seen.insert(manifest.clone()));
    if manifests.is_empty() {
        return Err(
            "registry must declare at least one campaign, in registry.campaigns or registry.d/"
                .to_owned(),
        );
    }
    Ok((payload, manifests))
}

/// Manifest paths declared by one-campaign-per-file fragments in `registry.d/`.
///
/// Registering a campaign by appending to the shared `registry.campaigns` array serialises every
/// lane in the fleet: the array is one file, so one claim on it blocks all other registration and
/// every concurrent append conflicts. A fragment is a distinct path, so lanes register in
/// parallel without touching each other's files. Both forms are read; neither is preferred.
fn load_registry_fragments(registry_path: &Path) -> Result<Vec<String>, String> {
    let Some(directory) = registry_path
        .parent()
        .map(|parent| parent.join("registry.d"))
    else {
        return Ok(Vec::new());
    };
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        // No fragments registered yet is the ordinary case, not an error.
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(format!(
                "cannot read campaign fragment directory {}: {error}",
                directory.display()
            ))
        }
    };
    // Sorted so the manifest order a given tree yields is stable across filesystems.
    let mut fragments: Vec<PathBuf> = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| {
            format!(
                "cannot read campaign fragment directory {}: {error}",
                directory.display()
            )
        })?;
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) == Some("json") {
            fragments.push(path);
        }
    }
    fragments.sort();
    let mut manifests = Vec::with_capacity(fragments.len());
    for fragment in fragments {
        let label = fragment
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("<fragment>")
            .to_owned();
        let (_, payload) = read_json_object(&fragment, &format!("registry.d/{label}"))?;
        let manifest = required_string(
            &payload,
            "manifest",
            &format!("registry.d/{label}.manifest"),
        )?;
        manifests.push(safe_relative(
            &manifest,
            &format!("registry.d/{label}.manifest"),
        )?);
    }
    Ok(manifests)
}

fn validate_value_analysis(
    value: Option<&Value>,
    ranked_nodeids: &[String],
    mutation_ids: &[String],
) -> Result<Option<ValueContract>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    let payload = object(Some(value), "value_analysis")?;
    if payload.get("enabled") != Some(&Value::Bool(true)) {
        return Err(
            "invalid value_analysis: value_analysis.enabled must be true when present".to_owned(),
        );
    }
    if !matches!(
        payload.get("adapter").and_then(Value::as_str),
        Some("pytest-junit" | "ctest-junit" | "cargo-libtest")
    ) {
        return Err(
            "invalid value_analysis: value_analysis.adapter must be 'pytest-junit', \
             'ctest-junit' or 'cargo-libtest'"
                .to_owned(),
        );
    }
    let repetitions = payload
        .get("baseline_repetitions")
        .and_then(Value::as_u64)
        .filter(|value| (2..=5).contains(value))
        .ok_or_else(|| {
            "invalid value_analysis: baseline_repetitions must be an integer in [2, 5]".to_owned()
        })?;
    let tests = payload
        .get("tests")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            "invalid value_analysis: value_analysis.tests must be a non-empty list".to_owned()
        })?;
    if tests.is_empty() {
        return Err(
            "invalid value_analysis: value_analysis.tests must be a non-empty list".to_owned(),
        );
    }
    let mut actual = Vec::with_capacity(tests.len());
    for (index, row) in tests.iter().enumerate() {
        let row = row.as_object().ok_or_else(|| {
            format!("invalid value_analysis: value_analysis.tests[{index}] must be an object")
        })?;
        actual.push(required_string(
            row,
            "nodeid",
            &format!("value_analysis.tests[{index}].nodeid"),
        )?);
    }
    if actual != ranked_nodeids {
        return Err(
            "invalid value_analysis: value_analysis.tests must exactly match ranked_tests in rank order"
                .to_owned(),
        );
    }
    let mutation_contracts = object(
        payload.get("mutation_contracts"),
        "value_analysis.mutation_contracts",
    )?;
    if mutation_contracts.len() != mutation_ids.len()
        || mutation_ids
            .iter()
            .any(|mutation_id| !mutation_contracts.contains_key(mutation_id))
    {
        return Err(
            "invalid value_analysis: mutation_contracts must exactly match planned mutations in order"
                .to_owned(),
        );
    }
    Ok(Some(ValueContract {
        expected_nodeids: ranked_nodeids.to_vec(),
        expected_repetitions: repetitions,
    }))
}

fn inventory_python_test_nodeids(root: &Path, relative: &str) -> Result<Vec<String>, String> {
    let source = fs::read_to_string(root.join(relative))
        .map_err(|error| format!("cannot inventory Python tests in {relative}: {error}"))?;
    let statements = ruff_python_parser::parse_module(&source)
        .map_err(|error| format!("cannot inventory Python tests in {relative}: {error}"))?
        .into_syntax()
        .body;
    let mut nodeids = Vec::new();
    for statement in statements {
        match statement {
            ast::Stmt::FunctionDef(function) if function.name.starts_with("test_") => {
                nodeids.push(format!("{relative}::{}", function.name));
            }
            ast::Stmt::ClassDef(class) if class.name.starts_with("Test") => {
                for child in class.body {
                    if let ast::Stmt::FunctionDef(function) = child {
                        if function.name.starts_with("test_") {
                            nodeids.push(format!("{relative}::{}::{}", class.name, function.name));
                        }
                    }
                }
            }
            _ => {}
        }
    }
    if nodeids.is_empty() {
        return Err(format!("complete Python test scope is empty: {relative}"));
    }
    Ok(nodeids)
}

/// Inventory the `#[test]` functions in one Rust source file, in source order.
///
/// The nodeid shape matches what `cargo_test` scopes already declare — `<path>::<fn>`,
/// with no module path — so a name repeated in two `mod` blocks is ambiguous and is
/// refused rather than silently collapsed. Attributes may stack (`#[test]` then
/// `#[ignore]`), so the scan walks forward from the marker to the first `fn` item.
/// How an outer attribute relates to test discovery.
#[derive(Debug, PartialEq, Eq)]
enum TestAttribute {
    /// `#[test]`, or any path whose final segment is `test` (`#[tokio::test(..)]`).
    Marks,
    /// A framework marker this reader cannot expand (`#[rstest]`, `#[test_case(..)]`).
    /// Guessing would silently shrink a scope that claims to be complete, so refuse.
    Unrecognised,
    Other,
}

/// Read one outer attribute starting at `start`, following continuation lines until its
/// brackets balance. Returns the attribute path and the last line it occupies.
fn read_rust_attribute(lines: &[&str], start: usize) -> Option<(String, usize)> {
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escaped = false;
    let mut text = String::new();
    for (offset, line) in lines[start..].iter().enumerate() {
        for ch in line.chars() {
            let inside_before = depth > 0;
            if escaped {
                escaped = false;
            } else if in_string {
                match ch {
                    '\\' => escaped = true,
                    '"' => in_string = false,
                    _ => {}
                }
            } else {
                match ch {
                    '"' => in_string = true,
                    '[' => depth += 1,
                    ']' => depth -= 1,
                    _ => {}
                }
            }
            if inside_before {
                if depth == 0 {
                    let path: String = text
                        .chars()
                        .take_while(|c| !matches!(c, '(' | '=' | ' ' | '\t'))
                        .collect();
                    return Some((path.trim().to_owned(), start + offset));
                }
                text.push(ch);
            }
        }
        if depth <= 0 {
            break;
        }
    }
    None
}

fn classify_rust_attribute(path: &str) -> TestAttribute {
    let last = path.rsplit("::").next().unwrap_or(path);
    if last == "test" {
        TestAttribute::Marks
    } else if last.contains("test") {
        TestAttribute::Unrecognised
    } else {
        TestAttribute::Other
    }
}

/// Inventory `#[test]` functions in a Rust source file, in source order, as the
/// `<path>::<fn>` nodeids a `cargo_test` scope declares.
fn inventory_rust_test_nodeids(root: &Path, relative: &str) -> Result<Vec<String>, String> {
    let source = fs::read_to_string(root.join(relative))
        .map_err(|error| format!("cannot inventory Rust tests in {relative}: {error}"))?;
    let lines: Vec<&str> = source.lines().collect();
    let mut names: Vec<String> = Vec::new();
    let mut pending: Option<usize> = None;
    let mut index = 0usize;
    while index < lines.len() {
        let trimmed = lines[index].trim();
        if trimmed.is_empty() || trimmed.starts_with("//") {
            index += 1;
            continue;
        }
        if trimmed.starts_with("#[") {
            let (path, end) = read_rust_attribute(&lines, index).ok_or_else(|| {
                format!(
                    "cannot inventory Rust tests in {relative}: unterminated attribute at line {}",
                    index + 1
                )
            })?;
            match classify_rust_attribute(&path) {
                TestAttribute::Marks => pending = pending.or(Some(index)),
                TestAttribute::Unrecognised => {
                    return Err(format!(
                        "cannot inventory Rust tests in {relative}: line {} uses #[{path}], a test \
                         framework this reader cannot expand; a complete cargo_test scope over \
                         this file would silently undercount",
                        index + 1
                    ))
                }
                TestAttribute::Other => {}
            }
            index = end + 1;
            continue;
        }
        if let Some(opened) = pending {
            let name = rust_fn_name(trimmed).ok_or_else(|| {
                format!(
                    "cannot inventory Rust tests in {relative}: #[test] at line {} has no fn",
                    opened + 1
                )
            })?;
            if names.contains(&name) {
                return Err(format!(
                    "cannot inventory Rust tests in {relative}: duplicate test name {name:?}; \
                     cargo_test nodeids carry no module path"
                ));
            }
            names.push(name);
            pending = None;
        }
        index += 1;
    }
    if let Some(opened) = pending {
        return Err(format!(
            "cannot inventory Rust tests in {relative}: #[test] at line {} has no fn",
            opened + 1
        ));
    }
    if names.is_empty() {
        return Err(format!("complete Rust test scope is empty: {relative}"));
    }
    Ok(names
        .into_iter()
        .map(|name| format!("{relative}::{name}"))
        .collect())
}
/// Read the test-function name from a `static void test_*(void)` definition.
///
/// The signature is part of the match on purpose: a `test_`-prefixed helper
/// with any other signature (a sink callback, a fixture) is not a test, and
/// CMake registers exactly the same shape, so the inventory and the ctest
/// registration cannot drift apart.
fn c_test_fn_name(line: &str) -> Option<String> {
    let rest = line.strip_prefix("static void ")?;
    let (name, tail) = rest.split_once('(')?;
    if !name.starts_with("test_") || name.is_empty() {
        return None;
    }
    if !name
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || character == '_')
    {
        return None;
    }
    let tail = tail.trim_start();
    let rest = tail.strip_prefix("void)")?;
    // A declaration is not a definition: `static void test_x(void);` registers
    // no test, and counting it would make a complete scope claim a case ctest
    // never runs.
    if rest.trim_start().starts_with('{') {
        Some(name.to_owned())
    } else {
        None
    }
}

/// Inventory `static void test_*(void)` definitions in a C or C++ source file,
/// in source order, as the `<path>::<fn>` nodeids a `c_test` scope declares.
fn inventory_c_test_nodeids(root: &Path, relative: &str) -> Result<Vec<String>, String> {
    let source = fs::read_to_string(root.join(relative))
        .map_err(|error| format!("cannot inventory C tests in {relative}: {error}"))?;
    let mut names: Vec<String> = Vec::new();
    for line in source.lines() {
        // Not trimmed: CMake registers cases with a `^static void`-anchored
        // regex, so an indented definition is never a registered ctest and
        // counting it would make a complete scope claim a case that cannot run.
        let Some(name) = c_test_fn_name(line) else {
            continue;
        };
        if names.contains(&name) {
            return Err(format!(
                "cannot inventory C tests in {relative}: duplicate test name {name:?}; \
                 ctest names are flat and the two cases would be indistinguishable"
            ));
        }
        names.push(name);
    }
    if names.is_empty() {
        return Err(format!("complete C test scope is empty: {relative}"));
    }
    Ok(names
        .into_iter()
        .map(|name| format!("{relative}::{name}"))
        .collect())
}

/// Extract the identifier from a `fn` item line, ignoring visibility and `async`.
fn rust_fn_name(line: &str) -> Option<String> {
    let mut rest = line;
    for prefix in [
        "pub(crate) ",
        "pub(super) ",
        "pub ",
        "async ",
        "const ",
        "unsafe ",
        "extern ",
    ] {
        while let Some(stripped) = rest.strip_prefix(prefix) {
            rest = stripped.trim_start();
        }
    }
    let rest = rest.strip_prefix("fn ")?.trim_start();
    let end = rest
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .unwrap_or(rest.len());
    if end == 0 {
        return None;
    }
    Some(rest[..end].to_owned())
}

/// Read a `{repository-relative path: sha256}` map, rejecting anything that is not one.
///
/// Both digest maps in a manifest are compared against a receipt byte for byte, so a
/// loosely-typed entry here would fail later as an unexplained mismatch rather than as the
/// malformed manifest it is.
fn digest_map(payload: &Map<String, Value>, key: &str) -> Result<Map<String, Value>, String> {
    let raw = object(payload.get(key), key)?;
    let mut digests = Map::new();
    for (raw_path, raw_digest) in raw {
        let path = safe_relative(raw_path, &format!("{key} path"))?;
        let digest = raw_digest
            .as_str()
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| format!("{key}[{path}] must be a non-empty string"))?;
        if !valid_sha256(digest) {
            return Err(format!("{key}[{path}] must be a lowercase SHA-256 digest"));
        }
        digests.insert(path, Value::String(digest.to_owned()));
    }
    if digests.is_empty() {
        return Err(format!("{key} must name at least one file"));
    }
    Ok(digests)
}

/// Build the contract for a campaign whose mutants come from an engine.
///
/// Deliberately much smaller than the patch-based loader, because almost everything that one
/// validates is curation: the ranked test list, the per-mutant contracts and rationales, the
/// planned-mutation slots, the killer assignments. None of it exists here, and none of it can,
/// since no human chose the mutants. What remains is the part that actually pins evidence to
/// code -- which tests ran, which sources were mutated, and which survivors were already
/// accepted -- and that is what this reads.
fn generated_campaign_contract(
    payload: &Map<String, Value>,
    manifest_bytes: Vec<u8>,
    relative_manifest: &str,
    mutation_engine: &str,
) -> Result<CampaignContract, String> {
    let generator = object(payload.get("generator"), "generator")?;
    let source_sha256 = digest_map(payload, "source_sha256")?;
    let test_sha256 = digest_map(payload, "test_sha256")?;

    let test_argv = string_list(payload.get("test_argv"), "test_argv")?;
    if test_argv.is_empty() {
        return Err("test_argv must be a non-empty list".to_owned());
    }
    // `pytest a.py b.py` names its files; `ctest` and `cargo test` run a whole suite and name
    // none. Both are honest, and a suite runner covers more than it declares, never less. What
    // is not honest is naming some and quietly claiming another, so the check bites there.
    let named: Vec<&String> = test_sha256
        .keys()
        .filter(|path| test_argv.iter().any(|argument| &argument == path))
        .collect();
    if !named.is_empty() && named.len() != test_sha256.len() {
        let missing: Vec<&str> = test_sha256
            .keys()
            .filter(|path| !named.contains(path))
            .map(String::as_str)
            .collect();
        return Err(format!(
            "test_argv names some declared tests but not {}",
            missing.join(", ")
        ));
    }

    let mut survivor_baseline = Vec::new();
    match payload.get("survivor_baseline") {
        Some(Value::Array(rows)) => {
            for (index, row) in rows.iter().enumerate() {
                let id = row
                    .as_str()
                    .filter(|value| !value.trim().is_empty())
                    .ok_or_else(|| {
                        format!("survivor_baseline[{index}] must be a non-empty string")
                    })?;
                survivor_baseline.push(id.to_owned());
            }
        }
        _ => return Err("survivor_baseline must be a list".to_owned()),
    }
    let unique: BTreeSet<&String> = survivor_baseline.iter().collect();
    if unique.len() != survivor_baseline.len() {
        return Err("survivor_baseline contains duplicate mutant ids".to_owned());
    }

    let timeout_seconds = generator
        .get("run_timeout_seconds")
        .and_then(Value::as_u64)
        .filter(|value| *value >= 1)
        .ok_or_else(|| "generator.run_timeout_seconds must be a positive integer".to_owned())?;

    let environment = payload
        .get("environment")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();

    // The scope is complete by construction: a generated campaign runs the whole test command,
    // never a hand-picked subset of node ids, so there is no partial mode for it to be in.
    let mut test_scopes = Map::new();
    for path in test_sha256.keys() {
        test_scopes.insert(
            path.clone(),
            json!({"mode": "complete", "selection": "test_argv"}),
        );
    }

    Ok(CampaignContract {
        campaign_id: required_string(payload, "campaign_id", "campaign_id")?,
        title: required_string(payload, "title", "title")?,
        language: required_string(payload, "language", "language")?,
        mutation_engine: mutation_engine.to_owned(),
        expected_mutations: 0,
        manifest: relative_manifest.to_owned(),
        manifest_sha256: format!("{:x}", Sha256::digest(manifest_bytes)),
        source_sha256: Value::Object(source_sha256),
        source_symbols: Value::Object(Map::new()),
        test_scopes,
        ranked_tests: Vec::new(),
        ranked_test_paths: test_sha256.keys().cloned().collect(),
        planned_mutations: Vec::new(),
        mutations: Vec::new(),
        test_argv,
        timeout_seconds,
        blocked_process_substrings: Vec::new(),
        poll_seconds: 0,
        environment,
        host_read_dependencies: Vec::new(),
        value_analysis_payload: None,
        value_analysis: None,
        source_drifted: false,
        generated: true,
        survivor_baseline,
        test_sha256: Value::Object(test_sha256),
    })
}

pub(crate) fn load_campaign_contract(
    root: &Path,
    relative_manifest: &str,
    python_test_nodeids: &HashMap<String, Vec<String>>,
    symbol_hashes: &HashMap<String, HashMap<String, String>>,
    candidate_paths: &BTreeSet<String>,
    inventory_from_source: bool,
) -> Result<CampaignContract, String> {
    let manifest_path = root.join(relative_manifest);
    let resolved = fs::canonicalize(&manifest_path).unwrap_or_else(|_| manifest_path.clone());
    let resolved_root = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    if resolved.strip_prefix(&resolved_root).is_err() {
        return Err("campaign manifest must be inside the repository".to_owned());
    }
    let (manifest_bytes, payload) = read_json_object(&resolved, "campaign")?;
    if payload.get("schema_version").and_then(Value::as_i64) != Some(1) {
        return Err(format!(
            "unsupported schema_version={}; expected 1",
            python_repr(payload.get("schema_version"))
        ));
    }
    let declared_engine = payload
        .get("mutation_engine")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    if GENERATED_ENGINES.contains(&declared_engine.as_str()) {
        return generated_campaign_contract(
            &payload,
            manifest_bytes,
            relative_manifest,
            &declared_engine,
        );
    }
    let expected_mutations = payload
        .get("expected_mutations")
        .and_then(Value::as_u64)
        .filter(|value| *value >= 1)
        .ok_or_else(|| "expected_mutations must be a positive integer".to_owned())?
        as usize;
    let source_raw = object(payload.get("source_sha256"), "source_sha256")?;
    let mut source_sha256 = Map::new();
    for (raw_path, raw_digest) in source_raw {
        let path = safe_relative(raw_path, "source_sha256 path")?;
        let digest = raw_digest
            .as_str()
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| format!("source_sha256[{path}] must be a non-empty string"))?;
        if !valid_sha256(digest) {
            return Err(format!(
                "source_sha256[{path}] must be a lowercase SHA-256 digest"
            ));
        }
        source_sha256.insert(path, Value::String(digest.to_owned()));
    }

    let ranked_rows = payload
        .get("ranked_tests")
        .and_then(Value::as_array)
        .filter(|rows| !rows.is_empty())
        .ok_or_else(|| "ranked_tests must be a non-empty list".to_owned())?;
    let mut ranked_nodeids = Vec::with_capacity(ranked_rows.len());
    let mut ranks = Vec::with_capacity(ranked_rows.len());
    let mut ranked_tests = Vec::with_capacity(ranked_rows.len());
    for (index, raw) in ranked_rows.iter().enumerate() {
        let row = raw
            .as_object()
            .ok_or_else(|| format!("ranked_tests[{index}] must be a JSON object"))?;
        let rank = row
            .get("rank")
            .and_then(Value::as_u64)
            .ok_or_else(|| format!("ranked_tests[{index}].rank must be an integer"))?;
        let nodeid = required_string(row, "nodeid", "ranked test nodeid")?;
        let contract = required_string(row, "contract", "ranked test contract")?;
        let rationale = required_string(row, "rationale", "ranked test rationale")?;
        ranks.push(rank as usize);
        ranked_nodeids.push(nodeid.clone());
        ranked_tests.push(RankedTestContract {
            rank: rank as i64,
            nodeid,
            contract,
            rationale,
        });
    }
    let expected_ranks: Vec<usize> = (1..=ranked_rows.len()).collect();
    if ranks != expected_ranks {
        return Err(format!(
            "ranked_tests must be ordered with contiguous ranks, got {ranks:?}"
        ));
    }
    if ranked_nodeids.iter().collect::<BTreeSet<_>>().len() != ranked_nodeids.len() {
        return Err("ranked_tests contains duplicate nodeids".to_owned());
    }
    let expected_ranked = payload
        .get("expected_ranked_tests")
        .and_then(Value::as_u64)
        .map_or(ranked_nodeids.len(), |value| value as usize);
    if expected_ranked != ranked_nodeids.len() {
        return Err(format!(
            "expected_ranked_tests={expected_ranked}, but {} tests are ranked",
            ranked_nodeids.len()
        ));
    }

    let planned_rows = payload
        .get("planned_mutations")
        .and_then(Value::as_array)
        .ok_or_else(|| "planned_mutations must be a list".to_owned())?;
    if planned_rows.len() != expected_mutations {
        return Err(format!(
            "expected {expected_mutations} planned mutation slots, got {}",
            planned_rows.len()
        ));
    }
    let mut planned_ids = Vec::with_capacity(planned_rows.len());
    let mut planned_targets = HashMap::new();
    let mut planned_killers = HashMap::new();
    let mut planned_mutations = Vec::with_capacity(planned_rows.len());
    for (index, raw) in planned_rows.iter().enumerate() {
        let row = raw
            .as_object()
            .ok_or_else(|| format!("planned_mutations[{index}] must be a JSON object"))?;
        let id = required_string(row, "id", &format!("planned_mutations[{index}].id"))?;
        let target = safe_relative(
            &required_string(
                row,
                "target_path",
                &format!("planned_mutations[{index}].target_path"),
            )?,
            &format!("planned_mutations[{index}].target_path"),
        )?;
        let contract = required_string(
            row,
            "contract",
            &format!("planned_mutations[{index}].contract"),
        )?;
        let description = required_string(
            row,
            "description",
            &format!("planned_mutations[{index}].description"),
        )?;
        let killers = string_list(
            row.get("expected_killers"),
            &format!("planned_mutations[{index}].expected_killers"),
        )?;
        planned_targets.insert(id.clone(), target.clone());
        planned_killers.insert(id.clone(), killers.clone());
        planned_mutations.push(PlannedMutationContract {
            id: id.clone(),
            target_path: target,
            contract,
            description,
            expected_killers: killers,
        });
        planned_ids.push(id);
    }
    if planned_ids.iter().collect::<BTreeSet<_>>().len() != planned_ids.len() {
        return Err("planned_mutations contains duplicate ids".to_owned());
    }

    let mutation_rows = payload
        .get("mutations")
        .and_then(Value::as_array)
        .ok_or_else(|| "mutations must be a list".to_owned())?;
    let mut mutations = Vec::with_capacity(mutation_rows.len());
    let mut mutation_ids = Vec::with_capacity(mutation_rows.len());
    for (index, raw) in mutation_rows.iter().enumerate() {
        let row = raw
            .as_object()
            .ok_or_else(|| format!("mutations[{index}] must be a JSON object"))?;
        let id = required_string(row, "id", &format!("mutations[{index}].id"))?;
        let patch_relative = safe_relative(
            &required_string(row, "patch_file", &format!("mutations[{index}].patch_file"))?,
            &format!("mutations[{index}].patch_file"),
        )?;
        let patch_path = resolved.parent().unwrap_or(root).join(&patch_relative);
        let patch_resolved = fs::canonicalize(&patch_path).unwrap_or(patch_path);
        if patch_resolved.strip_prefix(&resolved_root).is_err() {
            return Err(format!(
                "mutations[{index}].patch_file escapes the repository"
            ));
        }
        let allowed = string_list(
            row.get("allowed_paths"),
            &format!("mutations[{index}].allowed_paths"),
        )?;
        if allowed.is_empty() {
            return Err(format!("mutations[{index}].allowed_paths may not be empty"));
        }
        let mut allowed_paths: Vec<String> = allowed
            .iter()
            .map(|path| safe_relative(path, &format!("mutations[{index}].allowed_paths")))
            .collect::<Result<_, _>>()?;
        let patch_sha256 = required_string(
            row,
            "patch_sha256",
            &format!("mutations[{index}].patch_sha256"),
        )?;
        if !valid_sha256(&patch_sha256) {
            return Err(format!(
                "mutations[{index}].patch_sha256 must be a lowercase SHA-256 digest"
            ));
        }
        let actual_sha256 = sha256_file(&patch_resolved);
        if actual_sha256.as_deref() != Some(patch_sha256.as_str()) {
            let actual_value = actual_sha256.clone().map(Value::String);
            return Err(format!(
                "mutation {} patch hash drifted: expected {patch_sha256}, got {}",
                python_repr(Some(&Value::String(id.clone()))),
                python_repr(actual_value.as_ref())
            ));
        }
        allowed_paths.sort();
        allowed_paths.dedup();
        let actual_paths = patch_paths(&patch_resolved)?;
        if actual_paths != allowed_paths {
            return Err(format!(
                "mutation {} patch paths {} do not match allowed_paths {}",
                python_repr(Some(&Value::String(id.clone()))),
                python_list(actual_paths),
                python_list(allowed_paths)
            ));
        }
        let killers = string_list(
            row.get("expected_killers"),
            &format!("mutations[{index}].expected_killers"),
        )?;
        if planned_killers.get(&id) != Some(&killers) {
            return Err(format!(
                "mutation {} expected_killers drifted from its slot",
                python_repr(Some(&Value::String(id.clone())))
            ));
        }
        let target = planned_targets.get(&id).ok_or_else(|| {
            format!(
                "materialized mutations lack planned slots: [{}]",
                python_repr(Some(&Value::String(id.clone())))
            )
        })?;
        if !allowed_paths.contains(target) {
            return Err(format!(
                "mutation {} does not patch its planned target",
                python_repr(Some(&Value::String(id.clone())))
            ));
        }
        mutations.push(MutationContract {
            id: id.clone(),
            patch_file: patch_resolved.to_string_lossy().into_owned(),
            patch_sha256,
            allowed_paths,
            expected_killers: killers,
        });
        mutation_ids.push(id);
    }
    if mutation_ids.iter().collect::<BTreeSet<_>>().len() != mutation_ids.len() {
        return Err("mutations contains duplicate ids".to_owned());
    }

    let baseline = object(payload.get("baseline"), "baseline")?;
    let test_argv = string_list(baseline.get("argv"), "baseline.argv")?;
    let timeout_seconds = baseline
        .get("timeout_seconds")
        .and_then(Value::as_u64)
        .filter(|value| *value >= 1)
        .ok_or_else(|| "baseline.timeout_seconds must be a positive integer".to_owned())?;
    let missing_tests: Vec<String> = ranked_nodeids
        .iter()
        .filter(|nodeid| {
            !test_argv.contains(*nodeid)
                && !test_argv.contains(&nodeid.split("::").next().unwrap_or_default().to_owned())
        })
        .cloned()
        .collect();
    if !missing_tests.is_empty() {
        return Err(format!(
            "baseline.argv omits ranked tests: {}",
            python_list(missing_tests)
        ));
    }

    let empty_object = Value::Object(Map::new());
    let resource_gate = object(
        payload.get("resource_gate").or(Some(&empty_object)),
        "resource_gate",
    )?;
    let poll_seconds = resource_gate
        .get("poll_seconds")
        .map_or(Some(30), Value::as_u64)
        .filter(|value| (1..=300).contains(value))
        .ok_or_else(|| "resource_gate.poll_seconds must be in [1, 300]".to_owned())?;
    let blocked_process_substrings = string_list(
        resource_gate
            .get("blocked_process_substrings")
            .or(Some(&Value::Array(Vec::new()))),
        "resource_gate.blocked_process_substrings",
    )?;
    let environment_raw = object(
        payload.get("environment").or(Some(&empty_object)),
        "environment",
    )?;
    let mut environment = Map::new();
    for (key, value) in environment_raw {
        if key.trim().is_empty() {
            return Err("environment key must be a non-empty string".to_owned());
        }
        if !value.is_string() {
            return Err(format!("environment[{key:?}] must be a string"));
        }
        environment.insert(key.clone(), value.clone());
    }
    let host_read_dependencies = string_list(
        payload
            .get("host_read_dependencies")
            .or(Some(&Value::Array(Vec::new()))),
        "host_read_dependencies",
    )?
    .into_iter()
    .map(|path| safe_relative(&path, "host_read_dependencies"))
    .collect::<Result<Vec<_>, _>>()?;

    let ranked_paths: Vec<String> = ranked_nodeids
        .iter()
        .map(|nodeid| nodeid.split("::").next().unwrap_or_default().to_owned())
        .collect();
    let ranked_path_set: BTreeSet<&str> = ranked_paths.iter().map(String::as_str).collect();
    let empty_test_scopes = Value::Object(Map::new());
    let scopes = object(
        payload.get("test_scopes").or(Some(&empty_test_scopes)),
        "test_scopes",
    )?;
    let mut test_scopes = Map::new();
    for (raw_path, raw_scope) in scopes {
        let path = safe_relative(raw_path, "test_scopes path")?;
        let row = raw_scope
            .as_object()
            .ok_or_else(|| format!("test_scopes[{path}] must be a JSON object"))?;
        let mode = required_string(row, "mode", &format!("test_scopes[{path}].mode"))?;
        if mode != "complete" && mode != "partial" {
            return Err(format!(
                "test_scopes[{path}].mode must be 'complete' or 'partial'"
            ));
        }
        let inventory =
            required_string(row, "inventory", &format!("test_scopes[{path}].inventory"))?;
        let nodeids = string_list(row.get("nodeids"), &format!("test_scopes[{path}].nodeids"))?;
        if nodeids.is_empty() {
            return Err(format!("test_scopes[{path}].nodeids may not be empty"));
        }
        if nodeids.iter().collect::<BTreeSet<_>>().len() != nodeids.len() {
            return Err(format!("test_scopes[{path}].nodeids contains duplicates"));
        }
        let wrong: Vec<String> = nodeids
            .iter()
            .filter(|nodeid| nodeid.split("::").next() != Some(path.as_str()))
            .cloned()
            .collect();
        if !wrong.is_empty() {
            return Err(format!(
                "test_scopes[{path}] contains nodeids from another file: {}",
                python_list(wrong)
            ));
        }
        if !source_sha256.contains_key(&path) {
            return Err(format!("test_scopes[{path}] is not bound in source_sha256"));
        }
        if inventory != "python_ast"
            && inventory != "cargo_test"
            && inventory != "c_test"
            && mode == "complete"
        {
            return Err(format!(
                "complete test scope inventory is unsupported: {inventory:?}"
            ));
        }
        if inventory == "python_ast" && !path.ends_with(".py") {
            return Err(format!(
                "test_scopes[{path}].inventory='python_ast' requires a .py file"
            ));
        }
        if inventory == "cargo_test" && !path.ends_with(".rs") {
            return Err(format!(
                "test_scopes[{path}].inventory='cargo_test' requires a .rs file"
            ));
        }
        if inventory == "c_test"
            && !(path.ends_with(".c")
                || path.ends_with(".cc")
                || path.ends_with(".cpp")
                || path.ends_with(".cxx"))
        {
            return Err(format!(
                "test_scopes[{path}].inventory='c_test' requires a .c/.cc/.cpp/.cxx file"
            ));
        }
        if inventory == "c_test" && mode == "complete" {
            let discovered = inventory_c_test_nodeids(root, &path)?;
            if nodeids != discovered {
                let declared: BTreeSet<&str> = nodeids.iter().map(String::as_str).collect();
                let actual: BTreeSet<&str> = discovered.iter().map(String::as_str).collect();
                let missing = actual
                    .difference(&declared)
                    .map(|value| (*value).to_owned());
                let extra = declared
                    .difference(&actual)
                    .map(|value| (*value).to_owned());
                return Err(format!(
                    "complete test scope does not match current C inventory for {path}: missing={}, extra={}, expected_order={}",
                    python_list(missing),
                    python_list(extra),
                    python_list(discovered.clone())
                ));
            }
        }
        if inventory == "cargo_test" && mode == "complete" {
            let discovered = inventory_rust_test_nodeids(root, &path)?;
            if nodeids != discovered {
                let declared: BTreeSet<&str> = nodeids.iter().map(String::as_str).collect();
                let actual: BTreeSet<&str> = discovered.iter().map(String::as_str).collect();
                let missing = actual
                    .difference(&declared)
                    .map(|value| (*value).to_owned());
                let extra = declared
                    .difference(&actual)
                    .map(|value| (*value).to_owned());
                return Err(format!(
                    "complete test scope does not match current Rust inventory for {path}: missing={}, extra={}, expected_order={}",
                    python_list(missing),
                    python_list(extra),
                    python_list(discovered.clone())
                ));
            }
        }
        if inventory == "python_ast" && mode == "complete" {
            let source_inventory;
            let discovered = if let Some(discovered) = python_test_nodeids.get(&path) {
                discovered
            } else if inventory_from_source {
                source_inventory = inventory_python_test_nodeids(root, &path)?;
                &source_inventory
            } else {
                return Err(format!(
                    "cannot inventory Python tests in {path}: native AST map is missing"
                ));
            };
            if &nodeids != discovered {
                let declared: BTreeSet<&str> = nodeids.iter().map(String::as_str).collect();
                let actual: BTreeSet<&str> = discovered.iter().map(String::as_str).collect();
                let missing = actual
                    .difference(&declared)
                    .map(|value| (*value).to_owned());
                let extra = declared
                    .difference(&actual)
                    .map(|value| (*value).to_owned());
                return Err(format!(
                    "complete test scope does not match current Python inventory for {path}: missing={}, extra={}, expected_order={}",
                    python_list(missing),
                    python_list(extra),
                    python_list(discovered.clone())
                ));
            }
        }
        if mode == "complete" && !ranked_path_set.contains(path.as_str()) {
            return Err(format!(
                "complete test_scopes[{path}] must contain at least one ranked test"
            ));
        }
        test_scopes.insert(
            path,
            json!({
                "mode": mode,
                "inventory": inventory,
                "nodeids": nodeids,
            }),
        );
    }
    let scoped_nodeids: BTreeSet<&str> = test_scopes
        .values()
        .filter_map(Value::as_object)
        .filter_map(|scope| scope.get("nodeids").and_then(Value::as_array))
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let mut missing_ranked: Vec<String> = ranked_nodeids
        .iter()
        .filter(|nodeid| !scoped_nodeids.contains(nodeid.as_str()))
        .cloned()
        .collect();
    missing_ranked.sort();
    if !missing_ranked.is_empty() {
        return Err(format!(
            "ranked_tests nodeids are missing from declared test_scopes: {}",
            python_list(missing_ranked)
        ));
    }

    let empty_source_symbols = Value::Object(Map::new());
    let source_symbols_raw = object(
        payload
            .get("source_symbols")
            .or(Some(&empty_source_symbols)),
        "source_symbols",
    )?;
    let mut source_symbols = Map::new();
    for (raw_path, raw_symbols) in source_symbols_raw {
        let path = safe_relative(raw_path, "source_symbols key")?;
        if !source_sha256.contains_key(&path) {
            return Err(format!(
                "source_symbols[{path:?}] is not bound in source_sha256; a symbolically pinned file must still declare the file it belongs to"
            ));
        }
        let symbols = raw_symbols
            .as_object()
            .filter(|symbols| !symbols.is_empty())
            .ok_or_else(|| {
                format!(
                    "source_symbols[{path:?}] is empty; omit the path instead of pinning nothing, which would silently disable drift detection for it"
                )
            })?;
        for (symbol, digest) in symbols {
            let digest = digest.as_str().unwrap_or_default();
            if symbol.trim().is_empty() || !valid_sha256(digest) {
                return Err(format!(
                    "source_symbols[{path:?}][{symbol:?}] must be a lowercase SHA-256 digest"
                ));
            }
        }
        source_symbols.insert(path, raw_symbols.clone());
    }

    let relevant = ranked_paths
        .iter()
        .any(|path| candidate_paths.contains(path) && source_sha256.contains_key(path));
    let mut source_drifted = false;
    if relevant {
        for (relative, expected) in &source_sha256 {
            let expected = expected.as_str().unwrap_or_default();
            if let Some(symbols) = source_symbols.get(relative).and_then(Value::as_object) {
                let current = symbol_hashes.get(relative);
                if current.is_none()
                    || symbols.iter().any(|(symbol, digest)| {
                        current
                            .and_then(|values| values.get(symbol))
                            .map(String::as_str)
                            != digest.as_str()
                    })
                {
                    source_drifted = true;
                    break;
                }
            } else if sha256_file(&root.join(relative)).as_deref() != Some(expected) {
                source_drifted = true;
                break;
            }
        }
    }

    let value_analysis =
        validate_value_analysis(payload.get("value_analysis"), &ranked_nodeids, &planned_ids)?;
    Ok(CampaignContract {
        campaign_id: required_string(&payload, "campaign_id", "campaign_id")?,
        title: required_string(&payload, "title", "title")?,
        language: required_string(&payload, "language", "language")?,
        mutation_engine: required_string(&payload, "mutation_engine", "mutation_engine")?,
        expected_mutations,
        manifest: relative_manifest.to_owned(),
        manifest_sha256: format!("{:x}", Sha256::digest(manifest_bytes)),
        source_sha256: Value::Object(source_sha256),
        source_symbols: Value::Object(source_symbols),
        test_scopes,
        ranked_tests,
        ranked_test_paths: ranked_paths,
        planned_mutations,
        mutations,
        test_argv,
        timeout_seconds,
        blocked_process_substrings,
        poll_seconds,
        environment,
        host_read_dependencies,
        value_analysis_payload: payload.get("value_analysis").cloned(),
        value_analysis,
        source_drifted,
        generated: false,
        survivor_baseline: Vec::new(),
        test_sha256: Value::Object(Map::new()),
    })
}

/// A registered campaign whose manifest could not be loaded, kept for reporting.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct BrokenCampaign {
    pub manifest: String,
    pub error: String,
}

/// Load every registered campaign, isolating per-manifest failures.
///
/// A manifest that fails to load is recorded rather than propagated. This cannot weaken the
/// gate: a campaign that does not load also does not match any candidate path, so every test
/// it claimed to cover falls through to "no registered campaign ranks this test file" and stays
/// blocked. Propagating instead made one lane's broken manifest refuse evidence repository-wide,
/// blocking agents whose changes it has nothing to do with.
pub(crate) fn load_campaigns(
    root: &Path,
    registry_path: &Path,
    canonical_test_patterns: &[String],
    symbol_hashes: &HashMap<String, HashMap<String, String>>,
    candidate_paths: &BTreeSet<String>,
) -> Result<(Vec<CampaignContract>, Vec<BrokenCampaign>), String> {
    let (_, manifests) =
        load_registry_manifest_paths(root, registry_path, Some(canonical_test_patterns))?;
    let mut loaded = Vec::with_capacity(manifests.len());
    let mut broken = Vec::new();
    for manifest in &manifests {
        match load_campaign_contract(
            root,
            manifest,
            &HashMap::new(),
            symbol_hashes,
            candidate_paths,
            true,
        ) {
            Ok(campaign) => loaded.push(campaign),
            Err(error) => broken.push(BrokenCampaign {
                manifest: manifest.clone(),
                error,
            }),
        }
    }
    Ok((loaded, broken))
}

fn value_error(message: impl Into<String>) -> PyErr {
    PyValueError::new_err(message.into())
}

fn parse_json<T: for<'de> Deserialize<'de>>(text: &str, label: &str) -> PyResult<T> {
    serde_json::from_str(text).map_err(|error| value_error(format!("invalid {label}: {error}")))
}

#[pyfunction]
pub fn load_mutation_campaign_native(request_json: &str) -> PyResult<String> {
    let request: CampaignLoadRequest = parse_json(request_json, "campaign load request")?;
    let root = lexical_absolute(Path::new(&request.repo_root)).map_err(value_error)?;
    let manifest =
        safe_relative(&request.manifest_path, "campaign manifest").map_err(value_error)?;
    let campaign = load_campaign_contract(
        &root,
        &manifest,
        &request.python_test_nodeids,
        &request.symbol_hashes,
        &BTreeSet::new(),
        true,
    )
    .map_err(value_error)?;
    serde_json::to_string(&campaign).map_err(|error| value_error(error.to_string()))
}

#[pyfunction]
pub fn load_mutation_registry_native(request_json: &str) -> PyResult<String> {
    let request: RegistryLoadRequest = parse_json(request_json, "registry load request")?;
    let root = lexical_absolute(Path::new(&request.repo_root)).map_err(value_error)?;
    let registry =
        safe_relative(&request.registry_path, "mutation registry").map_err(value_error)?;
    let (mut payload, manifests) = load_registry_manifest_paths(
        &root,
        &root.join(registry),
        Some(&request.canonical_test_patterns),
    )
    .map_err(value_error)?;
    // Hand back the resolved campaign list, not the raw array: fragments in `registry.d/` are
    // registrations like any other, and a Python caller reading `campaigns` must see them too.
    payload.insert(
        "campaigns".to_owned(),
        Value::Array(
            manifests
                .into_iter()
                .map(|manifest| json!({ "manifest": manifest }))
                .collect(),
        ),
    );
    serde_json::to_string(&payload).map_err(|error| value_error(error.to_string()))
}

#[pyfunction]
pub fn mutation_source_drift_native(request_json: &str) -> PyResult<String> {
    let request: SourceDriftRequest = parse_json(request_json, "source drift request")?;
    let root = lexical_absolute(Path::new(&request.repo_root)).map_err(value_error)?;
    let mut drift = Vec::new();
    for (relative, expected) in &request.source_sha256 {
        if request.source_symbols.contains_key(relative) {
            continue;
        }
        let path = root.join(relative);
        let actual = path.is_file().then(|| sha256_file(&path)).flatten();
        if actual.as_deref() != expected.as_str() {
            drift.push(json!({
                "path": relative,
                "expected_sha256": expected,
                "actual_sha256": actual,
                "is_symlink": path.is_symlink(),
            }));
        }
    }
    for (relative, raw_symbols) in &request.source_symbols {
        let path = root.join(relative);
        if !path.is_file() || path.is_symlink() {
            drift.push(json!({
                "path": relative,
                "symbol": Value::Null,
                "reason": "absent or symlink",
            }));
            continue;
        }
        let current = request.symbol_hashes.get(relative);
        if let Some(symbols) = raw_symbols.as_object() {
            for (symbol, expected) in symbols {
                let actual = current.and_then(|values| values.get(symbol));
                if actual.map(String::as_str) != expected.as_str() {
                    drift.push(json!({
                        "path": relative,
                        "symbol": symbol,
                        "expected_sha256": expected,
                        "actual_sha256": actual,
                        "reason": if actual.is_none() { "symbol removed" } else { "symbol changed" },
                    }));
                }
            }
        }
    }
    serde_json::to_string(&drift).map_err(|error| value_error(error.to_string()))
}

#[pyfunction]
pub fn inspect_mutation_campaign_native(request_json: &str) -> PyResult<String> {
    let request: InspectRequest = parse_json(request_json, "campaign inspect request")?;
    let campaign = request.campaign;
    let mut reasons = Vec::new();
    if campaign.mutations.len() != campaign.expected_mutations {
        reasons.push(format!(
            "materialized_mutations={}; expected={}",
            campaign.mutations.len(),
            campaign.expected_mutations
        ));
    }
    if !request.source_drift.is_empty() {
        reasons.push(format!("source_hash_drift={}", request.source_drift.len()));
    }
    if request.manifest_hash_drift {
        reasons.push("manifest_hash_drift=1".to_owned());
    }
    let materialized: BTreeSet<&str> = campaign
        .mutations
        .iter()
        .map(|mutation| mutation.id.as_str())
        .collect();
    let planned: Vec<Value> = campaign
        .planned_mutations
        .iter()
        .map(|mutation| {
            json!({
                "id": mutation.id,
                "target_path": mutation.target_path,
                "contract": mutation.contract,
                "description": mutation.description,
                "expected_killers": mutation.expected_killers,
                "materialized": materialized.contains(mutation.id.as_str()),
            })
        })
        .collect();
    let result = json!({
        "schema_version": "llm.mutation-testing.inspect.v1",
        "campaign_id": campaign.campaign_id,
        "title": campaign.title,
        "status": if reasons.is_empty() { "READY" } else { "NOT_READY" },
        "readiness_reasons": reasons,
        "resource_status": if request.blocking_processes.is_empty() { "IDLE" } else { "BUSY" },
        "blocking_processes": request.blocking_processes,
        "source_drift": request.source_drift,
        "manifest_sha256": campaign.manifest_sha256,
        "manifest_hash_drift": request.manifest_hash_drift,
        "expected_mutations": campaign.expected_mutations,
        "materialized_mutations": campaign.mutations.len(),
        "ranked_tests": campaign.ranked_tests,
        "test_scopes": campaign.test_scopes,
        "value_analysis": request.value_analysis,
        "planned_mutations": planned,
    });
    serde_json::to_string(&result).map_err(|error| value_error(error.to_string()))
}

#[pyfunction]
pub fn plan_mutation_repin_native(
    repo_root: &str,
    manifests: Vec<RepinManifest>,
    source_hashes: HashMap<String, String>,
    symbol_hashes: HashMap<String, HashMap<String, String>>,
) -> PyResult<Vec<(String, String)>> {
    let root = lexical_absolute(Path::new(repo_root)).map_err(value_error)?;
    let mut rows = Vec::with_capacity(manifests.len());
    for (raw_manifest, sources, source_symbols) in manifests {
        let manifest = safe_relative(&raw_manifest, "campaign manifest").map_err(value_error)?;
        let path = root.join(&manifest);
        let mut text = fs::read_to_string(&path).map_err(|error| {
            value_error(format!("cannot load campaign {}: {error}", path.display()))
        })?;
        for (relative, recorded) in sources {
            if !source_symbols.contains_key(&relative) {
                let Some(actual) = source_hashes.get(&relative) else {
                    continue;
                };
                if actual != &recorded && text.matches(&recorded).count() == 1 {
                    text = text.replacen(&recorded, actual, 1);
                }
            }
        }
        for (relative, symbols) in source_symbols {
            let Some(current) = symbol_hashes.get(&relative) else {
                continue;
            };
            for (symbol, recorded) in symbols {
                let Some(actual) = current.get(&symbol) else {
                    continue;
                };
                if actual != &recorded && text.matches(&recorded).count() == 1 {
                    text = text.replacen(&recorded, actual, 1);
                }
            }
        }
        rows.push((manifest, text));
    }
    Ok(rows)
}

#[pyfunction]
pub fn mutation_patch_paths_native(path: &str) -> PyResult<Vec<String>> {
    patch_paths(Path::new(path)).map_err(value_error)
}

pub(crate) fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(load_mutation_campaign_native, module)?)?;
    module.add_function(wrap_pyfunction!(load_mutation_registry_native, module)?)?;
    module.add_function(wrap_pyfunction!(mutation_source_drift_native, module)?)?;
    module.add_function(wrap_pyfunction!(inspect_mutation_campaign_native, module)?)?;
    module.add_function(wrap_pyfunction!(plan_mutation_repin_native, module)?)?;
    module.add_function(wrap_pyfunction!(mutation_patch_paths_native, module)?)?;
    Ok(())
}

#[cfg(test)]
mod registry_resilience_tests {
    use std::collections::{BTreeSet, HashMap};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::{load_campaigns, load_registry_fragments, load_registry_manifest_paths};

    static NEXT_TREE: AtomicU64 = AtomicU64::new(0);

    /// A throwaway repository root containing a registry and whatever manifests a test needs.
    fn tree(label: &str) -> PathBuf {
        let serial = NEXT_TREE.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "llm-registry-resilience-{label}-{}-{serial}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("conductor/mutation_campaigns")).expect("create tree");
        root
    }

    fn write(path: &Path, body: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent");
        }
        fs::write(path, body).expect("write file");
    }

    fn write_registry(root: &Path, campaigns: &str) {
        write(
            &root.join("conductor/mutation_campaigns/registry.json"),
            &format!(
                r#"{{"schema_version":1,"enforcement":"changed_tests",
                    "test_patterns":["**/test_*.py"],
                    "receipt_directories":["conductor/mutation_campaigns/receipts"],
                    "campaigns":{campaigns}}}"#
            ),
        );
    }

    fn registry_of(root: &Path) -> PathBuf {
        root.join("conductor/mutation_campaigns/registry.json")
    }

    #[test]
    fn absent_fragment_directory_is_not_an_error() {
        let root = tree("no-fragments");
        write_registry(
            &root,
            r#"[{"manifest":"conductor/mutation_campaigns/a.json"}]"#,
        );
        let fragments = load_registry_fragments(&registry_of(&root)).expect("fragments");
        assert!(fragments.is_empty());
    }

    #[test]
    fn fragments_register_campaigns_without_touching_the_shared_array() {
        let root = tree("fragments");
        write_registry(&root, "[]");
        // Two lanes each drop their own file; neither edits a file the other owns.
        write(
            &root.join("conductor/mutation_campaigns/registry.d/zeta.json"),
            r#"{"manifest":"conductor/mutation_campaigns/zeta.json"}"#,
        );
        write(
            &root.join("conductor/mutation_campaigns/registry.d/alpha.json"),
            r#"{"manifest":"conductor/mutation_campaigns/alpha.json"}"#,
        );
        let (_, manifests) =
            load_registry_manifest_paths(&root, &registry_of(&root), None).expect("registry");
        // Sorted by fragment filename so the order a tree yields is stable.
        assert_eq!(
            manifests,
            vec![
                "conductor/mutation_campaigns/alpha.json".to_owned(),
                "conductor/mutation_campaigns/zeta.json".to_owned(),
            ]
        );
    }

    #[test]
    fn a_campaign_in_both_the_array_and_a_fragment_is_loaded_once() {
        let root = tree("dedup");
        write_registry(
            &root,
            r#"[{"manifest":"conductor/mutation_campaigns/a.json"}]"#,
        );
        write(
            &root.join("conductor/mutation_campaigns/registry.d/a.json"),
            r#"{"manifest":"conductor/mutation_campaigns/a.json"}"#,
        );
        let (_, manifests) =
            load_registry_manifest_paths(&root, &registry_of(&root), None).expect("registry");
        assert_eq!(manifests, vec!["conductor/mutation_campaigns/a.json"]);
    }

    #[test]
    fn a_registry_declaring_no_campaigns_at_all_is_still_refused() {
        let root = tree("empty");
        write_registry(&root, "[]");
        let error = load_registry_manifest_paths(&root, &registry_of(&root), None)
            .expect_err("empty registry must be refused");
        assert!(error.contains("at least one campaign"), "{error}");
    }

    #[test]
    fn one_unloadable_manifest_does_not_sink_the_whole_registry() {
        // The regression: this returned Err, which the gate reported as a repository-wide
        // REFUSED, blocking every agent over one lane's half-written manifest.
        let root = tree("broken");
        write_registry(
            &root,
            r#"[{"manifest":"conductor/mutation_campaigns/broken.json"},
                {"manifest":"conductor/mutation_campaigns/absent.json"}]"#,
        );
        write(
            &root.join("conductor/mutation_campaigns/broken.json"),
            "{ not json",
        );
        let (loaded, broken) = load_campaigns(
            &root,
            &registry_of(&root),
            &["**/test_*.py".to_owned()],
            &HashMap::new(),
            &BTreeSet::new(),
        )
        .expect("a broken manifest must be reported, not propagated");
        assert!(loaded.is_empty());
        assert_eq!(broken.len(), 2);
        let manifests: Vec<&str> = broken.iter().map(|entry| entry.manifest.as_str()).collect();
        assert!(manifests.contains(&"conductor/mutation_campaigns/broken.json"));
        assert!(manifests.contains(&"conductor/mutation_campaigns/absent.json"));
        assert!(broken.iter().all(|entry| !entry.error.is_empty()));
    }
}

#[cfg(test)]
mod rust_inventory_tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::{inventory_rust_test_nodeids, rust_fn_name};

    static NEXT_TREE: AtomicU64 = AtomicU64::new(0);

    fn write_source(body: &str) -> (PathBuf, String) {
        let serial = NEXT_TREE.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "llm-rust-inventory-{}-{serial}",
            std::process::id()
        ));
        let relative = "src/subject.rs";
        fs::create_dir_all(root.join("src")).expect("create tree");
        fs::write(root.join(relative), body).expect("write source");
        (root, relative.to_owned())
    }

    #[test]
    fn inventory_lists_tests_in_source_order_and_skips_helpers() {
        let (root, relative) = write_source(
            "#[cfg(test)]\nmod inner {\n    fn helper() {}\n\n    #[test]\n    fn beta() {}\n\n    /// doc\n    #[test]\n    #[ignore]\n    // why this test exists\n    pub fn alpha() {}\n\n    #[tokio::test]\n\n    async fn gamma() {}\n}\n",
        );
        let found = inventory_rust_test_nodeids(&root, &relative).expect("inventory");
        assert_eq!(
            found,
            vec![
                format!("{relative}::beta"),
                format!("{relative}::alpha"),
                format!("{relative}::gamma"),
            ],
            "source order is the contract; helpers and doc comments are not tests"
        );
    }

    #[test]
    fn inventory_refuses_a_name_repeated_across_modules() {
        let (root, relative) = write_source(
            "mod a {\n    #[test]\n    fn same() {}\n}\nmod b {\n    #[test]\n    fn same() {}\n}\n",
        );
        let error = inventory_rust_test_nodeids(&root, &relative)
            .expect_err("cargo_test nodeids carry no module path, so this is ambiguous");
        assert!(error.contains("duplicate test name"), "got {error}");
    }

    #[test]
    fn inventory_refuses_a_file_with_no_tests() {
        let (root, relative) = write_source("fn not_a_test() {}\n");
        let error = inventory_rust_test_nodeids(&root, &relative)
            .expect_err("an empty complete scope must fail loud");
        assert!(
            error.contains("complete Rust test scope is empty"),
            "got {error}"
        );
    }

    #[test]
    fn inventory_refuses_a_dangling_test_attribute() {
        let (root, relative) = write_source("#[test]\n");
        let error = inventory_rust_test_nodeids(&root, &relative)
            .expect_err("an attribute with no fn is malformed, not empty");
        assert!(error.contains("has no fn"), "got {error}");
    }

    #[test]
    fn inventory_refuses_a_missing_file() {
        let (root, _) = write_source("#[test]\nfn a() {}\n");
        let error = inventory_rust_test_nodeids(&root, "src/absent.rs")
            .expect_err("a missing file must not read as an empty inventory");
        assert!(error.contains("cannot inventory Rust tests"), "got {error}");
    }

    #[test]
    fn inventory_reads_attributes_that_carry_arguments() {
        let (root, relative) = write_source(
            "#[tokio::test(flavor = \"multi_thread\")]\nasync fn with_args() {}\n\n#[tokio::test(\n    flavor = \"multi_thread\",\n    worker_threads = 2,\n)]\nasync fn across_lines() {}\n",
        );
        let found = inventory_rust_test_nodeids(&root, &relative).expect("inventory");
        assert_eq!(
            found,
            vec![
                format!("{relative}::with_args"),
                format!("{relative}::across_lines"),
            ],
            "an attribute path is the text before its arguments, on one line or many"
        );
    }

    #[test]
    fn inventory_refuses_a_test_framework_it_cannot_expand() {
        for attribute in ["#[rstest]", "#[test_case(1, 2)]", "#[proptest]"] {
            let (root, relative) = write_source(&format!(
                "#[test]\nfn real() {{}}\n\n{attribute}\nfn other() {{}}\n"
            ));
            let error = inventory_rust_test_nodeids(&root, &relative)
                .expect_err("guessing would silently undercount a complete scope");
            assert!(error.contains("cannot expand"), "{attribute}: got {error}");
        }
    }

    #[test]
    fn inventory_is_not_confused_by_test_shaped_non_markers() {
        let (root, relative) = write_source(
            "#[cfg(test)]\nmod inner {\n    #[cfg_attr(test, derive(Debug))]\n    struct S;\n\n    #[test]\n    #[should_panic(expected = \"boom [test]\")]\n    fn only_one() {}\n}\n",
        );
        let found = inventory_rust_test_nodeids(&root, &relative).expect("inventory");
        assert_eq!(
            found,
            vec![format!("{relative}::only_one")],
            "cfg(test), cfg_attr(test, ..) and a bracket inside a string are not test markers"
        );
    }

    #[test]
    fn attribute_reader_reports_the_path_and_its_last_line() {
        let lines = [
            "#[tokio::test(",
            "    flavor = \"x\",",
            ")]",
            "async fn a() {}",
        ];
        let (path, end) = super::read_rust_attribute(&lines, 0).expect("balanced attribute");
        assert_eq!(path, "tokio::test");
        assert_eq!(end, 2, "the fn is found after the attribute's last line");
        assert_eq!(super::read_rust_attribute(&["#[tokio::test("], 0), None);
    }

    #[test]
    fn attribute_classification_splits_markers_from_frameworks() {
        use super::{classify_rust_attribute, TestAttribute};
        assert_eq!(classify_rust_attribute("test"), TestAttribute::Marks);
        assert_eq!(classify_rust_attribute("tokio::test"), TestAttribute::Marks);
        assert_eq!(
            classify_rust_attribute("rstest"),
            TestAttribute::Unrecognised
        );
        assert_eq!(
            classify_rust_attribute("test_case"),
            TestAttribute::Unrecognised
        );
        assert_eq!(classify_rust_attribute("cfg"), TestAttribute::Other);
        assert_eq!(
            classify_rust_attribute("should_panic"),
            TestAttribute::Other
        );
        assert_eq!(
            classify_rust_attribute("serial_test::serial"),
            TestAttribute::Other
        );
    }

    #[test]
    fn fn_name_strips_visibility_and_qualifiers() {
        assert_eq!(rust_fn_name("fn plain() {}").as_deref(), Some("plain"));
        assert_eq!(
            rust_fn_name("pub fn exported() {}").as_deref(),
            Some("exported")
        );
        assert_eq!(
            rust_fn_name("pub(crate) async fn scoped() {}").as_deref(),
            Some("scoped")
        );
        assert_eq!(
            rust_fn_name("fn generic<T: Copy>(value: T) {}").as_deref(),
            Some("generic")
        );
        assert_eq!(rust_fn_name("let fn_like = 1;"), None);
        assert_eq!(rust_fn_name("struct NotAFn;"), None);
        // `fn ` must be the whole prefix, not merely present somewhere on the
        // line. Matching it by substring would name a function after text that
        // only mentions one, and the reader would then inventory a test that
        // does not exist and certify a complete scope around it.
        assert_eq!(rust_fn_name("let s = \"fn phantom\";"), None);
        assert_eq!(rust_fn_name("// call fn helper() later"), None);
    }
}

#[cfg(test)]
mod c_inventory_tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::{c_test_fn_name, inventory_c_test_nodeids};

    static NEXT_TREE: AtomicU64 = AtomicU64::new(0);

    fn write_source(body: &str) -> (PathBuf, String) {
        let serial = NEXT_TREE.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("llm-c-inventory-{}-{serial}", std::process::id()));
        let relative = "tests/test_subject.c";
        fs::create_dir_all(root.join("tests")).expect("create tree");
        fs::write(root.join(relative), body).expect("write source");
        (root, relative.to_owned())
    }

    #[test]
    fn inventory_lists_c_tests_in_source_order_and_skips_callbacks() {
        // The callback is the real shape from test_profiler.c: a `test_`
        // prefixed helper that ctest never registers, so counting it would make
        // a complete scope claim a case that cannot run.
        let (root, relative) = write_source(
            "#include <assert.h>\n\
             static void test_beta(void) { assert(1); }\n\
             static void test_sink_fn(const evt_t* e, void* u) { (void)e; (void)u; }\n\
             static void test_alpha(void) { assert(1); }\n\
             int main(void) { return 0; }\n",
        );
        let found = inventory_c_test_nodeids(&root, &relative).expect("inventory");
        assert_eq!(
            found,
            vec![
                "tests/test_subject.c::test_beta".to_owned(),
                "tests/test_subject.c::test_alpha".to_owned(),
            ]
        );
    }

    #[test]
    fn inventory_skips_a_forward_declaration() {
        // `static void test_x(void);` compiles and registers nothing.
        let (root, relative) = write_source(
            "static void test_later(void);\n\
             static void test_now(void) { }\n\
             static void test_later(void) { }\n",
        );
        let found = inventory_c_test_nodeids(&root, &relative).expect("inventory");
        assert_eq!(
            found,
            vec![
                "tests/test_subject.c::test_now".to_owned(),
                "tests/test_subject.c::test_later".to_owned(),
            ]
        );
    }

    #[test]
    fn inventory_ignores_an_indented_definition() {
        // The governance inventory and the CMake registration must read the
        // same set. CMake anchors at column zero; anything else is a case ctest
        // will not run, and claiming it in a complete scope gates nothing.
        let (root, relative) = write_source(
            "static void test_real(void) { }\n#if 0\n    static void test_hidden(void) { }\n#endif\n",
        );
        let found = inventory_c_test_nodeids(&root, &relative).expect("inventory");
        assert_eq!(found, vec!["tests/test_subject.c::test_real".to_owned()]);
    }

    #[test]
    fn inventory_refuses_a_duplicate_c_test_name() {
        let (root, relative) =
            write_source("static void test_reset(void) { }\nstatic void test_reset(void) { }\n");
        let error = inventory_c_test_nodeids(&root, &relative)
            .expect_err("ctest names are flat, so two cases would be indistinguishable");
        assert!(error.contains("duplicate test name"), "got {error}");
    }

    #[test]
    fn inventory_refuses_a_c_file_with_no_tests() {
        let (root, relative) = write_source("int main(void) { return 0; }\n");
        let error = inventory_c_test_nodeids(&root, &relative)
            .expect_err("an empty complete scope would gate nothing");
        assert!(
            error.contains("complete C test scope is empty"),
            "got {error}"
        );
    }

    #[test]
    fn inventory_refuses_a_missing_c_file() {
        let (root, _) = write_source("static void test_x(void) { }\n");
        let error = inventory_c_test_nodeids(&root, "tests/absent.c")
            .expect_err("a missing file must not read as an empty inventory");
        assert!(error.contains("cannot inventory C tests"), "got {error}");
    }

    #[test]
    fn fn_name_requires_the_void_signature_and_a_body() {
        assert_eq!(
            c_test_fn_name("static void test_ok(void) {"),
            Some("test_ok".to_owned())
        );
        assert_eq!(c_test_fn_name("static void test_ok(void);"), None);
        assert_eq!(c_test_fn_name("static void test_cb(int x) {"), None);
        assert_eq!(c_test_fn_name("static void helper(void) {"), None);
        assert_eq!(c_test_fn_name("void test_ok(void) {"), None);
    }
}

#[cfg(test)]
mod generated_campaign_tests {
    use serde_json::{json, Map, Value};

    use super::{generated_campaign_contract, CampaignContract};

    /// A manifest with two declared tests and a command that names both.
    fn payload(overrides: Value) -> Map<String, Value> {
        let mut base = json!({
            "schema_version": 1,
            "campaign_id": "subject_fest_20260906",
            "title": "Subject under fest",
            "language": "python",
            "mutation_engine": "fest",
            "generator": {
                "engine": "fest",
                "source": ["conductor/subject.py"],
                "run_timeout_seconds": 900,
                "jobs": 1
            },
            "source_sha256": {"conductor/subject.py": "a".repeat(64)},
            "test_sha256": {
                "conductor/test_subject.py": "b".repeat(64),
                "conductor/test_subject_extra.py": "c".repeat(64)
            },
            "test_argv": [
                "python", "-m", "pytest", "-q",
                "conductor/test_subject.py",
                "conductor/test_subject_extra.py"
            ],
            "survivor_baseline": ["constant_replace-abc123456789-0"]
        });
        let object = base.as_object_mut().expect("object");
        for (key, value) in overrides.as_object().expect("overrides object") {
            if value.is_null() {
                object.remove(key);
            } else {
                object.insert(key.clone(), value.clone());
            }
        }
        object.clone()
    }

    fn contract(overrides: Value) -> Result<CampaignContract, String> {
        generated_campaign_contract(
            &payload(overrides),
            b"manifest bytes".to_vec(),
            "conductor/mutation_campaigns/subject_fest_20260906.json",
            "fest",
        )
    }

    #[test]
    fn a_generated_manifest_declares_no_mutants_and_a_complete_scope() {
        let contract = contract(json!({})).expect("contract");
        assert!(contract.generated);
        // The engine decides how many mutants exist, so the manifest cannot promise a count.
        assert_eq!(contract.expected_mutations, 0);
        assert!(contract.planned_mutations.is_empty());
        assert!(contract.mutations.is_empty());
        assert_eq!(
            contract.ranked_test_paths,
            vec![
                "conductor/test_subject.py".to_owned(),
                "conductor/test_subject_extra.py".to_owned()
            ]
        );
        for scope in contract.test_scopes.values() {
            assert_eq!(scope.get("mode").and_then(Value::as_str), Some("complete"));
        }
        assert_eq!(
            contract.survivor_baseline,
            vec!["constant_replace-abc123456789-0".to_owned()]
        );
        assert!(contract.covers_test("conductor/test_subject.py"));
        assert!(!contract.covers_test("conductor/test_elsewhere.py"));
    }

    #[test]
    fn a_command_naming_some_declared_tests_but_not_all_is_refused() {
        let error = contract(json!({
            "test_argv": ["python", "-m", "pytest", "-q", "conductor/test_subject.py"]
        }))
        .expect_err("partial test_argv must be refused");
        assert!(
            error.contains("conductor/test_subject_extra.py"),
            "error should name the test left out: {error}"
        );
    }

    #[test]
    fn a_suite_runner_that_names_no_test_file_is_accepted() {
        // `cargo test` and `ctest` run everything and name nothing. A suite runner covers more
        // than it declares, never less, so declaring tests it does not spell out is honest.
        let contract = contract(json!({"test_argv": ["cargo", "test", "--release"]}))
            .expect("suite runner contract");
        assert_eq!(contract.test_argv, vec!["cargo", "test", "--release"]);
        assert_eq!(contract.ranked_test_paths.len(), 2);
    }

    #[test]
    fn a_duplicated_baseline_id_is_refused() {
        let error = contract(json!({
            "survivor_baseline": ["dup-000000000000-0", "dup-000000000000-0"]
        }))
        .expect_err("duplicate baseline ids must be refused");
        assert!(error.contains("duplicate"), "{error}");
    }

    #[test]
    fn a_manifest_without_test_digests_is_refused() {
        // Without these a receipt outlives the tests it describes.
        let error = contract(json!({"test_sha256": null})).expect_err("missing test_sha256");
        assert!(error.contains("test_sha256"), "{error}");
        let error = contract(json!({"test_sha256": {}})).expect_err("empty test_sha256");
        assert!(error.contains("test_sha256"), "{error}");
    }

    #[test]
    fn a_manifest_without_a_run_timeout_is_refused() {
        let error = contract(json!({
            "generator": {"engine": "fest", "source": ["conductor/subject.py"]}
        }))
        .expect_err("missing run_timeout_seconds");
        assert!(error.contains("run_timeout_seconds"), "{error}");
    }

    #[test]
    fn a_missing_survivor_baseline_is_refused_but_an_empty_one_is_not() {
        // An absent baseline is an unanswered question; an empty one is the answer "none yet".
        let error = contract(json!({"survivor_baseline": null})).expect_err("missing baseline");
        assert!(error.contains("survivor_baseline"), "{error}");
        let contract = contract(json!({"survivor_baseline": []})).expect("empty baseline");
        assert!(contract.survivor_baseline.is_empty());
    }
}
