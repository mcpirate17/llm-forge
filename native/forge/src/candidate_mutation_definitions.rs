//! Actual candidate/base test definitions and fixed grandfather Git inventory.
use super::*;
use std::collections::BTreeMap;
pub(super) const ANCHOR_COMMIT: &str = "61343f575215dd222a74fc2c060d0328692ded5e";
pub(super) const ANCHOR_TREE: &str = "b01877ba62c32445f7649450f9b395dd70de306a";
const INVENTORY: &str = "conductor/candidate_review/grandfathered_test_nodeids_61343f57.json";
const INVENTORY_SHA: &str = "aa0a89e12c7a43e9dc3f509f5540ed67df112462cb9fd6cafe060c596f3414d8";
pub(super) fn legacy(spec: &crate::candidate_execution_workspace::MutationGrandfather) -> bool {
    spec.commit == ANCHOR_COMMIT
        && spec.tree == ANCHOR_TREE
        && spec.inventory_path == INVENTORY
        && spec.inventory_sha256 == INVENTORY_SHA
}
pub(super) fn grandfather(
    inputs: &mut Inputs<'_>,
    git: &mut GitInputs<'_>,
    candidate: &Candidate,
    spec: Option<&crate::candidate_execution_workspace::MutationGrandfather>,
) -> Result<Value> {
    let Some(spec) = spec else {
        return Ok(json!({}));
    };
    spec.validate()?;
    let raw = inputs
        .read_bytes_with_limit(&spec.inventory_path, 2 * 1024 * 1024)?
        .to_vec();
    ensure!(
        hex(&raw) == spec.inventory_sha256,
        "grandfather inventory differs from SHA bound at fixed anchor"
    );
    let base = candidate
        .base_commit_oid
        .as_deref()
        .context("grandfather baseline requires a candidate base commit")?;
    git.anchor_ancestor(&spec.commit, &spec.tree, base)?;
    let paths: Vec<_> = git
        .entries(&spec.tree)?
        .keys()
        .filter(|path| {
            path.ends_with(".py")
                && Path::new(path)
                    .file_name()
                    .is_some_and(|name| name.as_encoded_bytes().starts_with(b"test_"))
        })
        .cloned()
        .collect();
    ensure!(
        paths.len() <= 4096,
        "grandfather inventory exceeds4096 Python files"
    );
    let mut derived = Map::new();
    for path in paths {
        let text = std::str::from_utf8(git.read(&spec.tree, &path)?)?;
        let definitions =
            conductor_native::candidate_verification::python_test_definitions(text, &path)
                .map_err(anyhow::Error::msg)?;
        let keys: Vec<_> = definitions
            .as_object()
            .context("Python definition map missing")?
            .keys()
            .cloned()
            .collect();
        if !keys.is_empty() {
            derived.insert(path, json!(keys));
        }
    }
    let payload =
        crate::tool_schema::parse_document_with_limit(std::str::from_utf8(&raw)?, 2 * 1024 * 1024)?;
    let milestone = payload["milestone"]
        .as_str()
        .filter(|v| !v.is_empty() && v.len() <= 256 && !v.chars().any(char::is_control))
        .context("grandfather inventory milestone missing or invalid")?;
    let present: Vec<_> = inputs
        .entries()
        .iter()
        .filter(|(_, entry)| {
            entry.object_type == "blob" && matches!(entry.mode.as_str(), "100644" | "100755")
        })
        .map(|(path, _)| path)
        .collect();
    decide(
        "inventory_validate",
        &json!({"payload":payload,"expected_schema":"conductor.candidate_review.grandfather_inventory/v1",
        "anchor_commit":spec.commit,"milestone":milestone,"derived":derived,
        "present_paths":present,"dead_paths":if legacy(spec) { DEAD_PATHS } else { &[] }}),
    )
}
pub(super) fn value_test(change: &crate::candidate_source::Change) -> bool {
    change.classes.iter().any(|class| class == "native")
        || change.path.ends_with(".py") && change.classes.iter().any(|class| class == "test")
}
pub(super) fn gated(
    inputs: &mut Inputs<'_>,
    git: &mut GitInputs<'_>,
    _snapshot: &Snapshot,
    candidate: &Candidate,
    grandfathered: &Value,
    strict: bool,
) -> Result<(Value, Vec<Value>)> {
    let mut entries = Vec::new();
    let rust_needed = candidate
        .changes
        .iter()
        .any(|change| !change.deleted() && value_test(change) && change.path.ends_with(".rs"));
    let current_rust = if rust_needed {
        rust_files(inputs)?
    } else {
        BTreeMap::new()
    };
    let base_rust = if rust_needed && !strict {
        base_rust_files(git, &candidate.base_tree_oid)?
    } else {
        BTreeMap::new()
    };
    let mut notes = Vec::new();
    for change in candidate.changes.iter().filter(|change| !change.deleted()) {
        let mut row = json!({"path":change.path,"classes":change.classes,"old_mode":change.old_mode,"old_oid":change.old_oid});
        if strict {
            row["old_mode"] = json!("000000");
            row["old_oid"] = json!("0000000000000000000000000000000000000000");
        }
        if value_test(change) {
            row["definitions"] = definitions(
                inputs.read(&change.path)?,
                &change.path,
                Path::new("/"),
                &current_rust,
            )?;
            row["base"] = if strict || change.old_mode == "000000" {
                Value::Null
            } else {
                let path = change.old_path.as_deref().unwrap_or(&change.path);
                match git
                    .read(&candidate.base_tree_oid, path)
                    .and_then(|bytes| Ok(std::str::from_utf8(bytes)?.to_owned()))
                    .and_then(|source| definitions(&source, path, Path::new("/"), &base_rust))
                {
                    Ok(value) => value,
                    Err(error) => {
                        notes.push(json!({"path":path,"error":format!("{error:#}"),"fallback":"all current definitions gated"}));
                        Value::Null
                    }
                }
            };
        }
        entries.push(row);
    }
    Ok((
        decide(
            "gated_nodeids",
            &json!({"entries":entries,"grandfathered":grandfathered}),
        )?,
        notes,
    ))
}
fn definitions(
    source: &str,
    path: &str,
    root: &Path,
    files: &BTreeMap<PathBuf, Vec<u8>>,
) -> Result<Value> {
    if path.ends_with(".py") {
        return conductor_native::candidate_verification::python_test_definitions(source, path)
            .map_err(anyhow::Error::msg);
    }
    if !path.ends_with(".rs") {
        conductor_native::syntax::parse(source, conductor_native::syntax::Language::Cpp)?;
    }
    conductor_native::candidate_verification::native_test_definitions_captured(
        source, path, root, files,
    )
    .map_err(anyhow::Error::msg)
}
fn rust_files(inputs: &mut Inputs<'_>) -> Result<BTreeMap<PathBuf, Vec<u8>>> {
    let paths: Vec<_> = inputs
        .entries()
        .keys()
        .filter(|path| {
            path.ends_with(".rs")
                || Path::new(path)
                    .file_name()
                    .is_some_and(|name| name == "Cargo.toml")
        })
        .cloned()
        .collect();
    ensure!(
        paths.len() <= 4096,
        "captured Rust identity inventory exceeds4096 files"
    );
    let mut files = BTreeMap::new();
    for path in paths {
        files.insert(
            Path::new("/").join(&path),
            inputs.read_bytes_with_limit(&path, 1024 * 1024)?.to_vec(),
        );
    }
    Ok(files)
}
fn base_rust_files(git: &mut GitInputs<'_>, tree: &str) -> Result<BTreeMap<PathBuf, Vec<u8>>> {
    let paths: Vec<_> = git
        .entries(tree)?
        .keys()
        .filter(|path| {
            path.ends_with(".rs")
                || Path::new(path)
                    .file_name()
                    .is_some_and(|name| name == "Cargo.toml")
        })
        .cloned()
        .collect();
    ensure!(
        paths.len() <= 4096,
        "base Rust identity inventory exceeds4096 files"
    );
    let mut files = BTreeMap::new();
    for path in paths {
        files.insert(Path::new("/").join(&path), git.read(tree, &path)?.to_vec());
    }
    Ok(files)
}

const DEAD_PATHS: &[&str] = &[
    "research/tests/test_nm_f6_avo_fused_ce_battery.py",
    "research/tests/test_nm_f6_partitioned_head_ce.py",
    "research/tests/test_nm_f6_phase22_20k_continuation.py",
    "research/tests/test_nm_f6_phase22_chinchilla.py",
    "research/tests/test_nm_f6_phase22_chinchilla_minimal_battery_sweep.py",
    "research/tests/test_nm_f6_phase22_chinchilla_ordered_path_knockout.py",
    "research/tests/test_nm_f6_phase22_chinchilla_read_write_trajectory.py",
    "research/tests/test_nm_f6_phase22_chinchilla_terminal_stage1.py",
    "research/tests/test_nm_f6_phase22_minimal_battery.py",
    "research/tests/test_nm_f6_phase22_ordered_path_knockout.py",
    "research/tests/test_nm_f6_phase22_partitioned_head_training.py",
    "research/tests/test_nm_f6_phase22_postrun_validation.py",
];
