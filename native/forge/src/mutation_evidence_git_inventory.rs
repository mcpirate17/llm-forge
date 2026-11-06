//! Read-only libgit2 mutation-test inventory, with held Rust source classification.
use crate::{
    candidate_source::Source, mutation_evidence_inventory::Selection,
    mutation_live_inputs::LiveInputs,
};
use anyhow::{ensure, Context, Result};
use git2::{Delta, Diff, DiffFindOptions, DiffOptions, Status, StatusOptions, StatusShow};
use std::collections::{BTreeSet, HashSet};

const MAX_PATHS: usize = 100_000;
fn add(paths: &mut BTreeSet<String>, raw: &[u8]) -> Result<()> {
    let path = crate::candidate_source::safe_path(raw)?;
    paths.insert(path.to_owned());
    ensure!(
        paths.len() <= MAX_PATHS,
        "Git inventory exceeds 100000 paths"
    );
    Ok(())
}
fn index(source: &Source, paths: &mut BTreeSet<String>, all: bool) -> Result<()> {
    let index = source.repository().index()?;
    ensure!(
        !index.has_conflicts(),
        "mutation inventory refuses a conflicted index"
    );
    ensure!(index.len() <= MAX_PATHS, "Git index exceeds 100000 entries");
    if all {
        for entry in index.iter() {
            add(paths, &entry.path)?;
        }
    }
    Ok(())
}
fn diff_paths(mut diff: Diff<'_>, paths: &mut BTreeSet<String>, base: bool) -> Result<()> {
    ensure!(
        diff.deltas().len() <= MAX_PATHS,
        "Git diff exceeds 100000 entries"
    );
    let mut bytes = 0u64;
    for delta in diff.deltas() {
        for file in [delta.old_file(), delta.new_file()] {
            let size = file.size();
            ensure!(
                size <= 64 * 1024 * 1024,
                "Git similarity input exceeds 64 MiB"
            );
            bytes = bytes
                .checked_add(size)
                .context("Git similarity input size overflow")?;
            ensure!(
                bytes <= 2 * 1024 * 1024 * 1024,
                "Git similarity input exceeds 2 GiB"
            );
        }
    }
    diff.find_similar(Some(
        DiffFindOptions::new().renames(true).rename_limit(1000),
    ))?;
    for delta in diff.deltas() {
        if base
            && !matches!(
                delta.status(),
                Delta::Added | Delta::Copied | Delta::Modified | Delta::Renamed
            )
        {
            continue;
        }
        let file = if delta.status() == Delta::Deleted {
            delta.old_file()
        } else {
            delta.new_file()
        };
        add(
            paths,
            file.path_bytes().context("diff path is unavailable")?,
        )?;
    }
    Ok(())
}
fn changed(source: &Source, paths: &mut BTreeSet<String>, base: Option<&str>) -> Result<()> {
    let repo = source.repository();
    let head = repo.head()?.peel_to_commit()?;
    let mut options = DiffOptions::new();
    options.include_typechange(true);
    let diff = if let Some(base) = base {
        ensure!(
            !base.is_empty() && !base.starts_with('-') && !base.contains('\0'),
            "invalid base reference"
        );
        let base = repo.revparse_single(base)?.peel_to_commit()?;
        let bases = repo.merge_bases(base.id(), head.id())?;
        ensure!(
            bases.len() == 1,
            "base and HEAD must have exactly one merge base"
        );
        let old = repo
            .find_commit(*bases.first().context("merge base missing")?)?
            .tree()?;
        let new = head.tree()?;
        repo.diff_tree_to_tree(Some(&old), Some(&new), Some(&mut options))?
    } else {
        let tree = head.tree()?;
        repo.diff_tree_to_workdir_with_index(Some(&tree), Some(&mut options))?
    };
    diff_paths(diff, paths, base.is_some())
}
fn untracked(source: &Source, paths: &mut BTreeSet<String>) -> Result<()> {
    let mut options = StatusOptions::new();
    options
        .show(StatusShow::Workdir)
        .include_untracked(true)
        .recurse_untracked_dirs(true)
        .include_ignored(false);
    let statuses = source.repository().statuses(Some(&mut options))?;
    ensure!(
        statuses.len() <= MAX_PATHS,
        "Git workdir inventory exceeds 100000 paths"
    );
    for entry in statuses.iter() {
        if entry.status().contains(Status::WT_NEW) {
            add(paths, entry.path_bytes())?;
        }
    }
    Ok(())
}
pub(crate) fn select(
    source: &Source,
    capture: &mut LiveInputs,
    selection: &Selection,
    include_untracked: bool,
    skip: &[&str],
) -> Result<Vec<String>> {
    if let Selection::Explicit(paths) = selection {
        return paths
            .iter()
            .map(|path| {
                conductor_native::mutation_evidence::normalize_path(path)
                    .map_err(anyhow::Error::msg)
            })
            .collect();
    }
    let mut paths = BTreeSet::new();
    index(
        source,
        &mut paths,
        matches!(selection, Selection::Coverage | Selection::Canary),
    )?;
    if let Selection::Changed(base) = selection {
        changed(source, &mut paths, base.as_deref())?;
    }
    if include_untracked || matches!(selection, Selection::Changed(None)) {
        untracked(source, &mut paths)?;
    }
    let patterns = conductor_native::mutation_evidence::canonical_test_patterns();
    let skip: HashSet<_> = skip.iter().copied().collect();
    let mut selected = Vec::new();
    for path in paths {
        let Some(glob) =
            conductor_native::mutation_coverage::inventory_name_match(&path, &patterns, &skip)
        else {
            continue;
        };
        let rust = if !glob && path.ends_with(".rs") {
            capture
                .read(&path)?
                .map(|bytes| {
                    std::str::from_utf8(bytes)
                        .with_context(|| format!("Rust inventory input is not UTF-8: {path}"))
                        .map(conductor_native::mutation_coverage::inventory_rust_source)
                })
                .transpose()?
                .unwrap_or(false)
        } else {
            false
        };
        if glob || rust {
            selected.push(path);
        }
    }
    Ok(selected)
}

#[cfg(test)]
#[path = "mutation_evidence_git_inventory_tests.rs"]
mod tests;
