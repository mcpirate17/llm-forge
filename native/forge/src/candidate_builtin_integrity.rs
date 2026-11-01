use super::*;
use crate::candidate_source::{Change, TreeEntry};
use conductor_native::candidate_unicode::{casefold, python_repr};
use std::{collections::BTreeMap, io};

pub(super) fn prepare(
    selection: &Selection,
    source: &Source,
    directory: &Dir,
    cancel: Option<&Cancellation>,
) -> Result<Prepared> {
    let entries = source.list_tree(&selection.candidate.tree_oid)?;
    let by_path: BTreeMap<_, _> = entries
        .iter()
        .map(|entry| (entry.path.as_str(), entry))
        .collect();
    let mut budget = Budget::default();
    let mut rows = Vec::with_capacity(entries.len());
    for entry in &entries {
        live(cancel)?;
        let folded = casefold(&entry.path)?;
        let repr = python_repr(&entry.path)?;
        budget.add(entry.path.len() + entry.mode.len() + folded.len() + repr.len() + 80)?;
        rows.push(json!({"path":entry.path,"mode":entry.mode,"folded":folded,"repr":repr}));
    }
    let mut changes = Vec::with_capacity(selection.candidate.changes.len());
    let mut files = Vec::with_capacity(changes.capacity());
    for change in &selection.candidate.changes {
        live(cancel)?;
        let mut row = change_row(change)?;
        if !change.deleted() {
            if let Some(entry) = by_path.get(change.path.as_str()) {
                materialized(directory, entry, &mut row)?;
            }
        }
        budget.add(crate::json_canon::encoded_size(&row, MAX_INPUT_BYTES)?)?;
        budget.add(change.path.len() + 8)?;
        changes.push(row);
        files.push(change.path.clone());
    }
    files.sort_unstable();
    files.dedup();
    Ok(Prepared {
        payload: json!({"entries":rows,"changes":changes,"surface":selection.surface,
        "protected_globs":selection.policy.document["protected_delete_globs"],
        "max_file_bytes":selection.policy.document["max_file_bytes"],
        "max_binary_bytes":selection.policy.document["max_binary_bytes"]}),
        files,
    })
}
fn change_row(change: &Change) -> Result<Value> {
    let mut row = serde_json::to_value(change)?;
    row["deleted"] = json!(change.deleted());
    Ok(row)
}
fn materialized(directory: &Dir, entry: &TreeEntry, row: &mut Value) -> Result<()> {
    let metadata = match directory.symlink_metadata(&entry.path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            row["exists"] = json!(false);
            return Ok(());
        }
        Err(error) => {
            return Err(error).with_context(|| format!("inspect candidate path {}", entry.path))
        }
    };
    row["exists"] = json!(true);
    if entry.mode == "120000" {
        ensure!(metadata.is_symlink(), "candidate symlink changed type");
        let target = directory.read_link(&entry.path)?;
        let target = target
            .to_str()
            .context("candidate symlink target is not UTF-8")?;
        ensure!(
            target.len() <= 4096,
            "candidate symlink target exceeds 4096 bytes"
        );
        row["target"] = json!(target);
    } else {
        ensure!(
            metadata.is_file() && entry.size == Some(metadata.len()),
            "candidate file type or size differs from Git object: {}",
            entry.path
        );
        row["size"] = json!(metadata.len());
    }
    Ok(())
}
