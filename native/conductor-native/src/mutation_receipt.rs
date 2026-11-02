//! Receipt loading and immutable mutation-evidence validation.

use std::collections::{BTreeSet, HashMap};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

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

pub(crate) fn default_anchor_registry_path() -> String {
    ANCHOR_REGISTRY_PATH.to_owned()
}

fn sha256_file(path: &Path) -> Option<String> {
    crate::mutation_evidence::inputs::read_bounded(path, 32 * 1024 * 1024)
        .ok()
        .flatten()
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

fn exact_parse_error(path: &Path, bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Err(error) => format!("receipt is not UTF-8: {error}"),
        Ok(text) => match serde_json::from_str::<Value>(text) {
            Err(error) => format!("receipt is not JSON: {error}"),
            Ok(_) => format!("receipt {} must be a JSON object", path.display()),
        },
    }
}

pub(crate) fn load_receipts(
    repo_root: &Path,
    directories: &[String],
) -> Result<(Vec<Receipt>, Vec<String>), String> {
    let paths = receipt_paths(repo_root, directories)?;
    let mut receipts = Vec::with_capacity(paths.len());
    let mut total = 0usize;
    let mut malformed = Vec::new();
    for path in paths {
        let relative = path
            .strip_prefix(repo_root)
            .map_err(|_| "receipt path escapes repository".to_owned())?
            .to_string_lossy()
            .replace('\\', "/");
        let bytes = match crate::mutation_evidence::inputs::read_bounded(&path, 32 * 1024 * 1024) {
            Ok(Some(bytes)) => bytes,
            Ok(None) => {
                malformed.push(format!("{relative}: receipt disappeared"));
                continue;
            }
            Err(error) => {
                malformed.push(format!("{relative}: {error}"));
                continue;
            }
        };
        total = total
            .checked_add(bytes.len())
            .ok_or("mutation receipt byte overflow")?;
        if total > 64 * 1024 * 1024 {
            return Err("mutation receipt bytes exceed 64 MiB".into());
        }
        let value = match std::str::from_utf8(&bytes)
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(text).ok())
        {
            Some(Value::Object(object)) => Value::Object(object),
            _ => {
                malformed.push(format!("{relative}: {}", exact_parse_error(&path, &bytes)));
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

    let errors = anchor_identity_errors(anchor_repo, anchor);
    if !errors.is_empty() {
        return errors;
    }
    let errors = anchor_manifest_errors(anchor_repo, anchor, campaign);
    if !errors.is_empty() {
        return errors;
    }
    anchor_blob_errors(receipt, anchor_repo, anchor, &relative)
}
fn anchor_identity_errors(anchor_repo: &Path, anchor: &AnchorConfig) -> Vec<Rejection> {
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

    Vec::new()
}
fn anchor_manifest_errors(
    anchor_repo: &Path,
    anchor: &AnchorConfig,
    campaign: &CampaignContract,
) -> Vec<Rejection> {
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

    Vec::new()
}
fn anchor_blob_errors(
    receipt: &Receipt,
    anchor_repo: &Path,
    anchor: &AnchorConfig,
    relative: &str,
) -> Vec<Rejection> {
    let entry = git_bytes(
        anchor_repo,
        &[
            "ls-tree".to_owned(),
            anchor.commit.clone(),
            "--".to_owned(),
            relative.to_owned(),
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
    let path = package_root.join("conductor/mutation_runner_lineage.json");
    let Some(bytes) = crate::mutation_evidence::inputs::read_bounded(&path, 1024 * 1024)
        .ok()
        .flatten()
    else {
        return false;
    };
    let Ok(payload) = serde_json::from_slice::<Value>(&bytes) else {
        return false;
    };
    lineage_payload_accepts(recorded, &payload)
}
pub(crate) fn lineage_payload_accepts(recorded: Option<&Value>, payload: &Value) -> bool {
    let Some(Value::Object(_)) = recorded else {
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
    runner_errors_with(payload, context.runner, |recorded| {
        lineage_accepts(recorded, context.package_root)
    })
}
pub(crate) fn runner_errors_with(
    payload: &Map<String, Value>,
    runner: &RunnerState,
    lineage: impl Fn(Option<&Value>) -> bool,
) -> Vec<Rejection> {
    let mut errors = Vec::new();
    if let Some(error) = &runner.error {
        errors.push(reject(RejectionKind::RunnerMapMismatch, error.clone()));
    } else if let Some(components) = &runner.components {
        let recorded = payload.get("runner_components_sha256");
        if recorded != Some(components) && !lineage(recorded) {
            errors.push(reject(
                RejectionKind::RunnerMapMismatch,
                "runner component hash map mismatch",
            ));
        } else if recorded == Some(components)
            && payload.get("runner_sha256").and_then(Value::as_str)
                != runner.mutation_testing_sha256.as_deref()
        {
            errors.push(reject(
                RejectionKind::RunnerMapMismatch,
                "runner hash mismatch",
            ));
        }
    }
    errors
}

#[path = "mutation_receipt_generated.rs"]
mod generated;
pub(crate) use generated::generated_errors_with as captured_generated_errors;
#[cfg(test)]
pub(crate) use generated::generated_outcome_errors;
use generated::generated_receipt_errors;

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
    reviewed_errors_with(
        payload,
        campaign,
        || {
            legacy_receipt_anchor_errors(
                receipt,
                context.repo_root,
                context.anchor_repo,
                context.anchor,
                campaign,
            )
        },
        || runner_errors(payload, context),
    )
}
pub(crate) fn reviewed_errors_with(
    payload: &Map<String, Value>,
    campaign: &CampaignContract,
    anchor: impl FnOnce() -> Vec<Rejection>,
    runner: impl FnOnce() -> Vec<Rejection>,
) -> Vec<Rejection> {
    let mut errors = Vec::new();
    let schema = payload.get("schema_version").and_then(Value::as_str);
    let anchored_legacy = schema == Some(LEGACY_RECEIPT_SCHEMA);
    if anchored_legacy {
        errors.extend(anchor());
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
        errors.push(reject(
            // Same verdict as the generated rule: the path matches, so the
            // receipt documents an earlier revision of this campaign and the
            // campaign re-runs.
            RejectionKind::NotPass,
            "manifest hash mismatch",
        ));
    }
    if !anchored_legacy {
        errors.extend(runner());
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
    let expanded = match decode_captured(&receipt.value) {
        Ok(value) => value,
        Err(error) => return vec![error],
    };
    let Some(payload) = expanded.as_object() else {
        return vec![reject(
            RejectionKind::SchemaError,
            "receipt must be a JSON object",
        )];
    };
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

#[cfg(test)]
#[path = "mutation_receipt_tests.rs"]
mod generated_receipt_tests;

pub(crate) fn validate(request: &Value) -> Result<Value, String> {
    let request: ReceiptValidationRequest = serde_json::from_value(request.clone())
        .map_err(|error| format!("invalid receipt request: {error}"))?;
    let root = lexical_absolute(Path::new(&request.repo_root))?;
    let package_root = lexical_absolute(Path::new(&request.package_root))?;
    let anchor_repo = lexical_absolute(Path::new(&request.anchor_repo))?;
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
        None => serde_json::to_vec(&request.receipt).map_err(|error| error.to_string())?,
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
    Ok(serde_json::json!(rejection_details(receipt_errors(
        &receipt,
        &request.campaign,
        &context
    ))))
}

pub(crate) fn accepts_lineage(request: &Value) -> Result<bool, String> {
    let request: LineageRequest =
        serde_json::from_value(request.clone()).map_err(|error| error.to_string())?;
    let root = lexical_absolute(Path::new(&request.package_root))?;
    Ok(lineage_accepts(request.recorded.as_ref(), &root))
}
#[cfg(feature = "python")]
#[path = "mutation_receipt_bindings.rs"]
mod bindings;
#[cfg(feature = "python")]
pub(crate) use bindings::register;

fn receipt_paths(root: &Path, directories: &[String]) -> Result<Vec<PathBuf>, String> {
    if directories.len() > 64 {
        return Err("mutation receipt directories exceed 64".into());
    }
    let mut paths = Vec::new();
    let mut examined = 0usize;
    for raw in directories {
        let relative = safe_relative(raw, "receipt directory")?;
        let directory = root.join(relative);
        match fs::symlink_metadata(&directory) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.to_string()),
            Ok(meta) if meta.is_dir() => {}
            Ok(_) => return Err("mutation receipt directory must be a real directory".into()),
        }
        let mut local = Vec::new();
        for entry in fs::read_dir(&directory).map_err(|error| error.to_string())? {
            examined += 1;
            if examined > 65536 {
                return Err("mutation receipt inventory exceeds 65536 directory entries".into());
            }
            let path = entry.map_err(|error| error.to_string())?.path();
            if path.to_str().is_none() {
                return Err("mutation receipt paths must be UTF-8".into());
            }
            if path.extension().is_some_and(|suffix| suffix == "json") {
                if paths.len() + local.len() >= 4096 {
                    return Err("mutation receipt inventory exceeds 4096 files".into());
                }
                local.push(path);
            }
        }
        local.sort();
        paths.extend(local);
    }
    Ok(paths)
}

pub(crate) fn decode_captured(value: &Value) -> Result<Value, Rejection> {
    expand_receipt(value).map_err(|error| {
        let kind = if value
            .get("detail")
            .and_then(|detail| detail.get("encoding"))
            .and_then(Value::as_str)
            == Some("superseded")
        {
            RejectionKind::Superseded
        } else {
            RejectionKind::DecodeError
        };
        reject(kind, error)
    })
}
