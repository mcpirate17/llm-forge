//! Preflight every path and symlink before writing any candidate data.
use super::{Limits, Plan};
use crate::candidate_source::{Source, TreeEntry};
use anyhow::{ensure, Context, Result};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

fn path_parts(path: &str) -> Result<Vec<&str>> {
    crate::candidate_source::safe_path(path.as_bytes())?;
    let parts: Vec<_> = path.split('/').collect();
    ensure!(parts.len() <= 256, "candidate path exceeds 256 components");
    ensure!(
        !parts.iter().any(|part| part.eq_ignore_ascii_case(".git")),
        "candidate snapshots cannot contain Git metadata paths"
    );
    Ok(parts)
}

pub(super) fn prepare(source: &Source, tree: &str, limits: Limits) -> Result<Plan> {
    limits.validate()?;
    ensure!(
        tree.len() == 40 && tree.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "candidate snapshot requires a complete SHA-1 tree OID"
    );
    let entries = source.list_tree(tree)?;
    let mut paths = BTreeSet::new();
    let mut bytes = 0u64;
    let mut files = 0u64;
    let mut skipped = 0u64;
    for entry in &entries {
        path_parts(&entry.path)?;
        ensure!(
            paths.insert(entry.path.clone()),
            "duplicate candidate tree path"
        );
        if entry.mode == "160000" {
            ensure!(
                entry.object_type == "commit",
                "invalid candidate gitlink type"
            );
            skipped += 1;
            continue;
        }
        ensure!(
            matches!(entry.mode.as_str(), "100644" | "100755" | "120000")
                && entry.object_type == "blob",
            "unsupported candidate entry mode/type"
        );
        let size = entry.size.context("candidate blob size is missing")?;
        ensure!(
            size <= limits.max_blob_bytes,
            "candidate blob exceeds materialization limit: {}",
            entry.path
        );
        bytes = bytes
            .checked_add(size)
            .context("candidate byte budget overflow")?;
        ensure!(
            bytes <= limits.max_tree_bytes,
            "candidate tree exceeds materialization byte limit"
        );
        files += 1;
    }
    for path in &paths {
        let prefix = format!("{path}/");
        ensure!(
            !paths
                .range(prefix.clone()..)
                .next()
                .is_some_and(|next| next.starts_with(&prefix)),
            "candidate file is also a parent directory"
        );
    }
    let links = read_links(source, &entries)?;
    let modes: BTreeMap<_, _> = entries
        .iter()
        .map(|entry| (entry.path.as_str(), entry.mode.as_str()))
        .collect();
    let mut budget = ExpansionBudget::default();
    for (path, target) in &links {
        resolve_link(path, target, &links, &modes, &mut budget, false)?;
    }
    Ok(Plan {
        entries,
        links,
        bytes,
        files,
        skipped,
    })
}

fn read_links(source: &Source, entries: &[TreeEntry]) -> Result<BTreeMap<String, String>> {
    let mut links = BTreeMap::new();
    for entry in entries.iter().filter(|entry| entry.mode == "120000") {
        ensure!(
            entry.size.is_some_and(|size| size <= 4096),
            "candidate symlink exceeds 4096 bytes"
        );
        let blob = source
            .repository()
            .find_blob(git2::Oid::from_str(&entry.oid)?)?;
        ensure!(
            Some(blob.size() as u64) == entry.size,
            "candidate symlink size changed"
        );
        let target =
            std::str::from_utf8(blob.content()).context("candidate symlink must be UTF-8")?;
        ensure!(
            !target.is_empty() && !target.starts_with('/') && !target.contains('\0'),
            "candidate symlink must have a nonempty relative target without NUL"
        );
        links.insert(entry.path.clone(), target.to_owned());
    }
    Ok(links)
}

#[derive(Default)]
struct ExpansionBudget {
    steps: usize,
    bytes: usize,
}

fn resolve_link(
    path: &str,
    target: &str,
    links: &BTreeMap<String, String>,
    modes: &BTreeMap<&str, &str>,
    budget: &mut ExpansionBudget,
    capture: bool,
) -> Result<Option<(String, Vec<String>)>> {
    let mut resolved: Vec<&str> = path.split('/').collect();
    resolved.pop();
    let mut resolved: Vec<String> = resolved.into_iter().map(str::to_owned).collect();
    let mut pending: VecDeque<String> = target.split('/').map(str::to_owned).collect();
    let mut followed = usize::from(!path.is_empty());
    let mut traversed = Vec::new();
    let mut examined = 0usize;
    while let Some(part) = pending.pop_front() {
        examined += 1;
        budget.steps += 1;
        ensure!(
            budget.steps <= 1_000_000,
            "candidate symlink aggregate expansion limit exceeded"
        );
        ensure!(
            examined <= 16_384,
            "candidate symlink expansion limit exceeded"
        );
        match part.as_str() {
            "" | "." => continue,
            ".." => {
                ensure!(
                    resolved.pop().is_some(),
                    "candidate symlink escapes snapshot: {path}"
                );
                continue;
            }
            _ => ensure!(
                !part.eq_ignore_ascii_case(".git"),
                "candidate symlink targets Git metadata"
            ),
        }
        resolved.push(part);
        ensure!(
            resolved.len() <= 256,
            "candidate symlink exceeds path depth"
        );
        let current = resolved.join("/");
        budget.bytes += current.len();
        ensure!(
            budget.bytes <= 64 * 1024 * 1024,
            "candidate symlink aggregate path budget exceeded"
        );
        ensure!(
            current.len() <= 4096,
            "candidate symlink expanded path exceeds 4096 bytes"
        );
        if let Some(next) = links.get(&current) {
            if capture {
                traversed.push(current.clone());
            }
            followed += 1;
            ensure!(
                followed <= 40,
                "candidate symlink loop or chain exceeds 40 links: {path}"
            );
            resolved.pop();
            for component in next.split('/').rev() {
                pending.push_front(component.to_owned());
            }
        } else if !pending.is_empty() {
            ensure!(
                !modes
                    .get(current.as_str())
                    .is_some_and(|mode| matches!(*mode, "100644" | "100755")),
                "candidate symlink traverses a regular file: {path}"
            );
        }
    }
    Ok(capture.then(|| (resolved.join("/"), traversed)))
}

pub(super) fn resolve_captured_file(
    source: &Source,
    entries: &[TreeEntry],
    path: &str,
) -> Result<(String, Vec<(String, String)>)> {
    crate::candidate_source::safe_path(path.as_bytes())?;
    if entries
        .iter()
        .any(|entry| entry.path == path && matches!(entry.mode.as_str(), "100644" | "100755"))
    {
        return Ok((path.to_owned(), Vec::new()));
    }
    let mut symlinks = Vec::new();
    let mut total = 0u64;
    for entry in entries.iter().filter(|entry| entry.mode == "120000") {
        ensure!(
            symlinks.len() < 4096,
            "candidate policy symlink inventory exceeds 4096"
        );
        total = total
            .checked_add(entry.size.unwrap_or(u64::MAX))
            .context("candidate symlink byte overflow")?;
        ensure!(
            total <= 16 * 1024 * 1024,
            "candidate policy symlink inventory exceeds 16 MiB"
        );
        symlinks.push(entry.clone());
    }
    let links = read_links(source, &symlinks)?;
    let modes = entries
        .iter()
        .map(|entry| (entry.path.as_str(), entry.mode.as_str()))
        .collect();
    let (resolved, followed) = resolve_link(
        "",
        path,
        &links,
        &modes,
        &mut ExpansionBudget::default(),
        true,
    )?
    .context("captured policy resolution missing")?;
    let selected = followed
        .into_iter()
        .map(|path| {
            let target = links
                .get(&path)
                .context("captured policy symlink disappeared")?
                .clone();
            Ok((path, target))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok((resolved, selected))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aggregate_expansion_budgets_apply_across_links() {
        let links = BTreeMap::new();
        let modes = BTreeMap::new();
        let mut steps = ExpansionBudget {
            steps: 999_999,
            bytes: 0,
        };
        resolve_link("first", "file", &links, &modes, &mut steps, false).unwrap();
        assert!(resolve_link("second", "file", &links, &modes, &mut steps, false).is_err());
        let mut bytes = ExpansionBudget {
            steps: 0,
            bytes: 64 * 1024 * 1024 - 4,
        };
        resolve_link("first", "file", &links, &modes, &mut bytes, false).unwrap();
        assert!(resolve_link("second", "file", &links, &modes, &mut bytes, false).is_err());
    }
}
