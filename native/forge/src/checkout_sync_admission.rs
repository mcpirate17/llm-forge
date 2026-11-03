//! Pure, bounded path admission for a captured checkout transaction.
//! The caller supplies inventories captured under the shared writer lease;
//! ignored names must include all leaves and empty directories.
use crate::checkout_sync_reconcile::{self, Collision, Entry};
use anyhow::{bail, ensure, Result};
use std::collections::{BTreeMap, BTreeSet};

/// An expected semantic refusal (CLI exit 3), distinct from validation and
/// resource/internal errors (CLI exit 2). Only actual conflicts construct it.
#[derive(Debug)]
pub(crate) struct Blocked(pub String);
impl std::fmt::Display for Blocked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for Blocked {}

#[derive(Debug)]
pub(crate) struct PlannedPath {
    pub path: String,
    pub index: Option<Entry>,
    pub worktree: Option<Entry>,
}

/// `worktree` must contain admitted tracked/untracked leaf entries. `ignored`
/// lists ignored leaves and empty directories absent from the recovery snapshot.
/// A directory collision includes its children, not only an exact filename match.
pub(crate) fn plan(
    base: &BTreeMap<String, Entry>,
    index: &BTreeMap<String, Entry>,
    worktree: &BTreeMap<String, Entry>,
    incoming: &BTreeMap<String, Entry>,
    ignored: &BTreeSet<String>,
) -> Result<Vec<PlannedPath>> {
    ensure!(
        base.len() + index.len() + worktree.len() + incoming.len() + ignored.len() <= 500_000,
        "checkout inventory exceeds bound"
    );
    let mut names = BTreeSet::new();
    for map in [base, index, worktree, incoming] {
        names.extend(map.keys().cloned());
    }
    let untracked: BTreeSet<String> = worktree
        .keys()
        .filter(|p| !base.contains_key(*p) && !index.contains_key(*p))
        .cloned()
        .collect();
    let mut changes = Vec::new();
    for path in names {
        validate(&path)?;
        let b = base.get(&path);
        let i = index.get(&path);
        let w = worktree.get(&path);
        let n = incoming.get(&path);
        // Only an incoming change can replace or absorb another name. Ignored
        // directory parents are normal, but an ignored leaf at or below a new
        // tracked leaf would be destroyed by a type transition.
        if n != b && n.is_some() {
            if let Some(hidden) = intersecting(ignored, &path, true) {
                bail!(Blocked(format!("ignored path collision: {hidden}")));
            }
            if let Some(existing) = intersecting(&untracked, &path, false) {
                bail!(Blocked(format!(
                    "untracked path or ancestor collision: {existing}"
                )));
            }
        }
        let collision = if b.is_none() && i.is_none() && w.is_some() {
            Collision::Untracked
        } else {
            Collision::None
        };
        let result = checkout_sync_reconcile::reconcile(b, i, w, n, collision)?
            .map_err(|conflict| Blocked(format!("checkout conflict at {path}: {conflict:?}")))?;
        if result.index.as_ref() != i || result.worktree.as_ref() != w {
            changes.push(PlannedPath {
                path,
                index: result.index,
                worktree: result.worktree,
            });
        }
    }
    Ok(changes)
}
fn intersecting<'a>(names: &'a BTreeSet<String>, path: &str, exact: bool) -> Option<&'a str> {
    if exact {
        if let Some(name) = names.get(path) {
            return Some(name);
        }
    }
    for (offset, _) in path.match_indices('/') {
        if let Some(name) = names.get(&path[..offset]) {
            return Some(name);
        }
    }
    let prefix = format!("{path}/");
    names
        .range(prefix.clone()..)
        .next()
        .filter(|p| p.starts_with(&prefix))
        .map(String::as_str)
}
fn validate(path: &str) -> Result<()> {
    ensure!(
        !path.is_empty()
            && path.len() <= 4096
            && !path.starts_with('/')
            && !path.contains('\0')
            && path
                .split('/')
                .all(|p| !p.is_empty() && p != "." && p != ".." && !p.eq_ignore_ascii_case(".git")),
        "unsafe checkout path"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn f(bytes: &[u8]) -> Entry {
        Entry::File {
            executable: false,
            bytes: bytes.to_vec(),
        }
    }
    #[test]
    fn preserves_staged_and_unstaged_layers_and_refuses_ignored_descendants() {
        let base = BTreeMap::from([("a".into(), f(b"one\ntwo\nthree\nfour\nfive\n"))]);
        let staged = BTreeMap::from([("a".into(), f(b"ONE\ntwo\nthree\nfour\nfive\n"))]);
        let work = BTreeMap::from([("a".into(), f(b"ONE\ntwo\nthree\nfour\nFIVE\n"))]);
        let incoming = BTreeMap::from([("a".into(), f(b"one\ntwo\nTHREE\nfour\nfive\n"))]);
        let changes = plan(&base, &staged, &work, &incoming, &BTreeSet::new()).unwrap();
        assert_eq!(changes[0].index, Some(f(b"ONE\ntwo\nTHREE\nfour\nfive\n")));
        assert_eq!(
            changes[0].worktree,
            Some(f(b"ONE\ntwo\nTHREE\nfour\nFIVE\n"))
        );
        assert!(plan(
            &base,
            &staged,
            &work,
            &incoming,
            &BTreeSet::from(["a/secret".into()])
        )
        .is_err());
    }
    #[test]
    fn identical_untracked_is_admitted_but_different_is_blocked() {
        let empty = BTreeMap::new();
        let work = BTreeMap::from([("x".into(), f(b"one"))]);
        assert!(plan(&empty, &empty, &work, &work, &BTreeSet::new()).is_ok());
        let incoming = BTreeMap::from([("x".into(), f(b"two"))]);
        assert!(plan(&empty, &empty, &work, &incoming, &BTreeSet::new()).is_err());
    }
    #[test]
    fn untracked_ancestor_or_descendant_blocks_type_transition() {
        let empty = BTreeMap::new();
        let work = BTreeMap::from([("a/peer".into(), f(b"keep"))]);
        let incoming = BTreeMap::from([("a".into(), f(b"file"))]);
        assert!(plan(&empty, &empty, &work, &incoming, &BTreeSet::new()).is_err());
        let work = BTreeMap::from([("a".into(), f(b"keep"))]);
        let incoming = BTreeMap::from([("a/peer".into(), f(b"file"))]);
        assert!(plan(&empty, &empty, &work, &incoming, &BTreeSet::new()).is_err());
    }
    #[test]
    fn only_semantic_collisions_and_merge_conflicts_are_typed_blocked() {
        let empty = BTreeMap::new();
        let incoming = BTreeMap::from([("a".into(), f(b"incoming\n"))]);
        let ignored = BTreeSet::from(["a/secret".into()]);
        let error = plan(&empty, &empty, &empty, &incoming, &ignored).unwrap_err();
        assert!(error.downcast_ref::<Blocked>().is_some());
        assert!(error.to_string().contains("ignored path collision"));

        let untracked = BTreeMap::from([("a/peer".into(), f(b"keep\n"))]);
        let error = plan(&empty, &empty, &untracked, &incoming, &BTreeSet::new()).unwrap_err();
        assert!(error.downcast_ref::<Blocked>().is_some());
        assert!(error
            .to_string()
            .contains("untracked path or ancestor collision"));

        let untracked = BTreeMap::from([("a".into(), f(b"peer\n"))]);
        let error = plan(&empty, &empty, &untracked, &incoming, &BTreeSet::new()).unwrap_err();
        assert!(error.downcast_ref::<Blocked>().is_some());
        assert!(error.to_string().contains("UntrackedCollision"));

        let base = BTreeMap::from([("a".into(), f(b"base\n"))]);
        let staged = BTreeMap::from([("a".into(), f(b"local\n"))]);
        let error = plan(&base, &staged, &staged, &incoming, &BTreeSet::new()).unwrap_err();
        assert!(error.downcast_ref::<Blocked>().is_some());
        assert!(error.to_string().contains("Index"));
    }
}
