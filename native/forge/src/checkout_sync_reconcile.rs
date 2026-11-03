//! Pure per-path reconciliation for the shared checkout transaction.
//! No filesystem, index, reference, object database, or process mutation.
//! Inputs must be in one identical, explicitly selected byte representation;
//! callers must not pass raw working-tree bytes where Git filters change them.
use anyhow::{ensure, Result};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Entry {
    File { executable: bool, bytes: Vec<u8> },
    Symlink(Vec<u8>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Collision {
    None,
    Untracked,
    Ignored,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Reconciled {
    pub(crate) index: Option<Entry>,
    pub(crate) worktree: Option<Entry>,
    pub(crate) absorbed_untracked: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Conflict {
    IgnoredCollision,
    UntrackedCollision,
    Index,
    Worktree,
}

/// Independently preserve staged and unstaged deltas. A conflict in either
/// layer refuses the entire path; callers must refuse the entire transaction.
pub(crate) fn reconcile(
    base: Option<&Entry>,
    index: Option<&Entry>,
    worktree: Option<&Entry>,
    incoming: Option<&Entry>,
    collision: Collision,
) -> Result<std::result::Result<Reconciled, Conflict>> {
    for entry in [base, index, worktree, incoming].into_iter().flatten() {
        match entry {
            Entry::File { bytes, .. } => ensure!(
                bytes.len() <= 64 * 1024 * 1024,
                "file exceeds reconciliation limit"
            ),
            Entry::Symlink(target) => ensure!(
                target.len() <= 4096 && !target.contains(&0),
                "invalid symlink target"
            ),
        }
    }
    if collision != Collision::None {
        ensure!(
            base.is_none() && index.is_none() && worktree.is_some(),
            "collision requires a previously untracked present worktree entry"
        );
        if incoming.is_some() {
            if collision == Collision::Ignored {
                return Ok(Err(Conflict::IgnoredCollision));
            }
            if worktree != incoming {
                return Ok(Err(Conflict::UntrackedCollision));
            }
            return Ok(Ok(Reconciled {
                index: incoming.cloned(),
                worktree: incoming.cloned(),
                absorbed_untracked: true,
            }));
        }
        return Ok(Ok(Reconciled {
            index: None,
            worktree: worktree.cloned(),
            absorbed_untracked: false,
        }));
    }
    let Some(index_result) = merge_layer(base, index, incoming)? else {
        return Ok(Err(Conflict::Index));
    };
    let Some(worktree_result) = merge_layer(index, worktree, index_result.as_ref())? else {
        return Ok(Err(Conflict::Worktree));
    };
    Ok(Ok(Reconciled {
        index: index_result,
        worktree: worktree_result,
        absorbed_untracked: false,
    }))
}

// Outer Option signals a conflict; inner Option is a legitimate deletion.
fn merge_layer(
    ancestor: Option<&Entry>,
    ours: Option<&Entry>,
    theirs: Option<&Entry>,
) -> Result<Option<Option<Entry>>> {
    if ours == theirs {
        return Ok(Some(ours.cloned()));
    }
    if ours == ancestor {
        return Ok(Some(theirs.cloned()));
    }
    if theirs == ancestor {
        return Ok(Some(ours.cloned()));
    }
    let (
        Some(Entry::File {
            executable: base_mode,
            bytes: base,
        }),
        Some(Entry::File {
            executable: our_mode,
            bytes: ours,
        }),
        Some(Entry::File {
            executable: their_mode,
            bytes: theirs,
        }),
    ) = (ancestor, ours, theirs)
    else {
        return Ok(None);
    };
    let mode = if our_mode == their_mode {
        *our_mode
    } else if our_mode == base_mode {
        *their_mode
    } else if their_mode == base_mode {
        *our_mode
    } else {
        return Ok(None);
    };
    if [base, ours, theirs]
        .iter()
        .any(|bytes| bytes.contains(&0) || std::str::from_utf8(bytes).is_err())
    {
        return Ok(None);
    }
    let mut a = git2::MergeFileInput::new();
    let mut o = git2::MergeFileInput::new();
    let mut t = git2::MergeFileInput::new();
    a.content(base);
    o.content(ours);
    t.content(theirs);
    let merged = git2::merge_file(&a, &o, &t, None)?;
    if !merged.is_automergeable() {
        return Ok(None);
    }
    ensure!(
        merged.content().len() <= 64 * 1024 * 1024,
        "merged file exceeds reconciliation limit"
    );
    Ok(Some(Some(Entry::File {
        executable: mode,
        bytes: merged.content().to_vec(),
    })))
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
    fn staged_and_unstaged_changes_are_preserved_as_distinct_layers() {
        let base = f(b"one\ntwo\nthree\nfour\nfive\n");
        let staged = f(b"ONE\ntwo\nthree\nfour\nfive\n");
        let work = f(b"ONE\ntwo\nthree\nfour\nFIVE\n");
        let upstream = f(b"one\ntwo\nTHREE\nfour\nfive\n");
        let out = reconcile(
            Some(&base),
            Some(&staged),
            Some(&work),
            Some(&upstream),
            Collision::None,
        )
        .unwrap()
        .unwrap();
        assert_eq!(out.index, Some(f(b"ONE\ntwo\nTHREE\nfour\nfive\n")));
        assert_eq!(out.worktree, Some(f(b"ONE\ntwo\nTHREE\nfour\nFIVE\n")));
    }
    #[test]
    fn overlaps_and_unsafe_untracked_collisions_refuse() {
        let base = f(b"a\n");
        let staged = f(b"b\n");
        let theirs = f(b"c\n");
        assert_eq!(
            reconcile(
                Some(&base),
                Some(&staged),
                Some(&staged),
                Some(&theirs),
                Collision::None
            )
            .unwrap(),
            Err(Conflict::Index)
        );
        assert_eq!(
            reconcile(
                None,
                None,
                Some(&staged),
                Some(&theirs),
                Collision::Untracked
            )
            .unwrap(),
            Err(Conflict::UntrackedCollision)
        );
        assert_eq!(
            reconcile(None, None, Some(&theirs), Some(&theirs), Collision::Ignored).unwrap(),
            Err(Conflict::IgnoredCollision)
        );
        assert!(
            reconcile(
                None,
                None,
                Some(&theirs),
                Some(&theirs),
                Collision::Untracked
            )
            .unwrap()
            .unwrap()
            .absorbed_untracked
        );
    }
    #[test]
    fn binary_and_symlink_values_are_preserved_when_no_merge_is_needed() {
        let base = f(b"a\0b");
        let incoming = f(b"c\0d");
        assert_eq!(
            reconcile(
                Some(&base),
                Some(&base),
                Some(&base),
                Some(&incoming),
                Collision::None
            )
            .unwrap()
            .unwrap()
            .worktree,
            Some(incoming)
        );
        let link = Entry::Symlink(b"dir/target".to_vec());
        assert_eq!(
            reconcile(None, None, None, Some(&link), Collision::None)
                .unwrap()
                .unwrap()
                .worktree,
            Some(link)
        );
        let b = f(b"a\0x");
        let c = f(b"a\0y");
        assert_eq!(
            reconcile(Some(&base), Some(&b), Some(&b), Some(&c), Collision::None).unwrap(),
            Err(Conflict::Index)
        );
    }
    #[test]
    fn unchanged_upstream_keeps_both_local_layers_including_deletion() {
        let base = f(b"base\n");
        let staged = f(b"staged\n");
        let out = reconcile(
            Some(&base),
            Some(&staged),
            None,
            Some(&base),
            Collision::None,
        )
        .unwrap()
        .unwrap();
        assert_eq!(out.index, Some(staged));
        assert_eq!(out.worktree, None);
    }
    #[test]
    fn bounded_stress_preserves_unrelated_local_layers() {
        for index in 0..2048 {
            let base = f(format!("base {index}\n").as_bytes());
            let staged = f(format!("staged {index}\n").as_bytes());
            let work = f(format!("unstaged {index}\n").as_bytes());
            let result = reconcile(
                Some(&base),
                Some(&staged),
                Some(&work),
                Some(&base),
                Collision::None,
            )
            .unwrap()
            .unwrap();
            assert_eq!(result.index, Some(staged));
            assert_eq!(result.worktree, Some(work));
        }
    }
    #[test]
    fn distinct_binary_edits_and_type_collisions_are_not_merged() {
        let base = f(&[0xff, 1]);
        let left = f(&[0xfe, 1]);
        let right = f(&[0xfd, 1]);
        assert_eq!(
            reconcile(
                Some(&base),
                Some(&left),
                Some(&left),
                Some(&right),
                Collision::None
            )
            .unwrap(),
            Err(Conflict::Index)
        );
        let link = Entry::Symlink(b"x".to_vec());
        assert_eq!(
            reconcile(
                Some(&base),
                Some(&left),
                Some(&left),
                Some(&link),
                Collision::None
            )
            .unwrap(),
            Err(Conflict::Index)
        );
    }
}
