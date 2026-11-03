//! Build a complete path and index plan before publishing any checkout path.
pub(crate) use crate::checkout_sync_admission::Blocked;
use crate::{
    candidate_commit_snapshot::Recovery,
    candidate_source::Source,
    checkout_sync_admission::{self, PlannedPath},
    checkout_sync_eol::Policy,
    checkout_sync_inventory,
    checkout_sync_reconcile::Entry,
    checkout_sync_tree,
    checkout_sync_worktree::{Content, SyncLease},
};
use anyhow::{ensure, Result};
use git2::Oid;
use std::collections::{BTreeMap, BTreeSet};

pub(crate) struct Op {
    pub path: String,
    pub before: Option<Entry>,
    pub before_dir: bool,
    pub after: Option<Content>,
}
pub(crate) struct Plan {
    pub index: Vec<PlannedPath>,
    pub operations: Vec<Op>,
    pub policies: BTreeMap<String, Policy>,
    pub inventory: checkout_sync_inventory::Inventory,
    pub tracked: BTreeSet<String>,
}

pub(crate) fn build(
    source: &Source,
    lease: &SyncLease,
    snapshot: &Recovery,
    old: Oid,
    new: Oid,
) -> Result<Plan> {
    let repo = source.repository();
    let mut total = 0;
    let base = checkout_sync_tree::load(source, repo.find_commit(old)?.tree_id(), &mut total)?;
    let staged = checkout_sync_tree::load(source, snapshot.staged_tree, &mut total)?;
    let raw = checkout_sync_tree::load(source, snapshot.worktree_tree, &mut total)?;
    let incoming = checkout_sync_tree::load(source, repo.find_commit(new)?.tree_id(), &mut total)?;
    let tracked: BTreeSet<_> = base.keys().chain(staged.keys()).cloned().collect();
    let inventory = checkout_sync_inventory::inventory(source, lease, &tracked)?;
    from_maps(source, lease, &base, &staged, &raw, &incoming, inventory)
}

/// The preview and executor share reconciliation, filters and operation ordering.
/// Maps use canonical Git bytes except `raw`, which contains worktree bytes.
pub(crate) fn from_maps(
    source: &Source,
    lease: &SyncLease,
    base: &BTreeMap<String, Entry>,
    staged: &BTreeMap<String, Entry>,
    raw: &BTreeMap<String, Entry>,
    incoming: &BTreeMap<String, Entry>,
    inventory: checkout_sync_inventory::Inventory,
) -> Result<Plan> {
    let tracked = base.keys().chain(staged.keys()).cloned().collect();
    let mut collisions = inventory.ignored.clone();
    collisions.extend(inventory.empty.iter().cloned());
    let mut policies = BTreeMap::new();
    let mut work = raw.clone();
    for (path, entry) in &mut work {
        if let Entry::File { bytes, .. } = entry {
            let policy = Policy::capture(source, lease, path)?;
            *bytes = policy.convert(source, lease, path, bytes, true)?;
            policies.insert(path.clone(), policy);
        }
    }
    let index = checkout_sync_admission::plan(base, staged, &work, incoming, &collisions)?;
    let mut desired = work.clone();
    for change in &index {
        if change.path == ".gitattributes" || change.path.ends_with("/.gitattributes") {
            ensure!(
                change.worktree.as_ref() == work.get(&change.path),
                Blocked(format!(
                    "changing effective .gitattributes policy is unsupported: {}",
                    change.path
                ))
            );
        }
        match &change.worktree {
            Some(entry) => {
                desired.insert(change.path.clone(), entry.clone());
            }
            None => {
                desired.remove(&change.path);
            }
        }
    }
    let mut output = BTreeMap::new();
    for (path, entry) in &desired {
        let entry = if work.get(path) == Some(entry) {
            raw.get(path).cloned().unwrap_or_else(|| entry.clone())
        } else {
            smudge(source, lease, &mut policies, path, entry)?
        };
        output.insert(path.clone(), entry);
    }
    let operations = schedule(raw, &output, &inventory.directories);
    Ok(Plan {
        index,
        operations,
        policies,
        inventory,
        tracked,
    })
}
fn smudge(
    source: &Source,
    lease: &SyncLease,
    policies: &mut BTreeMap<String, Policy>,
    path: &str,
    entry: &Entry,
) -> Result<Entry> {
    match entry {
        Entry::Symlink(bytes) => Ok(Entry::Symlink(bytes.clone())),
        Entry::File { executable, bytes } => {
            let policy = Policy::capture(source, lease, path)?;
            let converted = policy.convert(source, lease, path, bytes, false)?;
            policies.insert(path.into(), policy);
            Ok(Entry::File {
                executable: *executable,
                bytes: converted,
            })
        }
    }
}
fn dirs(paths: impl Iterator<Item = String>) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for path in paths {
        for (at, _) in path.match_indices('/') {
            out.insert(path[..at].into());
        }
    }
    out
}
fn content(e: &Entry) -> Content {
    match e {
        Entry::File { executable, bytes } => Content::File {
            bytes: bytes.clone(),
            mode: if *executable { 0o755 } else { 0o644 },
        },
        Entry::Symlink(v) => Content::Symlink(v.clone()),
    }
}
fn schedule(
    raw: &BTreeMap<String, Entry>,
    output: &BTreeMap<String, Entry>,
    actual_dirs: &BTreeSet<String>,
) -> Vec<Op> {
    let target_dirs = dirs(output.keys().cloned());
    let remove_dirs: BTreeSet<_> = actual_dirs
        .iter()
        .filter(|dir| {
            output.contains_key(*dir)
                || dir
                    .match_indices('/')
                    .any(|(at, _)| output.contains_key(&dir[..at]))
        })
        .cloned()
        .collect();
    let mut ops = Vec::new();
    let mut removals: Vec<_> = raw
        .keys()
        .filter(|p| !output.contains_key(*p) && !target_dirs.contains(*p))
        .cloned()
        .collect();
    removals.sort_by_key(|p| std::cmp::Reverse(p.matches('/').count()));
    for path in removals {
        ops.push(Op {
            before: raw.get(&path).cloned(),
            path,
            before_dir: false,
            after: None,
        });
    }
    let mut dirs_remove: Vec<_> = remove_dirs.into_iter().collect();
    dirs_remove.sort_by_key(|p| std::cmp::Reverse(p.matches('/').count()));
    let mut handled = BTreeSet::new();
    for path in dirs_remove {
        let after = output.get(&path).map(content);
        if after.is_some() {
            handled.insert(path.clone());
        }
        ops.push(Op {
            path,
            before: None,
            before_dir: true,
            after,
        });
    }
    let mut dirs_add: Vec<_> = target_dirs.difference(actual_dirs).cloned().collect();
    dirs_add.sort_by_key(|p| p.matches('/').count());
    for path in dirs_add {
        ops.push(Op {
            before: raw.get(&path).cloned(),
            path,
            before_dir: false,
            after: Some(Content::EmptyDirectory { mode: 0o755 }),
        });
    }
    for (path, desired) in output {
        if !handled.contains(path) && raw.get(path) != Some(desired) {
            ops.push(Op {
                path: path.clone(),
                before: raw.get(path).cloned(),
                before_dir: false,
                after: Some(content(desired)),
            });
        }
    }
    ops
}
#[cfg(test)]
mod tests {
    use super::*;
    fn f(v: &[u8]) -> Entry {
        Entry::File {
            executable: false,
            bytes: v.to_vec(),
        }
    }
    #[test]
    fn directory_transitions_are_scheduled_in_safe_order() {
        let old = BTreeMap::from([("a/x".into(), f(b"old"))]);
        let new = BTreeMap::from([("a".into(), f(b"new"))]);
        let ops = schedule(&old, &new, &BTreeSet::from(["a".into()]));
        assert_eq!(
            ops.iter().map(|o| o.path.as_str()).collect::<Vec<_>>(),
            ["a/x", "a"]
        );
        assert!(ops[1].before_dir);
        let ops = schedule(&new, &old, &BTreeSet::new());
        assert_eq!(
            ops.iter().map(|o| o.path.as_str()).collect::<Vec<_>>(),
            ["a", "a/x"]
        );
        assert!(matches!(ops[0].after, Some(Content::EmptyDirectory { .. })));
    }
    #[test]
    fn plan_preserves_validation_errors_as_distinct_from_semantic_refusals() {
        let dir = tempfile::tempdir().unwrap();
        git2::Repository::init(dir.path()).unwrap();
        let source = Source::open(dir.path()).unwrap();
        let lease = SyncLease::acquire(dir.path(), &mut || Ok(())).unwrap();
        let empty = BTreeMap::new();
        // Invalid paths and malformed/oversized targets reach the shared planner
        // without large buffers or object writes. None is a semantic refusal.
        for (path, entry) in [
            ("../escape".into(), f(b"value")),
            ("x".repeat(4097), f(b"value")),
            ("a".into(), Entry::Symlink(b"bad\0target".to_vec())),
            ("a".into(), Entry::Symlink(vec![b'x'; 4097])),
        ] {
            let incoming = BTreeMap::from([(path, entry)]);
            let inventory =
                checkout_sync_inventory::inventory(&source, &lease, &BTreeSet::new()).unwrap();
            let error = from_maps(
                &source, &lease, &empty, &empty, &empty, &incoming, inventory,
            )
            .err()
            .unwrap();
            assert!(error.downcast_ref::<Blocked>().is_none(), "{error:#}");
            assert!(
                error.to_string().contains("unsafe checkout path")
                    || error.to_string().contains("invalid symlink target"),
                "{error:#}"
            );
        }
        let incoming = BTreeMap::from([("a".into(), f(b"incoming"))]);
        let mut inventory =
            checkout_sync_inventory::inventory(&source, &lease, &BTreeSet::new()).unwrap();
        inventory.ignored.insert("a/secret".into());
        let error = from_maps(
            &source, &lease, &empty, &empty, &empty, &incoming, inventory,
        )
        .err()
        .unwrap();
        assert!(error.downcast_ref::<Blocked>().is_some(), "{error:#}");
    }
}
