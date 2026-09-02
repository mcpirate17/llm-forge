//! Receipt loading and immutable mutation-evidence validation.

use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use pyo3::prelude::*;
use pyo3::types::PyBytes;
use serde::Deserialize;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::mutation_manifest::{
    lexical_absolute, python_repr, safe_relative, CampaignContract, ValueContract,
};

const RECEIPT_SCHEMA: &str = "llm.mutation-testing.receipt.v3";
const LEGACY_RECEIPT_SCHEMA: &str = "llm.mutation-testing.receipt.v2";
const VALUE_SCHEMA: &str = "llm.mutation-testing.test-value.v1";
const ANCHOR_REGISTRY_PATH: &str = "conductor/mutation_campaigns/registry.json";

#[derive(Debug, Deserialize)]
pub(crate) struct RunnerState {
    pub(crate) components: Option<Value>,
    pub(crate) error: Option<String>,
    pub(crate) mutation_testing_sha256: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct AnchorConfig {
    pub(crate) repo: String,
    pub(crate) commit: String,
    pub(crate) tree: String,
    pub(crate) receipt_prefix: String,
}

pub(crate) struct Receipt {
    pub(crate) path: PathBuf,
    pub(crate) relative: String,
    pub(crate) name: String,
    pub(crate) value: Value,
    pub(crate) bytes: Vec<u8>,
    pub(crate) parsed_bytes_available: bool,
}

pub(crate) struct ValidationContext<'a> {
    pub(crate) repo_root: &'a Path,
    pub(crate) anchor_repo: &'a Path,
    pub(crate) runner: &'a RunnerState,
    pub(crate) anchor: &'a AnchorConfig,
}

#[derive(Debug, Deserialize)]
struct ReceiptValidationRequest {
    repo_root: String,
    anchor_repo: String,
    campaign: CampaignContract,
    receipt: Value,
    receipt_path: Option<String>,
    receipt_bytes: Option<Vec<u8>>,
    runner: RunnerState,
    anchor: AnchorConfig,
}

#[derive(Debug, Deserialize)]
struct LineageRequest {
    repo_root: String,
    recorded: Option<Value>,
}

fn exact_parse_error(py: Python<'_>, path: &Path, bytes: &[u8]) -> String {
    let raw = PyBytes::new(py, bytes);
    let decoded = match raw.call_method1("decode", ("utf-8",)) {
        Ok(decoded) => decoded,
        Err(error) => {
            return error
                .value(py)
                .str()
                .and_then(|text| text.extract::<String>())
                .unwrap_or_else(|_| error.to_string());
        }
    };
    let parsed = py
        .import("json")
        .and_then(|module| module.call_method1("loads", (&decoded,)));
    match parsed {
        Ok(value) => {
            if value.cast::<pyo3::types::PyDict>().is_ok() {
                "unknown receipt parse error".to_owned()
            } else {
                format!("receipt {} must be a JSON object", path.display())
            }
        }
        Err(error) => error
            .value(py)
            .str()
            .and_then(|text| text.extract::<String>())
            .unwrap_or_else(|_| error.to_string()),
    }
}

pub(crate) fn load_receipts(
    py: Python<'_>,
    repo_root: &Path,
    directories: &[String],
) -> Result<(Vec<Receipt>, Vec<String>), String> {
    let mut paths = Vec::new();
    for raw in directories {
        let relative = safe_relative(raw, "receipt directory")?;
        let directory = repo_root.join(relative);
        if !directory.is_dir() {
            continue;
        }
        let mut local: Vec<PathBuf> = fs::read_dir(&directory)
            .map_err(|error| {
                format!(
                    "cannot read receipt directory {}: {error}",
                    directory.display()
                )
            })?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|suffix| suffix == "json"))
            .collect();
        local.sort();
        paths.extend(local);
    }

    let mut receipts = Vec::with_capacity(paths.len());
    let mut malformed = Vec::new();
    for path in paths {
        let relative = path
            .strip_prefix(repo_root)
            .map_err(|_| "receipt path escapes repository".to_owned())?
            .to_string_lossy()
            .replace('\\', "/");
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) => {
                malformed.push(format!("{relative}: {error}"));
                continue;
            }
        };
        let value = match std::str::from_utf8(&bytes)
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(text).ok())
        {
            Some(Value::Object(object)) => Value::Object(object),
            _ => {
                malformed.push(format!(
                    "{relative}: {}",
                    exact_parse_error(py, &path, &bytes)
                ));
                continue;
            }
        };
        let name = path
            .file_name()
            .map(|value| value.to_string_lossy().into_owned())
            .unwrap_or_default();
        receipts.push(Receipt {
            path,
            relative,
            name,
            value,
            bytes,
            parsed_bytes_available: true,
        });
    }
    Ok((receipts, malformed))
}

fn git_bytes(repo: &Path, args: &[String]) -> Option<Output> {
    let mut command = Command::new("git");
    command
        .arg("--no-replace-objects")
        .args(args)
        .current_dir(repo);
    for (key, _) in env::vars_os().filter(|(key, _)| key.to_string_lossy().starts_with("GIT_")) {
        command.env_remove(key);
    }
    command.env("GIT_NO_REPLACE_OBJECTS", "1");
    command.output().ok()
}

fn successful(output: &Option<Output>) -> bool {
    output.as_ref().is_some_and(|value| value.status.success())
}

fn lexical_regular_file(path: &Path, root: &Path, label: &str) -> Result<String, String> {
    let root = lexical_absolute(root)?;
    let path = lexical_absolute(path)?;
    if fs::symlink_metadata(&root)
        .map(|metadata| metadata.file_type().is_symlink())
        .unwrap_or(false)
    {
        return Err(format!("{label} repository root is a symlink"));
    }
    let relative = path
        .strip_prefix(&root)
        .map_err(|_| format!("{label} path escapes the repository"))?;
    let mut current = root;
    for part in relative.components() {
        current.push(part.as_os_str());
        if fs::symlink_metadata(&current)
            .map(|metadata| metadata.file_type().is_symlink())
            .unwrap_or(false)
        {
            return Err(format!("{label} path has a symlink component"));
        }
    }
    if !path.is_file() {
        return Err(format!("{label} path is missing or unsafe"));
    }
    Ok(relative.to_string_lossy().replace('\\', "/"))
}

fn legacy_receipt_anchor_errors(
    receipt: &Receipt,
    repo_root: &Path,
    anchor_repo: &Path,
    anchor_commit: &str,
    anchor_tree: &str,
    receipt_prefix: &str,
    campaign: &CampaignContract,
) -> Vec<String> {
    if receipt.path.as_os_str().is_empty() || !receipt.parsed_bytes_available {
        return vec!["legacy receipt path or parsed bytes are unavailable".to_owned()];
    }
    let relative = match lexical_regular_file(&receipt.path, repo_root, "legacy receipt") {
        Ok(relative) => relative,
        Err(error) => return vec![error],
    };
    if !relative.starts_with(receipt_prefix) || !relative.ends_with(".json") {
        return vec!["legacy receipt path is outside the anchored receipt directory".to_owned()];
    }

    let top = git_bytes(
        anchor_repo,
        &["rev-parse".to_owned(), "--show-toplevel".to_owned()],
    );
    let expected_top = lexical_absolute(anchor_repo)
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_default();
    if !successful(&top)
        || String::from_utf8_lossy(&top.as_ref().expect("checked").stdout).trim() != expected_top
    {
        return vec!["legacy receipt anchor repository is unavailable".to_owned()];
    }
    let commit_type = git_bytes(
        anchor_repo,
        &[
            "cat-file".to_owned(),
            "-t".to_owned(),
            anchor_commit.to_owned(),
        ],
    );
    if !successful(&commit_type)
        || commit_type.as_ref().expect("checked").stdout.as_slice() != b"commit\n"
    {
        return vec!["legacy receipt anchor commit is unavailable".to_owned()];
    }
    let tree = git_bytes(
        anchor_repo,
        &["rev-parse".to_owned(), format!("{anchor_commit}^{{tree}}")],
    );
    if !successful(&tree)
        || String::from_utf8_lossy(&tree.as_ref().expect("checked").stdout).trim() != anchor_tree
    {
        return vec!["legacy receipt anchor tree mismatch".to_owned()];
    }

    let registry = git_bytes(
        anchor_repo,
        &[
            "cat-file".to_owned(),
            "blob".to_owned(),
            format!("{anchor_commit}:{ANCHOR_REGISTRY_PATH}"),
        ],
    );
    if !successful(&registry) {
        return vec!["legacy receipt anchor registry is unavailable".to_owned()];
    }
    let registered = registry
        .as_ref()
        .and_then(|output| serde_json::from_slice::<Value>(&output.stdout).ok())
        .and_then(|payload| payload.get("campaigns").and_then(Value::as_array).cloned())
        .map(|rows| {
            rows.iter().any(|row| {
                row.get("manifest").and_then(Value::as_str) == Some(campaign.manifest.as_str())
            })
        });
    let Some(registered) = registered else {
        return vec!["legacy receipt anchor registry is malformed".to_owned()];
    };
    if !registered {
        return vec!["legacy receipt campaign was not registered at the anchor".to_owned()];
    }
    let manifest = git_bytes(
        anchor_repo,
        &[
            "cat-file".to_owned(),
            "blob".to_owned(),
            format!("{anchor_commit}:{}", campaign.manifest),
        ],
    );
    if !successful(&manifest) {
        return vec!["legacy receipt anchor manifest is unavailable".to_owned()];
    }
    let digest = format!(
        "{:x}",
        Sha256::digest(&manifest.as_ref().expect("checked").stdout)
    );
    if digest != campaign.manifest_sha256 {
        return vec!["legacy receipt anchor manifest hash mismatch".to_owned()];
    }

    let entry = git_bytes(
        anchor_repo,
        &[
            "ls-tree".to_owned(),
            anchor_commit.to_owned(),
            "--".to_owned(),
            relative.clone(),
        ],
    );
    let expected_suffix = format!("\t{relative}\n");
    if !successful(&entry)
        || !entry
            .as_ref()
            .expect("checked")
            .stdout
            .starts_with(b"100644 blob ")
        || !entry
            .as_ref()
            .expect("checked")
            .stdout
            .ends_with(expected_suffix.as_bytes())
    {
        return vec!["legacy receipt is absent or unsafe at the anchor".to_owned()];
    }
    let blob = git_bytes(
        anchor_repo,
        &[
            "cat-file".to_owned(),
            "blob".to_owned(),
            format!("{anchor_commit}:{relative}"),
        ],
    );
    if !successful(&blob) || blob.as_ref().expect("checked").stdout != receipt.bytes {
        return vec!["legacy receipt parsed bytes differ from the anchor".to_owned()];
    }
    Vec::new()
}

fn lineage_accepts(recorded: Option<&Value>, repo_root: &Path) -> bool {
    let Some(Value::Object(_)) = recorded else {
        return false;
    };
    let path = repo_root.join("conductor/mutation_runner_lineage.json");
    let Ok(bytes) = fs::read(path) else {
        return false;
    };
    let Ok(Value::Object(payload)) = serde_json::from_slice::<Value>(&bytes) else {
        return false;
    };
    if payload.get("schema_version").and_then(Value::as_i64) != Some(1) {
        return false;
    }
    let Some(entries) = payload.get("entries").and_then(Value::as_array) else {
        return false;
    };
    for entry in entries {
        let Some(entry) = entry.as_object() else {
            continue;
        };
        if entry.get("runner_components_sha256") == recorded {
            return true;
        }
    }
    false
}

fn value_receipt_errors(value: Option<&Value>, contract: &ValueContract) -> Vec<String> {
    let Some(Value::Object(payload)) = value else {
        return vec!["test_value evidence is missing".to_owned()];
    };
    let mut errors = Vec::new();
    if payload.get("schema_version").and_then(Value::as_str) != Some(VALUE_SCHEMA) {
        errors.push("test_value schema is not current".to_owned());
    }
    if payload.get("status").and_then(Value::as_str) != Some("PASS") {
        errors.push(format!(
            "test_value status={}",
            python_repr(payload.get("status"))
        ));
    }
    let actual_nodeids: Vec<Value> = payload
        .get("tests")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(Value::as_object)
                .map(|row| row.get("nodeid").cloned().unwrap_or(Value::Null))
                .collect()
        })
        .unwrap_or_default();
    let expected_nodeids: Vec<Value> = contract
        .expected_nodeids
        .iter()
        .cloned()
        .map(Value::String)
        .collect();
    if actual_nodeids != expected_nodeids {
        errors.push("test_value nodeids do not match ranked tests".to_owned());
    }
    if payload.get("baseline_repetitions").and_then(Value::as_u64)
        != Some(contract.expected_repetitions)
    {
        errors.push("test_value baseline repetitions mismatch".to_owned());
    }
    errors
}

pub(crate) fn receipt_errors(
    receipt: &Receipt,
    campaign: &CampaignContract,
    context: &ValidationContext<'_>,
) -> Vec<String> {
    let payload = receipt
        .value
        .as_object()
        .expect("receipt loader requires object");
    let mut errors = Vec::new();
    let schema = payload.get("schema_version").and_then(Value::as_str);
    let anchored_legacy = schema == Some(LEGACY_RECEIPT_SCHEMA);
    if anchored_legacy {
        errors.extend(legacy_receipt_anchor_errors(
            receipt,
            context.repo_root,
            context.anchor_repo,
            &context.anchor.commit,
            &context.anchor.tree,
            &context.anchor.receipt_prefix,
            campaign,
        ));
    } else if schema != Some(RECEIPT_SCHEMA) {
        errors.push("receipt schema is not current".to_owned());
    }
    if payload.get("status").and_then(Value::as_str) != Some("PASS") {
        errors.push(format!("status={}", python_repr(payload.get("status"))));
    }
    if payload.get("campaign_id").and_then(Value::as_str) != Some(&campaign.campaign_id) {
        errors.push("campaign_id mismatch".to_owned());
    }
    if payload.get("manifest").and_then(Value::as_str) != Some(&campaign.manifest) {
        errors.push("manifest path mismatch".to_owned());
    }
    if payload.get("manifest_sha256").and_then(Value::as_str) != Some(&campaign.manifest_sha256) {
        errors.push("manifest hash mismatch".to_owned());
    }
    if !anchored_legacy {
        if let Some(error) = &context.runner.error {
            errors.push(error.clone());
        } else if let Some(components) = &context.runner.components {
            let recorded = payload.get("runner_components_sha256");
            if recorded != Some(components) && !lineage_accepts(recorded, context.repo_root) {
                errors.push("runner component hash map mismatch".to_owned());
            } else if recorded == Some(components)
                && payload.get("runner_sha256").and_then(Value::as_str)
                    != context.runner.mutation_testing_sha256.as_deref()
            {
                errors.push("runner hash mismatch".to_owned());
            }
        }
    }
    if payload.get("source_sha256") != Some(&campaign.source_sha256) {
        errors.push("source hash map mismatch".to_owned());
    }
    if payload
        .get("source_symbols")
        .unwrap_or(&Value::Object(Map::new()))
        != &campaign.source_symbols
    {
        errors.push("source symbol map mismatch".to_owned());
    }
    if payload
        .get("test_scopes")
        .unwrap_or(&Value::Object(Map::new()))
        != &Value::Object(campaign.test_scopes.clone())
    {
        errors.push("test scope map mismatch".to_owned());
    }
    if payload.get("complete_campaign") != Some(&Value::Bool(true)) {
        errors.push("partial campaign receipt".to_owned());
    }
    let expected_ids: Vec<Value> = campaign
        .mutations
        .iter()
        .map(|mutation| Value::String(mutation.id.clone()))
        .collect();
    if payload.get("selected_mutations").and_then(Value::as_array) != Some(&expected_ids) {
        errors.push("selected mutation ids mismatch".to_owned());
    }
    match payload.get("mutants").and_then(Value::as_array) {
        None => errors.push("mutants must be a list".to_owned()),
        Some(rows) => {
            let mut ordered_ids = Vec::new();
            let mut actual: HashMap<String, &Map<String, Value>> = HashMap::new();
            for row in rows.iter().filter_map(Value::as_object) {
                if let Some(id) = row.get("id").and_then(Value::as_str) {
                    if !actual.contains_key(id) {
                        ordered_ids.push(Value::String(id.to_owned()));
                    }
                    actual.insert(id.to_owned(), row);
                }
            }
            if ordered_ids != expected_ids {
                errors.push("mutant result ids mismatch".to_owned());
            }
            for mutation in &campaign.mutations {
                let row = actual.get(&mutation.id);
                if row
                    .and_then(|row| row.get("outcome"))
                    .and_then(Value::as_str)
                    != Some("KILLED")
                {
                    errors.push(format!("mutant {} was not killed", mutation.id));
                }
                if row
                    .and_then(|row| row.get("patch_sha256"))
                    .and_then(Value::as_str)
                    != Some(mutation.patch_sha256.as_str())
                {
                    errors.push(format!("mutant {} patch hash mismatch", mutation.id));
                }
            }
        }
    }
    if payload.get("mutation_score").and_then(Value::as_f64) != Some(1.0) {
        errors.push("mutation score is not 1.0".to_owned());
    }
    if let Some(contract) = &campaign.value_analysis {
        errors.extend(value_receipt_errors(payload.get("test_value"), contract));
    }
    if campaign.source_drifted {
        errors.push("current source hashes drifted".to_owned());
    }
    errors
}

fn scope_error(campaign: &CampaignContract, test_path: &str) -> Option<String> {
    let scope = campaign.test_scopes.get(test_path)?.as_object();
    match scope {
        None => Some("campaign lacks explicit test scope".to_owned()),
        Some(scope) => match scope.get("mode").and_then(Value::as_str) {
            Some("complete") => None,
            Some(mode) => Some(format!(
                "test scope is '{}', not 'complete'",
                mode.replace('\'', "\\'")
            )),
            None => Some("test scope is None, not 'complete'".to_owned()),
        },
    }
}

pub(crate) fn campaign_scope_error(campaign: &CampaignContract, test_path: &str) -> Option<String> {
    if !campaign.test_scopes.contains_key(test_path) {
        return Some("campaign lacks explicit test scope".to_owned());
    }
    scope_error(campaign, test_path)
}

fn value_error(message: impl Into<String>) -> PyErr {
    pyo3::exceptions::PyValueError::new_err(message.into())
}

fn parse_json<T: for<'de> Deserialize<'de>>(text: &str, label: &str) -> PyResult<T> {
    serde_json::from_str(text).map_err(|error| value_error(format!("invalid {label}: {error}")))
}

#[pyfunction]
pub fn validate_mutation_receipt_native(request_json: &str) -> PyResult<String> {
    let request: ReceiptValidationRequest = parse_json(request_json, "receipt request")?;
    let root = lexical_absolute(Path::new(&request.repo_root)).map_err(value_error)?;
    let anchor_repo = lexical_absolute(Path::new(&request.anchor_repo)).map_err(value_error)?;
    let raw_path = request.receipt_path.unwrap_or_default();
    let path = if raw_path.is_empty() {
        PathBuf::new()
    } else {
        let value = PathBuf::from(raw_path);
        if value.is_absolute() {
            value
        } else {
            root.join(value)
        }
    };
    let relative = path
        .strip_prefix(&root)
        .unwrap_or(&path)
        .to_string_lossy()
        .replace('\\', "/");
    let name = path
        .file_name()
        .map(|value| value.to_string_lossy().into_owned())
        .unwrap_or_default();
    let parsed_bytes_available = request.receipt_bytes.is_some();
    let bytes = match request.receipt_bytes {
        Some(bytes) => bytes,
        None => {
            serde_json::to_vec(&request.receipt).map_err(|error| value_error(error.to_string()))?
        }
    };
    let receipt = Receipt {
        path,
        relative,
        name,
        value: request.receipt,
        bytes,
        parsed_bytes_available,
    };
    let context = ValidationContext {
        repo_root: &root,
        anchor_repo: &anchor_repo,
        runner: &request.runner,
        anchor: &request.anchor,
    };
    serde_json::to_string(&receipt_errors(&receipt, &request.campaign, &context))
        .map_err(|error| value_error(error.to_string()))
}

#[pyfunction]
pub fn mutation_runner_lineage_accepts_native(request_json: &str) -> PyResult<bool> {
    let request: LineageRequest = parse_json(request_json, "lineage request")?;
    let root = lexical_absolute(Path::new(&request.repo_root)).map_err(value_error)?;
    Ok(lineage_accepts(request.recorded.as_ref(), &root))
}

pub(crate) fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(validate_mutation_receipt_native, module)?)?;
    module.add_function(wrap_pyfunction!(
        mutation_runner_lineage_accepts_native,
        module
    )?)?;
    Ok(())
}
