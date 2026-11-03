//! Held Git authorities for worktree recovery. Callers must WAL lock creation
//! and release, and hold the cooperative worktree lease, before using these.
use crate::{checkout_sync_index::IndexLock, checkout_sync_journal::Identity};
use anyhow::{ensure, Result};
use git2::{Oid, Repository, Transaction};
use std::path::Path;

/// Keeps both symbolic HEAD and its branch locked while the index and paths
/// are inspected or changed. This does not lock arbitrary worktree writers.
pub(crate) struct RecoveryRefs<'repo> {
    repo: &'repo Repository,
    tx: Transaction<'repo>,
    branch: String,
    expected: Oid,
}
impl<'repo> RecoveryRefs<'repo> {
    /// Acquire with either the published or original branch value. The value
    /// is read only after both names are locked.
    pub(crate) fn acquire_for_restore(
        repo: &'repo Repository,
        branch: &str,
        old: Oid,
        new: Oid,
    ) -> Result<Self> {
        ensure!(
            branch.starts_with("refs/heads/") && git2::Reference::is_valid_name(branch),
            "invalid branch"
        );
        let mut tx = repo.transaction()?;
        let mut names = ["HEAD", branch];
        names.sort_unstable();
        for name in names {
            tx.lock_ref(name)?;
        }
        ensure!(
            repo.find_reference("HEAD")?.symbolic_target()? == Some(branch),
            "HEAD changed during recovery"
        );
        let current = repo
            .find_reference(branch)?
            .target()
            .ok_or_else(|| anyhow::anyhow!("branch is not direct"))?;
        ensure!(
            current == old || current == new,
            "branch changed during recovery"
        );
        Ok(Self {
            repo,
            tx,
            branch: branch.into(),
            expected: current,
        })
    }
    /// Caller journals release intent before entering: commit consumes locks.
    pub(crate) fn restore(mut self, old: Oid, signature: &git2::Signature<'_>) -> Result<()> {
        self.verify()?;
        if self.expected != old {
            self.tx.set_target(
                &self.branch,
                old,
                Some(signature),
                "forge checkout-sync rollback",
            )?;
            self.tx.commit()?;
        }
        Ok(())
    }
    pub(crate) fn verify(&self) -> Result<()> {
        ensure!(
            self.repo.find_reference("HEAD")?.symbolic_target()? == Some(self.branch.as_str()),
            "HEAD changed during recovery"
        );
        ensure!(
            self.repo.find_reference(&self.branch)?.target() == Some(self.expected),
            "branch changed during recovery"
        );
        Ok(())
    }
}

/// Holds index.lock across worktree restoration. The expected image must come
/// from durable transaction authority, not from this acquisition itself.
pub(crate) struct RecoveryIndex {
    lock: IndexLock,
}
impl RecoveryIndex {
    pub(crate) fn acquire(index: &Path, expected: Option<&Identity>) -> Result<Self> {
        let lock = IndexLock::acquire(index)?;
        let observed = lock.original().map(|i| i.journal_identity());
        // A successful index rollback can create a new inode containing the
        // original bytes. Authority here is exact content and mode; the held
        // lock then prevents cooperative Git writers from changing it.
        let matches = match (observed.as_ref(), expected) {
            (None, None) => true,
            (Some(a), Some(b)) => a.mode == b.mode && a.length == b.length && a.sha256 == b.sha256,
            _ => false,
        };
        ensure!(matches, "index does not match recorded original");
        lock.revalidate()?;
        Ok(Self { lock })
    }
    pub(crate) fn acquire_for_restore(
        index: &Path,
        original: Option<&Identity>,
        staged: &Identity,
    ) -> Result<Self> {
        let lock = IndexLock::acquire(index)?;
        let current = lock.original().map(|i| i.journal_identity());
        ensure!(
            current.as_ref() == original || current.as_ref() == Some(staged),
            "index changed before recovery"
        );
        lock.revalidate()?;
        Ok(Self { lock })
    }
    pub(crate) fn restore(
        self,
        original: Option<&Identity>,
        staged: &Identity,
        bytes: Option<&[u8]>,
    ) -> Result<()> {
        self.lock.restore_guard(original, staged, bytes)
    }
    pub(crate) fn verify(&self) -> Result<()> {
        self.lock.revalidate()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use git2::Signature;
    use std::fs;

    #[test]
    fn index_guard_holds_git_lock_and_detects_uncooperative_edit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("index");
        fs::write(&path, b"staged\0data").unwrap();
        let recorded = crate::checkout_sync_index::journal_identity(&path).unwrap();
        let guard = RecoveryIndex::acquire(&path, recorded.as_ref()).unwrap();
        assert!(IndexLock::acquire(&path).is_err());
        fs::write(&path, b"other staged data").unwrap();
        assert!(guard.verify().is_err());
        drop(guard);
        assert_eq!(fs::read(path).unwrap(), b"other staged data");
    }

    #[test]
    fn ref_guard_refuses_drift_and_holds_locks() {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let sig = Signature::now("guard", "guard@example.invalid").unwrap();
        let tree_id = repo.treebuilder(None).unwrap().write().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let old = repo
            .commit(Some("refs/heads/main"), &sig, &sig, "old", &tree, &[])
            .unwrap();
        repo.set_head("refs/heads/main").unwrap();
        let guard = RecoveryRefs::acquire_for_restore(&repo, "refs/heads/main", old, old).unwrap();
        guard.verify().unwrap();
        let mut rival = repo.transaction().unwrap();
        assert!(rival.lock_ref("refs/heads/main").is_err());
        drop(rival);
        drop(guard);
        assert!(RecoveryRefs::acquire_for_restore(
            &repo,
            "refs/heads/main",
            Oid::ZERO_SHA1,
            Oid::ZERO_SHA1
        )
        .is_err());
    }
}
