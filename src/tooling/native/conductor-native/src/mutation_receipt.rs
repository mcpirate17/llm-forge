//! Receipt loading and immutable mutation-evidence validation.

use std::collections::{BTreeSet, HashMap};
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
use crate::receipt_slim::expand_receipt;

const RECEIPT_SCHEMA: &str = "llm.mutation-testing.receipt.v3";
const LEGACY_RECEIPT_SCHEMA: &str = "llm.mutation-testing.receipt.v2";
const VALUE_SCHEMA: &str = "llm.mutation-testing.test-value.v1";

/// Why a receipt (or a campaign around it) cannot be evidence, named where the
/// failure is produced rather than parsed back out of a message.
///
/// The split that matters to callers: `not_pass`/`no_campaign`/`scope_error`/
/// `superseded`/`runner_map_mismatch` are debt -- a campaign that has not run,
/// a ratchet held, a receipt from an older runner era -- while `decode_error`,
/// `schema_error` and `manifest_load_error` mean the validator could not read
/// what sits on disk, which is a defect. The producers below push the kind at
/// the site that knows the rule; the evidence layer maps its own campaign-level
/// failures onto the same enum.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RejectionKind {
    NoCampaign,
    ScopeError,
    NotPass,
    Superseded,
    RunnerMapMismatch,
    DecodeError,
    SchemaError,
    ManifestLoadError,
}

impl RejectionKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            RejectionKind::NoCampaign => "no_campaign",
            RejectionKind::ScopeError => "scope_error",
            RejectionKind::NotPass => "not_pass",
            RejectionKind::Superseded => "superseded",
            RejectionKind::RunnerMapMismatch => "runner_map_mismatch",
            RejectionKind::DecodeError => "decode_error",
            RejectionKind::SchemaError => "schema_error",
            RejectionKind::ManifestLoadError => "manifest_load_error",
        }
    }
}

/// One classified rejection. `detail` keeps the exact free text the rules
/// always produced, so nothing that reads the old message breaks.
#[derive(Clone, Debug)]
pub(crate) struct Rejection {
    pub(crate) kind: RejectionKind,
    pub(crate) detail: String,
}

fn reject(kind: RejectionKind, detail: impl Into<String>) -> Rejection {
    Rejection {
        kind,
        detail: detail.into(),
    }
}

/// The old `Vec<String>` shape, for the seams that still speak it
/// (`validate_mutation_receipt_native`'s stable output, and tests asserting on
/// message text).
pub(crate) fn rejection_details(errors: Vec<Rejection>) -> Vec<String> {
    errors.into_iter().map(|error| error.detail).collect()
}
/// The registry as it stood in the anchor commit. A host may keep its registry
/// elsewhere today, so Python names the anchored spelling in the request; this is
/// the value the monorepo anchor was written against.
const ANCHOR_REGISTRY_PATH: &str = "conductor/mutation_campaigns/registry.json";

fn default_anchor_registry_path() -> String {
    ANCHOR_REGISTRY_PATH.to_owned()
}

fn sha256_file(path: &Path) -> Option<String> {
    fs::read(path)
        .ok()
        .map(|bytes| format!("{:x}", Sha256::digest(bytes)))
}

#[derive(Debug, Deserialize)]
pub(crate) struct RunnerState {
    pub(crate) components: Option<Value>,
    pub(crate) error: Option<String>,
    pub(crate) mutation_testing_sha256: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct AnchorConfig {
    pub(crate) repo: String,
    pub(crate) commit: String,
    pub(crate) tree: String,
    pub(crate) receipt_prefix: String,
    #[serde(default = "default_anchor_registry_path")]
    pub(crate) registry_path: String,
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
    /// Where a bare `conductor/...` runner-component literal resolves from -- the
    /// package's parent directory, per this tree's own layout configuration. Distinct
    /// from `repo_root`: a manifest path or receipt location is spelled relative to the
    /// true repo root even under a src layout, but `core_sha256`, `scope_guard_sha256`,
    /// `adapter_sha256` and the runner lineage file are spelled `conductor/...` and must
    /// join onto the package's parent instead.
    pub(crate) package_root: &'a Path,
    pub(crate) anchor_repo: &'a Path,
    pub(crate) runner: &'a RunnerState,
    pub(crate) anchor: &'a AnchorConfig,
}

#[derive(Debug, Deserialize)]
struct ReceiptValidationRequest {
    repo_root: String,
    package_root: String,
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
    package_root: String,
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
    anchor: &AnchorConfig,
    campaign: &CampaignContract,
) -> Vec<Rejection> {
    // Every anchor rule is a provenance check on the receipt itself, so every
    // failure here is the validator refusing the shape -- schema_error.
    if receipt.path.as_os_str().is_empty() || !receipt.parsed_bytes_available {
        return vec![reject(
            RejectionKind::SchemaError,
            "legacy receipt path or parsed bytes are unavailable",
        )];
    }
    let relative = match lexical_regular_file(&receipt.path, repo_root, "legacy receipt") {
        Ok(relative) => relative,
        Err(error) => {
            return vec![reject(RejectionKind::SchemaError, error)];
        }
    };
    if !relative.starts_with(&anchor.receipt_prefix) || !relative.ends_with(".json") {
        return vec![reject(
            RejectionKind::SchemaError,
            "legacy receipt path is outside the anchored receipt directory",
        )];
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
        return vec![reject(
            RejectionKind::SchemaError,
            "legacy receipt anchor repository is unavailable",
        )];
    }
    let commit_type = git_bytes(
        anchor_repo,
        &[
            "cat-file".to_owned(),
            "-t".to_owned(),
            anchor.commit.clone(),
        ],
    );
    if !successful(&commit_type)
        || commit_type.as_ref().expect("checked").stdout.as_slice() != b"commit\n"
    {
        return vec![reject(
            RejectionKind::SchemaError,
            "legacy receipt anchor commit is unavailable",
        )];
    }
    let tree = git_bytes(
        anchor_repo,
        &[
            "rev-parse".to_owned(),
            format!("{}^{{tree}}", anchor.commit),
        ],
    );
    if !successful(&tree)
        || String::from_utf8_lossy(&tree.as_ref().expect("checked").stdout).trim() != anchor.tree
    {
        return vec![reject(
            RejectionKind::SchemaError,
            "legacy receipt anchor tree mismatch",
        )];
    }

    let registry = git_bytes(
        anchor_repo,
        &[
            "cat-file".to_owned(),
            "blob".to_owned(),
            format!("{}:{}", anchor.commit, anchor.registry_path),
        ],
    );
    if !successful(&registry) {
        return vec![reject(
            RejectionKind::SchemaError,
            "legacy receipt anchor registry is unavailable",
        )];
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
        return vec![reject(
            RejectionKind::SchemaError,
            "legacy receipt anchor registry is malformed",
        )];
    };
    if !registered {
        return vec![reject(
            RejectionKind::SchemaError,
            "legacy receipt campaign was not registered at the anchor",
        )];
    }
    let manifest = git_bytes(
        anchor_repo,
        &[
            "cat-file".to_owned(),
            "blob".to_owned(),
            format!("{}:{}", anchor.commit, campaign.manifest),
        ],
    );
    if !successful(&manifest) {
        return vec![reject(
            RejectionKind::SchemaError,
            "legacy receipt anchor manifest is unavailable",
        )];
    }
    let digest = format!(
        "{:x}",
        Sha256::digest(&manifest.as_ref().expect("checked").stdout)
    );
    if digest != campaign.manifest_sha256 {
        return vec![reject(
            RejectionKind::SchemaError,
            "legacy receipt anchor manifest hash mismatch",
        )];
    }

    let entry = git_bytes(
        anchor_repo,
        &[
            "ls-tree".to_owned(),
            anchor.commit.clone(),
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
        return vec![reject(
            RejectionKind::SchemaError,
            "legacy receipt is absent or unsafe at the anchor",
        )];
    }
    let blob = git_bytes(
        anchor_repo,
        &[
            "cat-file".to_owned(),
            "blob".to_owned(),
            format!("{}:{relative}", anchor.commit),
        ],
    );
    if !successful(&blob) || blob.as_ref().expect("checked").stdout != receipt.bytes {
        return vec![reject(
            RejectionKind::SchemaError,
            "legacy receipt parsed bytes differ from the anchor",
        )];
    }
    Vec::new()
}

fn lineage_accepts(recorded: Option<&Value>, package_root: &Path) -> bool {
    let Some(Value::Object(_)) = recorded else {
        return false;
    };
    let path = package_root.join("conductor/mutation_runner_lineage.json");
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

fn value_receipt_errors(value: Option<&Value>, contract: &ValueContract) -> Vec<Rejection> {
    let Some(Value::Object(payload)) = value else {
        return vec![reject(
            RejectionKind::SchemaError,
            "test_value evidence is missing",
        )];
    };
    let mut errors = Vec::new();
    if payload.get("schema_version").and_then(Value::as_str) != Some(VALUE_SCHEMA) {
        errors.push(reject(
            RejectionKind::SchemaError,
            "test_value schema is not current",
        ));
    }
    if payload.get("status").and_then(Value::as_str) != Some("PASS") {
        errors.push(reject(
            RejectionKind::NotPass,
            format!("test_value status={}", python_repr(payload.get("status"))),
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
        errors.push(reject(
            RejectionKind::SchemaError,
            "test_value nodeids do not match ranked tests",
        ));
    }
    if payload.get("baseline_repetitions").and_then(Value::as_u64)
        != Some(contract.expected_repetitions)
    {
        errors.push(reject(
            RejectionKind::SchemaError,
            "test_value baseline repetitions mismatch",
        ));
    }
    errors
}

/// Check the runner that produced a receipt is the runner on disk, or a recorded ancestor.
///
/// Shared by both acceptance rules: whichever kind of campaign a receipt belongs to, evidence
/// produced by a runner nobody can reconstruct is not evidence. Every failure here is the
/// same debt class: the receipt pins a runner era that is neither current nor recorded,
/// so the campaign needs a re-run, not a repair.
fn runner_errors(payload: &Map<String, Value>, context: &ValidationContext<'_>) -> Vec<Rejection> {
    let mut errors = Vec::new();
    if let Some(error) = &context.runner.error {
        errors.push(reject(RejectionKind::RunnerMapMismatch, error.clone()));
    } else if let Some(components) = &context.runner.components {
        let recorded = payload.get("runner_components_sha256");
        if recorded != Some(components) && !lineage_accepts(recorded, context.package_root) {
            errors.push(reject(
                RejectionKind::RunnerMapMismatch,
                "runner component hash map mismatch",
            ));
        } else if recorded == Some(components)
            && payload.get("runner_sha256").and_then(Value::as_str)
                != context.runner.mutation_testing_sha256.as_deref()
        {
            errors.push(reject(
                RejectionKind::RunnerMapMismatch,
                "runner hash mismatch",
            ));
        }
    }
    errors
}

/// Whether a generated campaign's receipt is current, complete and within its ratchet.
///
/// The patch-based rule this replaces demands `mutation_score == 1.0` and a `KILLED` outcome for
/// every mutant the manifest names. Applied to a generated corpus that rule is unsatisfiable, and
/// the shape of the old evidence shows what it cost: 482 of 483 scored campaigns in this
/// repository publish exactly 1.0, because a campaign that scored anything else could not land.
/// The only way to hit the target was to choose the mutants, and choosing them is what made the
/// number meaningless.
///
/// So the question asked here is different, and not a weaker one. The engine picks the mutants,
/// the manifest records which of them are already known to survive, and a run passes only when it
/// introduces no survivor outside that set. A percentage can be held flat while a real gap opens,
/// because closing one mutant pays for losing another; a survivor set cannot. It also means a
/// campaign can land honestly at 0.63 and still refuse the next commit that makes the suite worse,
/// which is the whole point of measuring in the first place.
fn generated_receipt_errors(
    payload: &Map<String, Value>,
    campaign: &CampaignContract,
    context: &ValidationContext<'_>,
) -> Vec<Rejection> {
    let mut errors = Vec::new();
    if payload.get("schema_version").and_then(Value::as_str) != Some(RECEIPT_SCHEMA) {
        errors.push(reject(
            RejectionKind::SchemaError,
            "receipt schema is not current",
        ));
    }
    // A patch receipt carries the same campaign fields, so without this a hand-authored receipt
    // could be offered as evidence for a generated campaign and skip the ratchet entirely.
    if payload.get("mutants_are_generated") != Some(&Value::Bool(true)) {
        errors.push(reject(
            RejectionKind::SchemaError,
            "receipt does not record engine-generated mutants",
        ));
    }
    match payload.get("status").and_then(Value::as_str) {
        Some("PASS") => {}
        other => {
            // A receipt that did not pass is not evidence for one reason: it
            // did not pass. RATCHET_HELD lands here too, by design -- a held
            // ratchet is honest debt to re-run, not a receipt the validator
            // cannot read. Everything below (survivor rows, score, outcome
            // accounting) presumes a PASS claim; judged on a held run it
            // fragments one debt verdict into a wall of defect-class noise,
            // which is exactly what the kinds exist to prevent. The two shape
            // checks above still fire alongside: a wrong schema or a
            // hand-authored patch offered to a generated campaign is a broken
            // receipt whichever way its status reads.
            return vec![reject(
                RejectionKind::NotPass,
                format!("status={}", python_repr(other.map(Value::from).as_ref())),
            )];
        }
    }
    if payload.get("campaign_id").and_then(Value::as_str) != Some(&campaign.campaign_id) {
        errors.push(reject(RejectionKind::SchemaError, "campaign_id mismatch"));
    }
    if payload.get("manifest").and_then(Value::as_str) != Some(&campaign.manifest) {
        errors.push(reject(RejectionKind::SchemaError, "manifest path mismatch"));
    }
    if payload.get("manifest_sha256").and_then(Value::as_str) != Some(&campaign.manifest_sha256) {
        errors.push(reject(RejectionKind::SchemaError, "manifest hash mismatch"));
    }
    if payload.get("source_sha256") != Some(&campaign.source_sha256) {
        errors.push(reject(
            RejectionKind::SchemaError,
            "source hash map mismatch",
        ));
    }
    if payload.get("test_sha256") != Some(&campaign.test_sha256) {
        errors.push(reject(RejectionKind::SchemaError, "test hash map mismatch"));
    }
    if campaign.source_drifted {
        errors.push(reject(
            // A stale PASS is not a current PASS -- the missing-evidence reason
            // for a drifted campaign is literally "no current complete PASS
            // receipt", so the debt class is not_pass: the campaign re-runs.
            RejectionKind::NotPass,
            "current source or test hashes drifted",
        ));
    }
    errors.extend(runner_errors(payload, context));
    // The core, adapter and scope guard all affect what a generated campaign measures, and
    // live outside the generic runner component map. Presence alone is insufficient: an old
    // receipt must not survive an edit to any of these files. Their failures share the
    // runner map's debt class: the receipt was written by another era of the runner, and
    // the campaign re-runs -- nothing here says the validator misread the file.
    let bindings = [
        ("core_sha256", "conductor/mutation_engine_generated.py"),
        ("scope_guard_sha256", "conductor/mutation_run_scope.py"),
    ];
    for (key, relative) in bindings {
        let recorded = payload.get(key).and_then(Value::as_str);
        let current = sha256_file(&context.package_root.join(relative));
        if recorded.is_none_or(|value| value.trim().is_empty()) {
            errors.push(reject(
                RejectionKind::RunnerMapMismatch,
                format!("{key} is missing"),
            ));
        } else if current.as_deref() != recorded {
            errors.push(reject(
                RejectionKind::RunnerMapMismatch,
                format!("{key} hash mismatch"),
            ));
        }
    }
    let adapter_path = match campaign.mutation_engine.as_str() {
        "cargo-mutants" => Some("conductor/mutation_engine_cargo.py"),
        "fest" => Some("conductor/mutation_engine_fest.py"),
        "mull" => Some("conductor/mutation_engine_mull.py"),
        _ => None,
    };
    let recorded_adapter = payload.get("adapter_sha256").and_then(Value::as_str);
    match adapter_path {
        Some(relative) => {
            let current_adapter = sha256_file(&context.package_root.join(relative));
            if recorded_adapter.is_none_or(|value| value.trim().is_empty()) {
                errors.push(reject(
                    RejectionKind::RunnerMapMismatch,
                    "adapter_sha256 is missing",
                ));
            } else if current_adapter.as_deref() != recorded_adapter {
                errors.push(reject(
                    RejectionKind::RunnerMapMismatch,
                    "adapter_sha256 hash mismatch",
                ));
            }
        }
        None => errors.push(reject(
            RejectionKind::SchemaError,
            format!(
                "unsupported generated mutation engine: {}",
                campaign.mutation_engine
            ),
        )),
    }
    match payload.get("survivors").and_then(Value::as_array) {
        None => errors.push(reject(
            RejectionKind::SchemaError,
            "survivors must be a list",
        )),
        Some(rows) => {
            if rows.iter().any(|row| !row.is_string()) {
                errors.push(reject(
                    RejectionKind::SchemaError,
                    "survivors must contain only mutation IDs",
                ));
            }
            if !rows.is_empty() {
                errors.push(reject(
                    RejectionKind::SchemaError,
                    "PASS receipt reports surviving mutants",
                ));
            }
            let baseline: BTreeSet<&str> = campaign
                .survivor_baseline
                .iter()
                .map(String::as_str)
                .collect();
            let mut escaped: Vec<&str> = rows
                .iter()
                .filter_map(Value::as_str)
                .filter(|id| !baseline.contains(id))
                .collect();
            escaped.sort_unstable();
            escaped.dedup();
            if !escaped.is_empty() {
                // A survivor outside the baseline contradicts the PASS the
                // receipt claims -- the validator refuses the claim. It is not
                // `not_pass` debt: status says PASS, and CI must not wave a
                // genuinely regressed ratchet through as re-runnable debt.
                errors.push(reject(
                    RejectionKind::SchemaError,
                    format!(
                        "{} survivor(s) outside the recorded baseline: {}",
                        escaped.len(),
                        escaped.join(", ")
                    ),
                ));
            }
        }
    }
    let baseline_ok = payload
        .get("baseline")
        .and_then(Value::as_object)
        .is_some_and(|baseline| {
            baseline.get("returncode").and_then(Value::as_i64) == Some(0)
                && baseline.get("timed_out") == Some(&Value::Bool(false))
        });
    if !baseline_ok {
        errors.push(reject(
            RejectionKind::SchemaError,
            "baseline must pass without timeout",
        ));
    }
    let Some(mutants) = payload.get("mutants").and_then(Value::as_array) else {
        errors.push(reject(
            RejectionKind::SchemaError,
            "mutants must be a non-empty list",
        ));
        return errors;
    };
    let mut ids = BTreeSet::new();
    let mut counts = std::collections::BTreeMap::new();
    for mutant in mutants {
        let id = mutant.get("id").and_then(Value::as_str);
        let outcome = mutant.get("outcome").and_then(Value::as_str);
        if id.is_none_or(str::is_empty) || !ids.insert(id.unwrap_or_default()) {
            errors.push(reject(
                RejectionKind::SchemaError,
                "mutants must have unique non-empty IDs",
            ));
        }
        match outcome {
            // TIMED_OUT rows score as neither killed nor surviving: the engine
            // cut the mutant off at the bound the campaign asked for, so the
            // run measured it as unknown rather than failed. It is counted and
            // cross-checked through `outcome_counts` like every other
            // non-scoring outcome.
            Some(outcome @ ("KILLED" | "NO_COVERAGE" | "UNVIABLE" | "TIMED_OUT")) => {
                *counts.entry(outcome).or_insert(0usize) += 1
            }
            _ => errors.push(reject(
                RejectionKind::SchemaError,
                "PASS receipt has an invalid mutant outcome",
            )),
        }
    }
    if !counts.contains_key("KILLED") {
        errors.push(reject(
            RejectionKind::SchemaError,
            "PASS receipt requires at least one killed mutant",
        ));
    }
    let declared = payload.get("outcome_counts").and_then(Value::as_object);
    for outcome in [
        "KILLED",
        "NO_COVERAGE",
        "UNVIABLE",
        "ERROR",
        "SURVIVED",
        "TIMED_OUT",
    ] {
        if declared
            .and_then(|rows| rows.get(outcome))
            .and_then(Value::as_u64)
            != Some(*counts.get(outcome).unwrap_or(&0) as u64)
        {
            errors.push(reject(
                RejectionKind::SchemaError,
                "outcome_counts do not match mutant rows",
            ));
            break;
        }
    }
    if payload.get("mutation_score").and_then(Value::as_f64) != Some(1.0) {
        errors.push(reject(
            RejectionKind::SchemaError,
            "mutation score is not 1.0",
        ));
    }
    errors
}

/// The reviewed rule's mutant cross-check: the receipt's result rows are the
/// manifest's chosen mutations, in order, each killed with its recorded patch
/// digest.
fn reviewed_mutant_errors(
    payload: &Map<String, Value>,
    campaign: &CampaignContract,
) -> Vec<Rejection> {
    // Every row cross-check is the manifest's chosen mutations being refused as
    // evidence: a mutant that survived contradicts the complete-PASS the
    // receipt claims, so it is a rejected claim (schema_error), not debt.
    let mut errors = Vec::new();
    let expected_ids: Vec<Value> = campaign
        .mutations
        .iter()
        .map(|mutation| Value::String(mutation.id.clone()))
        .collect();
    if payload.get("selected_mutations").and_then(Value::as_array) != Some(&expected_ids) {
        errors.push(reject(
            RejectionKind::SchemaError,
            "selected mutation ids mismatch",
        ));
    }
    match payload.get("mutants").and_then(Value::as_array) {
        None => errors.push(reject(RejectionKind::SchemaError, "mutants must be a list")),
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
                errors.push(reject(
                    RejectionKind::SchemaError,
                    "mutant result ids mismatch",
                ));
            }
            for mutation in &campaign.mutations {
                let row = actual.get(&mutation.id);
                if row
                    .and_then(|row| row.get("outcome"))
                    .and_then(Value::as_str)
                    != Some("KILLED")
                {
                    errors.push(reject(
                        RejectionKind::SchemaError,
                        format!("mutant {} was not killed", mutation.id),
                    ));
                }
                if row
                    .and_then(|row| row.get("patch_sha256"))
                    .and_then(Value::as_str)
                    != Some(mutation.patch_sha256.as_str())
                {
                    errors.push(reject(
                        RejectionKind::SchemaError,
                        format!("mutant {} patch hash mismatch", mutation.id),
                    ));
                }
            }
        }
    }
    errors
}

/// The reviewed-campaign rule (hand-authored mutations): every summary field
/// agrees with the manifest and every named mutation is killed.
fn reviewed_receipt_errors(
    receipt: &Receipt,
    payload: &Map<String, Value>,
    campaign: &CampaignContract,
    context: &ValidationContext<'_>,
) -> Vec<Rejection> {
    let mut errors = Vec::new();
    let schema = payload.get("schema_version").and_then(Value::as_str);
    let anchored_legacy = schema == Some(LEGACY_RECEIPT_SCHEMA);
    if anchored_legacy {
        errors.extend(legacy_receipt_anchor_errors(
            receipt,
            context.repo_root,
            context.anchor_repo,
            context.anchor,
            campaign,
        ));
    } else if schema != Some(RECEIPT_SCHEMA) {
        errors.push(reject(
            RejectionKind::SchemaError,
            "receipt schema is not current",
        ));
    }
    if payload.get("status").and_then(Value::as_str) != Some("PASS") {
        // Same verdict shape as the generated rule: a run that did not pass is
        // rejected as not passing, once, without the PASS-rule noise a held or
        // failed run would otherwise accumulate.
        return vec![reject(
            RejectionKind::NotPass,
            format!("status={}", python_repr(payload.get("status"))),
        )];
    }
    if payload.get("campaign_id").and_then(Value::as_str) != Some(&campaign.campaign_id) {
        errors.push(reject(RejectionKind::SchemaError, "campaign_id mismatch"));
    }
    if payload.get("manifest").and_then(Value::as_str) != Some(&campaign.manifest) {
        errors.push(reject(RejectionKind::SchemaError, "manifest path mismatch"));
    }
    if payload.get("manifest_sha256").and_then(Value::as_str) != Some(&campaign.manifest_sha256) {
        errors.push(reject(RejectionKind::SchemaError, "manifest hash mismatch"));
    }
    if !anchored_legacy {
        errors.extend(runner_errors(payload, context));
    }
    if payload.get("source_sha256") != Some(&campaign.source_sha256) {
        errors.push(reject(
            RejectionKind::SchemaError,
            "source hash map mismatch",
        ));
    }
    if payload
        .get("source_symbols")
        .unwrap_or(&Value::Object(Map::new()))
        != &campaign.source_symbols
    {
        errors.push(reject(
            RejectionKind::SchemaError,
            "source symbol map mismatch",
        ));
    }
    if payload
        .get("test_scopes")
        .unwrap_or(&Value::Object(Map::new()))
        != &Value::Object(campaign.test_scopes.clone())
    {
        errors.push(reject(
            RejectionKind::SchemaError,
            "test scope map mismatch",
        ));
    }
    if payload.get("complete_campaign") != Some(&Value::Bool(true)) {
        errors.push(reject(
            RejectionKind::SchemaError,
            "partial campaign receipt",
        ));
    }
    errors.extend(reviewed_mutant_errors(payload, campaign));
    if payload.get("mutation_score").and_then(Value::as_f64) != Some(1.0) {
        errors.push(reject(
            RejectionKind::SchemaError,
            "mutation score is not 1.0",
        ));
    }
    if let Some(contract) = &campaign.value_analysis {
        errors.extend(value_receipt_errors(payload.get("test_value"), contract));
    }
    if campaign.source_drifted {
        errors.push(reject(
            // Stale PASS, same as the generated rule: not a current PASS, so
            // the debt class is not_pass.
            RejectionKind::NotPass,
            "current source hashes drifted",
        ));
    }
    errors
}

pub(crate) fn receipt_errors(
    receipt: &Receipt,
    campaign: &CampaignContract,
    context: &ValidationContext<'_>,
) -> Vec<Rejection> {
    // A slim receipt (PR #41) keeps the summary block plain but moves the bulky
    // lists -- `mutants`, `test_value` -- under one `detail` value, and the rules
    // below read those keys. Expand first: a plain receipt has no `detail` key
    // and expands to itself, so pre-slim receipts validate unchanged. An
    // expansion failure is itself the rejection, never a reason to judge the
    // summary alone: a `superseded` pointer names the receipt that replaced this
    // one, a corrupt blob names its own decode error, and a slim receipt read
    // without its detail would look like a plain one that mysteriously lost its
    // lists. The kind comes from the pointer's own encoding field -- the same
    // value the decoder branches on -- not from matching the message text.
    let expanded = match expand_receipt(&receipt.value) {
        Ok(value) => value,
        Err(error) => {
            let superseded = receipt
                .value
                .get("detail")
                .and_then(Value::as_object)
                .is_some_and(|detail| {
                    detail.get("encoding") == Some(&Value::String("superseded".to_owned()))
                });
            let kind = if superseded {
                RejectionKind::Superseded
            } else {
                RejectionKind::DecodeError
            };
            return vec![reject(kind, error)];
        }
    };
    let payload = expanded
        .as_object()
        .expect("receipt loader requires object");
    if campaign.generated {
        return generated_receipt_errors(payload, campaign, context);
    }
    reviewed_receipt_errors(receipt, payload, campaign, context)
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
    let package_root = lexical_absolute(Path::new(&request.package_root)).map_err(value_error)?;
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
        package_root: &package_root,
        anchor_repo: &anchor_repo,
        runner: &request.runner,
        anchor: &request.anchor,
    };
    // This seam keeps its stable string-array output: the kinds travel with the
    // evidence layer's result, not the single-receipt validator.
    serde_json::to_string(&rejection_details(receipt_errors(
        &receipt,
        &request.campaign,
        &context,
    )))
    .map_err(|error| value_error(error.to_string()))
}

#[pyfunction]
pub fn mutation_runner_lineage_accepts_native(request_json: &str) -> PyResult<bool> {
    let request: LineageRequest = parse_json(request_json, "lineage request")?;
    let root = lexical_absolute(Path::new(&request.package_root)).map_err(value_error)?;
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

#[cfg(test)]
mod generated_receipt_tests {
    use std::path::Path;
    use std::path::PathBuf;

    use serde_json::{json, Map, Value};

    use super::{
        campaign_scope_error, default_anchor_registry_path, exact_parse_error,
        generated_receipt_errors, legacy_receipt_anchor_errors, lineage_accepts, load_receipts,
        receipt_errors, rejection_details, runner_errors, scope_error, sha256_file,
        value_receipt_errors, AnchorConfig, Receipt, Rejection, RejectionKind, RunnerState,
        ValidationContext,
    };
    use crate::mutation_manifest::{CampaignContract, ValueContract};

    const MANIFEST: &str = "conductor/mutation_campaigns/subject_fest_20260906.json";

    fn repo_root() -> PathBuf {
        std::env::temp_dir().join(format!(
            "conductor-native-receipt-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ))
    }

    #[test]
    fn binding_fixtures_are_isolated_between_parallel_tests() {
        let current = repo_root();
        assert_eq!(current, repo_root());
        let other = std::thread::spawn(repo_root)
            .join()
            .expect("fixture thread");
        assert_ne!(current, other);
    }

    fn ensure_binding_files(root: &Path) {
        std::fs::create_dir_all(root.join("conductor")).expect("binding directory");
        for relative in [
            "conductor/mutation_engine_generated.py",
            "conductor/mutation_engine_fest.py",
            "conductor/mutation_run_scope.py",
        ] {
            let path = root.join(relative);
            if !path.exists() {
                std::fs::write(path, "fixture binding\n").expect("binding file");
            }
        }
    }

    fn campaign() -> CampaignContract {
        serde_json::from_value(json!({
            "campaign_id": "subject_fest_20260906",
            "title": "Subject under fest",
            "language": "python",
            "mutation_engine": "fest",
            "expected_mutations": 0,
            "manifest": MANIFEST,
            "manifest_sha256": "a".repeat(64),
            "source_sha256": {"conductor/subject.py": "b".repeat(64)},
            "source_symbols": {},
            "test_scopes": {},
            "ranked_tests": [],
            "ranked_test_paths": ["conductor/test_subject.py"],
            "planned_mutations": [],
            "mutations": [],
            "test_argv": ["python", "-m", "pytest", "-q", "conductor/test_subject.py"],
            "timeout_seconds": 900,
            "blocked_process_substrings": [],
            "poll_seconds": 0,
            "environment": {},
            "host_read_dependencies": [],
            "value_analysis_payload": null,
            "value_analysis": null,
            "source_drifted": false,
            "generated": true,
            "survivor_baseline": ["constant_replace-abc123456789-0"],
            "test_sha256": {"conductor/test_subject.py": "c".repeat(64)}
        }))
        .expect("campaign contract")
    }

    /// A receipt that agrees with the campaign on every pinned digest.
    fn receipt(overrides: Value) -> Map<String, Value> {
        let campaign = campaign();
        let root = repo_root();
        ensure_binding_files(&root);
        let mut base = json!({
            "schema_version": "llm.mutation-testing.receipt.v3",
            "mutants_are_generated": true,
            "status": "PASS",
            "campaign_id": campaign.campaign_id,
            "manifest": MANIFEST,
            "manifest_sha256": campaign.manifest_sha256,
            "source_sha256": campaign.source_sha256,
            "test_sha256": campaign.test_sha256,
            "core_sha256": sha256_file(&root.join("conductor/mutation_engine_generated.py")),
            "adapter_sha256": sha256_file(&root.join("conductor/mutation_engine_fest.py")),
            "scope_guard_sha256": sha256_file(&root.join("conductor/mutation_run_scope.py")),
            "survivors": ["constant_replace-abc123456789-0"],
            "baseline": {"returncode": 0, "timed_out": false},
            "mutants": [{"id": "fixture-kill", "outcome": "KILLED"}],
            "outcome_counts": {"KILLED": 1, "NO_COVERAGE": 0, "UNVIABLE": 0, "ERROR": 0, "SURVIVED": 0, "TIMED_OUT": 0},
            "mutation_score": 1.0
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

    fn rejections(overrides: Value) -> Vec<Rejection> {
        // No runner components on disk means the runner check has nothing to compare against,
        // which isolates these tests to the generated-campaign rule they are about.
        let runner = RunnerState {
            components: None,
            error: None,
            mutation_testing_sha256: None,
        };
        let anchor = AnchorConfig {
            repo: String::new(),
            commit: String::new(),
            tree: String::new(),
            receipt_prefix: String::new(),
            registry_path: default_anchor_registry_path(),
        };
        let root = repo_root();
        ensure_binding_files(&root);
        let context = ValidationContext {
            repo_root: &root,
            package_root: &root,
            anchor_repo: Path::new("/nonexistent"),
            runner: &runner,
            anchor: &anchor,
        };
        generated_receipt_errors(&receipt(overrides), &campaign(), &context)
    }

    fn errors(overrides: Value) -> Vec<String> {
        rejection_details(rejections(overrides))
    }

    fn manual_errors(overrides: Value) -> Vec<String> {
        let mut campaign = campaign();
        campaign.generated = false;
        campaign.mutation_engine = "reviewed_unified_diff".to_owned();
        manual_errors_for(overrides, campaign)
    }

    fn manual_errors_for(overrides: Value, campaign: CampaignContract) -> Vec<String> {
        let mut payload = receipt(json!({
            "complete_campaign": true,
            "selected_mutations": [],
            "mutants": [],
            "mutation_score": 1.0
        }));
        for (key, value) in overrides.as_object().expect("overrides object") {
            if value.is_null() {
                payload.remove(key);
            } else {
                payload.insert(key.clone(), value.clone());
            }
        }
        let runner = RunnerState {
            components: None,
            error: None,
            mutation_testing_sha256: None,
        };
        let anchor = AnchorConfig {
            repo: String::new(),
            commit: String::new(),
            tree: String::new(),
            receipt_prefix: String::new(),
            registry_path: default_anchor_registry_path(),
        };
        let root = repo_root();
        let context = ValidationContext {
            repo_root: &root,
            package_root: &root,
            anchor_repo: Path::new("/nonexistent"),
            runner: &runner,
            anchor: &anchor,
        };
        let receipt = Receipt {
            path: PathBuf::new(),
            relative: String::new(),
            name: String::new(),
            value: Value::Object(payload),
            bytes: Vec::new(),
            parsed_bytes_available: false,
        };
        rejection_details(receipt_errors(&receipt, &campaign, &context))
    }

    #[test]
    fn generated_completeness_requires_consistent_baseline_and_mutant_accounting() {
        let valid = errors(json!({"survivors": []}));
        assert!(valid.is_empty(), "{valid:?}");
        for (field, value, expected) in [
            (
                "baseline",
                json!({"returncode": 1, "timed_out": false}),
                "baseline must pass",
            ),
            (
                "baseline",
                json!({"returncode": 0, "timed_out": true}),
                "baseline must pass",
            ),
            ("baseline", json!({}), "baseline must pass"),
            ("mutants", json!([]), "at least one killed"),
            ("mutants", json!({}), "non-empty list"),
            (
                "mutants",
                json!([{"id":"a", "outcome":"SURVIVED"}]),
                "invalid mutant outcome",
            ),
            (
                "mutants",
                json!([{"id":"", "outcome":"KILLED"}]),
                "unique non-empty IDs",
            ),
            (
                "mutants",
                json!([{"outcome":"KILLED"}]),
                "unique non-empty IDs",
            ),
            (
                "mutants",
                json!([{"id":"a", "outcome":"KILLED"},{"id":"a", "outcome":"KILLED"}]),
                "unique non-empty IDs",
            ),
            (
                "mutants",
                json!([{"id":"a", "outcome":"NO_COVERAGE"}]),
                "at least one killed",
            ),
            ("outcome_counts", json!({}), "outcome_counts do not match"),
            ("mutation_score", json!(0.5), "mutation score is not 1.0"),
        ] {
            let found = errors(json!({"survivors": [], field: value}));
            assert!(
                found.iter().any(|error| error.contains(expected)),
                "{field}: {found:?}"
            );
        }
        let mut counts =
            json!({"KILLED":1,"NO_COVERAGE":0,"UNVIABLE":0,"ERROR":0,"SURVIVED":0,"TIMED_OUT":0});
        counts["KILLED"] = json!(2);
        assert!(errors(json!({"survivors": [], "outcome_counts":counts}))
            .iter()
            .any(|error| error.contains("outcome_counts do not match")));
        assert!(errors(json!({"survivors": [], "outcome_counts":counts, "mutants":[{"id":"a","outcome":"KILLED"},{"id":"b","outcome":"KILLED"}]})).is_empty());
    }

    #[test]
    fn a_timed_out_mutant_is_a_measurement_not_an_invalid_outcome() {
        // The engine cut the mutant off at the campaign's own per-mutant bound:
        // unknown, neither killed nor surviving, counted like NO_COVERAGE.
        assert!(errors(json!({
            "survivors": [],
            "mutants": [{"id":"a", "outcome":"KILLED"}, {"id":"b", "outcome":"TIMED_OUT"}],
            "outcome_counts": {"KILLED":1,"NO_COVERAGE":0,"UNVIABLE":0,"ERROR":0,"SURVIVED":0,"TIMED_OUT":1}
        }))
        .is_empty());
        // The count still has to agree with the rows it summarizes.
        assert!(errors(json!({
            "survivors": [],
            "mutants": [{"id":"a", "outcome":"KILLED"}, {"id":"b", "outcome":"TIMED_OUT"}],
            "outcome_counts": {"KILLED":1,"NO_COVERAGE":0,"UNVIABLE":0,"ERROR":0,"SURVIVED":0,"TIMED_OUT":0}
        }))
        .iter()
        .any(|error| error.contains("outcome_counts do not match")));
    }

    #[test]
    fn a_missing_process_output_is_not_success() {
        assert!(!super::successful(&None));
        let failed = std::process::Command::new("git")
            .arg("--invalid-receipt-fixture-option")
            .output()
            .expect("git executable");
        assert!(!super::successful(&Some(failed)));
        let passed = std::process::Command::new("git")
            .arg("--version")
            .output()
            .expect("git executable");
        assert!(super::successful(&Some(passed)));
    }

    #[test]
    fn legacy_mutant_rows_bind_the_kill_and_patch_digest() {
        for (outcome, digest, expected) in [
            ("KILLED", "a", None),
            ("SURVIVED", "a", Some("mutant fixture was not killed")),
            ("KILLED", "b", Some("mutant fixture patch hash mismatch")),
        ] {
            let mut contract = campaign();
            contract.generated = false;
            contract.mutations = serde_json::from_value(json!([{
                "id":"fixture", "patch_file":"parser-only.patch", "patch_sha256":"a",
                "allowed_paths":[], "expected_killers":[]
            }]))
            .expect("parser-only mutation contract");
            let found = manual_errors_for(
                json!({
                    "selected_mutations":["fixture"],
                    "mutants":[{"id":"fixture","outcome":outcome,"patch_sha256":digest}]
                }),
                contract,
            );
            if let Some(expected) = expected {
                assert!(found.iter().any(|error| error == expected), "{found:?}");
            } else {
                assert!(found.is_empty(), "{found:?}");
            }
        }
    }

    /// The same anchor with one binding varied -- the shape every negative case here
    /// needs, now that the bindings travel together.
    fn rebound(anchor: &AnchorConfig, commit: &str, tree: &str) -> AnchorConfig {
        AnchorConfig {
            commit: commit.to_owned(),
            tree: tree.to_owned(),
            ..anchor.clone()
        }
    }

    fn legacy_anchor_fixture() -> (PathBuf, Receipt, CampaignContract, AnchorConfig) {
        let root = std::env::temp_dir().join(format!(
            "conductor-native-anchor-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join("conductor/mutation_campaigns"))
            .expect("campaign directory");
        std::fs::create_dir_all(root.join("research/reports/mutation_testing"))
            .expect("receipt directory");
        let manifest_path = "conductor/mutation_campaigns/legacy.json";
        let manifest = b"{\"fixture\":true}\n";
        std::fs::write(root.join(manifest_path), manifest).expect("manifest");
        std::fs::write(
            root.join("conductor/mutation_campaigns/registry.json"),
            format!("{{\"campaigns\":[{{\"manifest\":\"{manifest_path}\"}}]}}"),
        )
        .expect("registry");
        let relative = "research/reports/mutation_testing/legacy.json";
        let bytes = b"{\"schema_version\":\"llm.mutation-testing.receipt.v2\"}\n".to_vec();
        std::fs::write(root.join(relative), &bytes).expect("receipt");
        for args in [
            vec!["init", "-q"],
            vec!["config", "user.email", "fixture@example.invalid"],
            vec!["config", "user.name", "fixture"],
            vec!["add", "."],
            vec!["commit", "-q", "-m", "fixture"],
        ] {
            let status = std::process::Command::new("git")
                .args(args)
                .current_dir(&root)
                .status()
                .expect("git available");
            assert!(status.success(), "git fixture setup");
        }
        let rev = |argument: &str| {
            String::from_utf8(
                std::process::Command::new("git")
                    .args(["rev-parse", argument])
                    .current_dir(&root)
                    .output()
                    .expect("git revision")
                    .stdout,
            )
            .expect("utf8")
            .trim()
            .to_owned()
        };
        let mut campaign = campaign();
        campaign.generated = false;
        campaign.manifest = manifest_path.to_owned();
        campaign.manifest_sha256 = sha256_file(&root.join(manifest_path)).expect("manifest hash");
        let receipt = Receipt {
            path: root.join(relative),
            relative: relative.to_owned(),
            name: "legacy.json".to_owned(),
            value: json!({}),
            bytes,
            parsed_bytes_available: true,
        };
        let anchor = AnchorConfig {
            repo: root.to_string_lossy().into_owned(),
            commit: rev("HEAD"),
            tree: rev("HEAD^{tree}"),
            receipt_prefix: "research/reports/mutation_testing/".to_owned(),
            registry_path: default_anchor_registry_path(),
        };
        (root, receipt, campaign, anchor)
    }

    #[test]
    fn a_pass_receipt_cannot_label_baseline_survivors_as_complete() {
        let found = errors(json!({}));
        assert!(
            found
                .iter()
                .any(|error| error == "PASS receipt reports surviving mutants"),
            "{found:?}"
        );
    }

    #[test]
    fn a_clean_run_is_accepted_even_though_the_score_is_not_one() {
        // This is the whole point of the second rule: the patch rule demanded score == 1.0,
        // which is why 482 of 483 scored campaigns published exactly 1.0.
        assert_eq!(errors(json!({"survivors": []})), Vec::<String>::new());
    }

    #[test]
    fn a_survivor_outside_the_baseline_is_reported_by_id() {
        let found = errors(json!({
            "survivors": ["constant_replace-abc123456789-0", "operator_swap-0123456789ab-2"]
        }));
        assert!(
            found
                .iter()
                .any(|error| error.contains("operator_swap-0123456789ab-2")),
            "{found:?}"
        );
        assert!(
            found.iter().any(|error| error.starts_with("1 survivor(s)")),
            "{found:?}"
        );
    }

    #[test]
    fn a_pass_receipt_rejects_non_string_survivor_rows() {
        let found = errors(json!({"survivors": [17]}));
        assert!(
            found
                .iter()
                .any(|error| error == "survivors must contain only mutation IDs"),
            "{found:?}"
        );
        assert!(
            found
                .iter()
                .any(|error| error == "PASS receipt reports surviving mutants"),
            "{found:?}"
        );
    }

    #[test]
    fn a_receipt_rejects_nonmatching_core_adapter_and_scope_bindings() {
        for key in ["core_sha256", "adapter_sha256", "scope_guard_sha256"] {
            let found = errors(json!({key: "f".repeat(64)}));
            assert!(
                found.iter().any(|error| error.starts_with(key)),
                "{key}: {found:?}"
            );
        }
    }

    #[test]
    fn a_generated_receipt_rejects_each_identity_binding_independently() {
        let cases = [
            ("schema_version", json!("old"), "receipt schema"),
            ("mutants_are_generated", json!(false), "engine-generated"),
            ("status", json!("ERROR"), "status="),
            ("status", json!("RATCHET_HELD"), "status="),
            ("campaign_id", json!("other"), "campaign_id mismatch"),
            ("manifest", json!("other.json"), "manifest path mismatch"),
            (
                "manifest_sha256",
                json!("f".repeat(64)),
                "manifest hash mismatch",
            ),
            ("source_sha256", json!({}), "source hash map mismatch"),
            ("test_sha256", json!({}), "test hash map mismatch"),
        ];
        for (key, value, expected) in cases {
            let found = errors(json!({key: value}));
            assert!(
                found.iter().any(|error| error.contains(expected)),
                "{key}: {found:?}"
            );
        }
    }

    #[test]
    fn a_legacy_receipt_rejects_each_complete_campaign_binding() {
        let cases = [
            (
                "schema_version",
                json!("old"),
                "receipt schema is not current",
            ),
            ("status", json!("ERROR"), "status='ERROR'"),
            ("campaign_id", json!("other"), "campaign_id mismatch"),
            ("manifest", json!("other.json"), "manifest path mismatch"),
            (
                "manifest_sha256",
                json!("f".repeat(64)),
                "manifest hash mismatch",
            ),
            ("source_sha256", json!({}), "source hash map mismatch"),
            (
                "source_symbols",
                json!({"other.py": {}}),
                "source symbol map mismatch",
            ),
            (
                "test_scopes",
                json!({"other.py": {}}),
                "test scope map mismatch",
            ),
            (
                "complete_campaign",
                json!(false),
                "partial campaign receipt",
            ),
            (
                "selected_mutations",
                json!(["unexpected"]),
                "selected mutation ids mismatch",
            ),
            (
                "mutants",
                json!([{"id": "unexpected", "outcome": "KILLED"}]),
                "mutant result ids mismatch",
            ),
            ("mutation_score", json!(0.99), "mutation score is not 1.0"),
        ];
        for (key, value, expected) in cases {
            let found = manual_errors(json!({key: value}));
            assert!(
                found.iter().any(|error| error.contains(expected)),
                "{key}: {found:?}"
            );
        }
    }

    #[test]
    fn value_and_runner_evidence_bind_every_required_field() {
        let contract = ValueContract {
            expected_nodeids: vec!["tests::subject".to_owned()],
            expected_repetitions: 2,
        };
        let valid_value = json!({
            "schema_version": "llm.mutation-testing.test-value.v1",
            "status": "PASS",
            "tests": [{"nodeid": "tests::subject"}],
            "baseline_repetitions": 2
        });
        assert!(value_receipt_errors(Some(&valid_value), &contract).is_empty());
        for (value, expected) in [
            (Value::Null, "test_value evidence is missing"),
            (json!({}), "test_value schema is not current"),
            (
                json!({"schema_version": "llm.mutation-testing.test-value.v1", "status": "FAIL"}),
                "test_value status='FAIL'",
            ),
            (
                json!({"schema_version": "llm.mutation-testing.test-value.v1", "status": "PASS", "tests": [], "baseline_repetitions": 2}),
                "test_value nodeids do not match ranked tests",
            ),
            (
                json!({"schema_version": "llm.mutation-testing.test-value.v1", "status": "PASS", "tests": [{"nodeid":"tests::subject"}], "baseline_repetitions": 1}),
                "test_value baseline repetitions mismatch",
            ),
        ] {
            let found = rejection_details(value_receipt_errors(Some(&value), &contract));
            assert!(
                found.iter().any(|error| error.contains(expected)),
                "{found:?}"
            );
        }

        let root = repo_root();
        let anchor = AnchorConfig {
            repo: String::new(),
            commit: String::new(),
            tree: String::new(),
            receipt_prefix: String::new(),
            registry_path: default_anchor_registry_path(),
        };
        let components = json!({"core": "a".repeat(64)});
        let runner = RunnerState {
            components: Some(components.clone()),
            error: None,
            mutation_testing_sha256: Some("b".repeat(64)),
        };
        let context = ValidationContext {
            repo_root: &root,
            package_root: &root,
            anchor_repo: Path::new("/nonexistent"),
            runner: &runner,
            anchor: &anchor,
        };
        assert!(runner_errors(json!({"runner_components_sha256": components.clone(), "runner_sha256": "b".repeat(64)}).as_object().expect("object"), &context).is_empty());
        let stale_map = runner_errors(
            json!({"runner_components_sha256": {}, "runner_sha256": "b".repeat(64)})
                .as_object()
                .expect("object"),
            &context,
        );
        assert!(stale_map
            .iter()
            .any(|error| error.detail.contains("component hash map mismatch")));
        assert!(stale_map
            .iter()
            .any(|error| error.kind == RejectionKind::RunnerMapMismatch));
        let stale_hash = runner_errors(
            json!({"runner_components_sha256": components, "runner_sha256": "c".repeat(64)})
                .as_object()
                .expect("object"),
            &context,
        );
        assert!(stale_hash
            .iter()
            .any(|error| error.detail.contains("runner hash mismatch")));
        assert!(stale_hash
            .iter()
            .any(|error| error.kind == RejectionKind::RunnerMapMismatch));
        let mut legacy = campaign();
        legacy.generated = false;
        let current = Receipt {
            path: PathBuf::new(),
            relative: String::new(),
            name: String::new(),
            value: Value::Object(receipt(json!({"runner_components_sha256": {}}))),
            bytes: Vec::new(),
            parsed_bytes_available: false,
        };
        assert!(
            rejection_details(receipt_errors(&current, &legacy, &context))
                .iter()
                .any(|error| error.contains("component hash map mismatch"))
        );
    }

    #[test]
    fn receipt_loading_keeps_valid_objects_and_reports_exact_malformed_inputs() {
        let root =
            std::env::temp_dir().join(format!("conductor-native-receipts-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let directory = root.join("research/receipts");
        std::fs::create_dir_all(&directory).expect("receipt directory");
        std::fs::write(directory.join("valid.json"), b"{\"status\":\"PASS\"}\n")
            .expect("valid receipt");
        std::fs::write(directory.join("array.json"), b"[]\n").expect("array receipt");
        std::fs::write(directory.join("broken.json"), b"{\n").expect("broken receipt");
        pyo3::Python::initialize();
        pyo3::Python::try_attach(|py| {
            let (receipts, malformed) = load_receipts(
                py,
                &root,
                &["research/receipts".to_owned(), "absent".to_owned()],
            )
            .expect("readable directory");
            assert_eq!(receipts.len(), 1);
            assert_eq!(receipts[0].name, "valid.json");
            assert_eq!(malformed.len(), 2, "{malformed:?}");
            assert!(malformed
                .iter()
                .any(|row| row.contains("array.json") && row.contains("JSON object")));
            assert!(malformed.iter().any(|row| row.contains("broken.json")));
            assert!(
                exact_parse_error(py, &directory.join("array.json"), b"[]").contains("JSON object")
            );
        })
        .expect("Python interpreter attached");
        std::fs::remove_dir_all(root).expect("fixture cleanup");
    }

    #[test]
    fn complete_campaign_scope_is_required_for_each_changed_test() {
        let mut contract = campaign();
        assert_eq!(
            campaign_scope_error(&contract, "conductor/test_subject.py"),
            Some("campaign lacks explicit test scope".to_owned())
        );
        contract.test_scopes.insert(
            "conductor/test_subject.py".to_owned(),
            json!({"mode": "partial"}),
        );
        assert_eq!(
            scope_error(&contract, "conductor/test_subject.py"),
            Some("test scope is 'partial', not 'complete'".to_owned())
        );
        contract.test_scopes.insert(
            "conductor/test_subject.py".to_owned(),
            json!({"mode": "complete"}),
        );
        assert_eq!(scope_error(&contract, "conductor/test_subject.py"), None);
    }

    #[test]
    fn lineage_requires_a_current_schema_and_exact_component_map_entry() {
        let root = std::env::temp_dir().join(format!("native-lineage-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("conductor")).expect("lineage directory");
        let recorded = json!({"core": "a".repeat(64)});
        for value in [
            json!({}),
            json!({"schema_version": 0, "entries": []}),
            json!({"schema_version": 1}),
            json!({"schema_version": 1, "entries": [{}]}),
            json!({"schema_version": 1, "entries": [{"runner_components_sha256": {}}]}),
        ] {
            std::fs::write(
                root.join("conductor/mutation_runner_lineage.json"),
                serde_json::to_vec(&value).expect("lineage json"),
            )
            .expect("lineage write");
            assert!(!lineage_accepts(Some(&recorded), &root));
        }
        std::fs::write(
            root.join("conductor/mutation_runner_lineage.json"),
            serde_json::to_vec(
                &json!({"schema_version": 1, "entries": [{"runner_components_sha256": recorded}]}),
            )
            .expect("lineage json"),
        )
        .expect("lineage write");
        assert!(lineage_accepts(
            Some(&json!({"core": "a".repeat(64)})),
            &root
        ));
        std::fs::remove_dir_all(root).expect("fixture cleanup");
    }

    #[test]
    fn legacy_receipt_anchor_requires_the_committed_receipt_and_all_anchor_bindings() {
        let (root, receipt, campaign, anchor) = legacy_anchor_fixture();
        assert!(
            legacy_receipt_anchor_errors(&receipt, &root, &root, &anchor, &campaign,).is_empty()
        );
        let bad_tree = rejection_details(legacy_receipt_anchor_errors(
            &receipt,
            &root,
            &root,
            &rebound(&anchor, &anchor.commit, "0"),
            &campaign,
        ));
        assert!(bad_tree.iter().any(|error| error.contains("tree mismatch")));
        for (anchor_root, commit, tree, expected) in [
            (
                root.join("conductor"),
                anchor.commit.as_str(),
                anchor.tree.as_str(),
                "legacy receipt anchor repository is unavailable",
            ),
            (
                root.clone(),
                anchor.tree.as_str(),
                anchor.tree.as_str(),
                "legacy receipt anchor commit is unavailable",
            ),
        ] {
            assert_eq!(
                rejection_details(legacy_receipt_anchor_errors(
                    &receipt,
                    &root,
                    &anchor_root,
                    &rebound(&anchor, commit, tree),
                    &campaign
                )),
                vec![expected.to_owned()]
            );
        }
        let changed_bytes = Receipt {
            path: receipt.path.clone(),
            relative: receipt.relative.clone(),
            name: receipt.name.clone(),
            value: receipt.value.clone(),
            bytes: b"different parser bytes".to_vec(),
            parsed_bytes_available: true,
        };
        let outside_directory = Receipt {
            path: root.join(&campaign.manifest),
            relative: campaign.manifest.clone(),
            name: "legacy.json".to_owned(),
            value: receipt.value.clone(),
            bytes: receipt.bytes.clone(),
            parsed_bytes_available: true,
        };
        assert_eq!(
            rejection_details(legacy_receipt_anchor_errors(
                &outside_directory,
                &root,
                &root,
                &anchor,
                &campaign
            )),
            vec!["legacy receipt path is outside the anchored receipt directory".to_owned()]
        );
        assert_eq!(
            rejection_details(legacy_receipt_anchor_errors(
                &changed_bytes,
                &root,
                &root,
                &anchor,
                &campaign
            )),
            vec!["legacy receipt parsed bytes differ from the anchor".to_owned()]
        );
        for args in [
            vec!["update-index", "--chmod=+x", receipt.relative.as_str()],
            vec!["commit", "-q", "-m", "executable receipt fixture"],
        ] {
            assert!(std::process::Command::new("git")
                .args(args)
                .current_dir(&root)
                .status()
                .expect("fixture git")
                .success());
        }
        let revision = |name: &str| {
            String::from_utf8(
                std::process::Command::new("git")
                    .args(["rev-parse", name])
                    .current_dir(&root)
                    .output()
                    .expect("fixture revision")
                    .stdout,
            )
            .expect("revision utf8")
            .trim()
            .to_owned()
        };
        assert_eq!(
            rejection_details(legacy_receipt_anchor_errors(
                &receipt,
                &root,
                &root,
                &rebound(&anchor, &revision("HEAD"), &revision("HEAD^{tree}")),
                &campaign
            )),
            vec!["legacy receipt is absent or unsafe at the anchor".to_owned()]
        );
        let unavailable = Receipt {
            parsed_bytes_available: false,
            ..receipt
        };
        assert!(rejection_details(legacy_receipt_anchor_errors(
            &unavailable,
            &root,
            &root,
            &anchor,
            &campaign,
        ))
        .iter()
        .any(|error| error.contains("path or parsed bytes")));
        std::fs::remove_dir_all(root).expect("fixture cleanup");
    }

    #[test]
    fn a_generated_receipt_rejects_an_unknown_engine_mapping() {
        let mut campaign = campaign();
        campaign.mutation_engine = "unknown".to_owned();
        let runner = RunnerState {
            components: None,
            error: None,
            mutation_testing_sha256: None,
        };
        let anchor = AnchorConfig {
            repo: String::new(),
            commit: String::new(),
            tree: String::new(),
            receipt_prefix: String::new(),
            registry_path: default_anchor_registry_path(),
        };
        let root = repo_root();
        ensure_binding_files(&root);
        let context = ValidationContext {
            repo_root: &root,
            package_root: &root,
            anchor_repo: Path::new("/nonexistent"),
            runner: &runner,
            anchor: &anchor,
        };
        let found = rejection_details(generated_receipt_errors(
            &receipt(json!({})),
            &campaign,
            &context,
        ));
        assert!(
            found
                .iter()
                .any(|error| error.contains("unsupported generated mutation engine")),
            "{found:?}"
        );
    }

    #[test]
    fn generated_adapter_binding_requires_the_exact_engine_adapter() {
        for (engine, adapter) in [
            ("fest", "conductor/mutation_engine_fest.py"),
            ("cargo-mutants", "conductor/mutation_engine_cargo.py"),
            ("mull", "conductor/mutation_engine_mull.py"),
        ] {
            let root = repo_root();
            ensure_binding_files(&root);
            std::fs::write(root.join(adapter), "adapter\n").expect("adapter");
            let mut campaign = campaign();
            campaign.mutation_engine = engine.to_owned();
            let runner = RunnerState {
                components: None,
                error: None,
                mutation_testing_sha256: None,
            };
            let anchor = AnchorConfig {
                repo: String::new(),
                commit: String::new(),
                tree: String::new(),
                receipt_prefix: String::new(),
                registry_path: default_anchor_registry_path(),
            };
            let context = ValidationContext {
                repo_root: &root,
                package_root: &root,
                anchor_repo: Path::new("/nonexistent"),
                runner: &runner,
                anchor: &anchor,
            };
            let good = receipt(
                json!({"survivors": [], "adapter_sha256": sha256_file(&root.join(adapter))}),
            );
            assert!(
                generated_receipt_errors(&good, &campaign, &context).is_empty(),
                "{engine}"
            );
            let bad = receipt(json!({"survivors": [], "adapter_sha256": "f".repeat(64)}));
            assert!(
                rejection_details(generated_receipt_errors(&bad, &campaign, &context))
                    .iter()
                    .any(|error| error.contains("adapter_sha256 hash mismatch")),
                "{engine}"
            );
            let missing = receipt(json!({"survivors": [], "adapter_sha256": ""}));
            assert!(
                rejection_details(generated_receipt_errors(&missing, &campaign, &context))
                    .iter()
                    .any(|error| error.contains("adapter_sha256 is missing")),
                "{engine}"
            );
        }
    }

    #[test]
    fn generated_bindings_resolve_against_package_root_not_repo_root() {
        // Under a src layout `repo_root` (the git root) and `package_root` (where a bare
        // `conductor/...` literal resolves from) differ -- `package_root` is `repo_root/src`.
        // The core/scope-guard/adapter bindings must join `package_root`, never `repo_root`,
        // or every receipt in a src-layout repository hashes the wrong (or absent) file.
        let repo_root = repo_root();
        let package_root = repo_root.join("src");
        ensure_binding_files(&package_root);
        let adapter = "conductor/mutation_engine_fest.py";
        std::fs::write(package_root.join(adapter), "adapter\n").expect("adapter");
        let mut campaign = campaign();
        campaign.mutation_engine = "fest".to_owned();
        let runner = RunnerState {
            components: None,
            error: None,
            mutation_testing_sha256: None,
        };
        let anchor = AnchorConfig {
            repo: String::new(),
            commit: String::new(),
            tree: String::new(),
            receipt_prefix: String::new(),
            registry_path: default_anchor_registry_path(),
        };
        let context = ValidationContext {
            repo_root: &repo_root,
            package_root: &package_root,
            anchor_repo: Path::new("/nonexistent"),
            runner: &runner,
            anchor: &anchor,
        };
        let good = receipt(json!({
            "survivors": [],
            "core_sha256": sha256_file(&package_root.join("conductor/mutation_engine_generated.py")),
            "scope_guard_sha256": sha256_file(&package_root.join("conductor/mutation_run_scope.py")),
            "adapter_sha256": sha256_file(&package_root.join(adapter)),
        }));
        let found = rejection_details(generated_receipt_errors(&good, &campaign, &context));
        assert!(
            !found
                .iter()
                .any(|error| error.contains("_sha256 hash mismatch")
                    || error.contains("_sha256 is missing")),
            "{found:?}"
        );
        // A receipt bound to the (wrong) repo_root copies must still be rejected: the fix
        // must not have made the check universally lenient, only correctly rooted.
        let stale = receipt(json!({
            "survivors": [],
            "core_sha256": "f".repeat(64),
            "scope_guard_sha256": "f".repeat(64),
            "adapter_sha256": "f".repeat(64),
        }));
        let stale_found = rejection_details(generated_receipt_errors(&stale, &campaign, &context));
        assert!(
            stale_found
                .iter()
                .any(|error| error == "core_sha256 hash mismatch"),
            "{stale_found:?}"
        );
    }

    #[test]
    fn a_receipt_rejects_current_source_or_test_drift() {
        let mut campaign = campaign();
        campaign.source_drifted = true;
        let runner = RunnerState {
            components: None,
            error: None,
            mutation_testing_sha256: None,
        };
        let anchor = AnchorConfig {
            repo: String::new(),
            commit: String::new(),
            tree: String::new(),
            receipt_prefix: String::new(),
            registry_path: default_anchor_registry_path(),
        };
        let root = repo_root();
        let context = ValidationContext {
            repo_root: &root,
            package_root: &root,
            anchor_repo: Path::new("/nonexistent"),
            runner: &runner,
            anchor: &anchor,
        };
        let found = rejection_details(generated_receipt_errors(
            &receipt(json!({})),
            &campaign,
            &context,
        ));
        assert!(
            found
                .iter()
                .any(|error| error == "current source or test hashes drifted"),
            "{found:?}"
        );
    }

    #[test]
    fn rejections_carry_the_kind_of_the_rule_that_produced_them() {
        // A held ratchet is debt, not a defect: status lands as not_pass.
        assert!(
            rejections(json!({"status": "RATCHET_HELD", "survivors": []}))
                .iter()
                .any(|error| error.kind == RejectionKind::NotPass
                    && error.detail.starts_with("status="))
        );
        // The class PR #46 lived through: a mutants list the validator cannot
        // accept is a schema_error, never silently debt.
        assert!(rejections(json!({"mutants": json!({}), "survivors": []}))
            .iter()
            .any(|error| error.kind == RejectionKind::SchemaError
                && error.detail == "mutants must be a non-empty list"));
        // Identity bindings are schema errors...
        assert!(rejections(json!({"campaign_id": "other", "survivors": []}))
            .iter()
            .any(|error| error.kind == RejectionKind::SchemaError
                && error.detail == "campaign_id mismatch"));
        // ...while the runner-era bindings share the map's debt class.
        assert!(
            rejections(json!({"core_sha256": "f".repeat(64), "survivors": []}))
                .iter()
                .any(|error| error.kind == RejectionKind::RunnerMapMismatch
                    && error.detail == "core_sha256 hash mismatch")
        );
        // A survivor outside the baseline contradicts the claimed PASS: the
        // validator refuses the claim rather than filing it as re-run debt.
        assert!(rejections(json!({
            "survivors": ["operator_swap-0123456789ab-2"],
            "mutants": [{"id": "operator_swap-0123456789ab-2", "outcome": "SURVIVED"}],
            "outcome_counts": {"KILLED":0,"NO_COVERAGE":0,"UNVIABLE":0,"ERROR":0,"SURVIVED":1,"TIMED_OUT":0}
        }))
        .iter()
        .any(|error| error.kind == RejectionKind::SchemaError
            && error.detail.starts_with("1 survivor(s) outside the recorded baseline")));
    }

    #[test]
    fn a_receipt_that_does_not_declare_generated_mutants_is_refused() {
        // Otherwise a hand-authored patch receipt could be offered here and skip the ratchet.
        let found = errors(json!({"mutants_are_generated": null}));
        assert!(
            found.iter().any(|e| e.contains("engine-generated")),
            "{found:?}"
        );
    }

    #[test]
    fn a_receipt_whose_tests_have_changed_is_refused() {
        let found = errors(json!({
            "test_sha256": {"conductor/test_subject.py": "f".repeat(64)}
        }));
        assert!(
            found.iter().any(|e| e == "test hash map mismatch"),
            "{found:?}"
        );
    }

    #[test]
    fn a_receipt_without_an_adapter_digest_is_refused() {
        let found = errors(json!({"adapter_sha256": ""}));
        assert!(
            found.iter().any(|e| e == "adapter_sha256 is missing"),
            "{found:?}"
        );
    }

    #[test]
    fn a_failing_status_cannot_be_offered_as_evidence() {
        let found = errors(json!({"status": "FAIL"}));
        assert!(found.iter().any(|e| e.starts_with("status=")), "{found:?}");
    }
}
