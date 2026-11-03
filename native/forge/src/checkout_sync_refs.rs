//! Git ref locks for one fast-forward publication. This is only one component of
//! checkout sync: callers must own the index lock, worktree lease and journal.
use anyhow::{ensure, Context, Result};
use git2::{Oid, Repository, Signature, Transaction};

pub(crate) struct RefLocks<'repo> {
    repo: &'repo Repository,
    tx: Transaction<'repo>,
    branch: String,
    target: String,
    old: Oid,
    new: Oid,
}

#[derive(Debug)]
pub(crate) enum RefPublication {
    Committed,
    /// Git may have applied the update before it returned an error. The observed
    /// value is diagnostic, not proof of which writer changed it after unlock.
    CommitError {
        error: String,
        observed: Option<Oid>,
    },
}

impl<'repo> RefLocks<'repo> {
    /// Both refs must be fully qualified direct references. Lock HEAD too so a
    /// cooperating checkout cannot switch the symbolic branch during publication.
    pub(crate) fn acquire(
        repo: &'repo Repository,
        branch: &str,
        target: &str,
        old: Oid,
        new: Oid,
    ) -> Result<Self> {
        ensure!(
            branch.starts_with("refs/heads/") && git2::Reference::is_valid_name(branch),
            "invalid checkout branch"
        );
        ensure!(
            target.starts_with("refs/") && git2::Reference::is_valid_name(target),
            "invalid target ref"
        );
        ensure!(branch != target && old != new, "invalid fast-forward refs");
        ensure!(
            repo.graph_descendant_of(new, old)?,
            "target is not a fast-forward"
        );
        let mut names = vec!["HEAD", branch, target];
        names.sort_unstable();
        names.dedup();
        let mut tx = repo.transaction()?;
        for name in names {
            tx.lock_ref(name)
                .with_context(|| format!("lock Git ref {name}"))?;
        }
        let held = Self {
            repo,
            tx,
            branch: branch.into(),
            target: target.into(),
            old,
            new,
        };
        held.verify()?;
        Ok(held)
    }
    pub(crate) fn verify(&self) -> Result<()> {
        let head = self.repo.find_reference("HEAD")?;
        ensure!(
            head.symbolic_target()? == Some(self.branch.as_str()),
            "symbolic HEAD changed"
        );
        ensure!(
            self.repo.find_reference(&self.branch)?.target() == Some(self.old),
            "checkout branch changed"
        );
        ensure!(
            self.repo.find_reference(&self.target)?.target() == Some(self.new),
            "target ref changed"
        );
        Ok(())
    }
    /// Consumes the held Git transaction and writes only the branch ref. libgit2
    /// documents non-atomic multi-ref commit, so no other ref gets a set_target.
    /// Caller must journal before entry and retain recovery on CommitError.
    pub(crate) fn publish(mut self, signature: &Signature<'_>) -> Result<RefPublication> {
        self.verify()?;
        self.tx.set_target(
            &self.branch,
            self.new,
            Some(signature),
            "forge checkout-sync fast-forward",
        )?;
        let branch = self.branch.clone();
        let repo = self.repo;
        match self.tx.commit() {
            Ok(()) => Ok(RefPublication::Committed),
            Err(error) => Ok(RefPublication::CommitError {
                error: error.to_string(),
                observed: repo.find_reference(&branch).ok().and_then(|r| r.target()),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (tempfile::TempDir, Repository, Oid, Oid) {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let tree_id = repo.treebuilder(None).unwrap().write().unwrap();
        let signature = Signature::now("sync-test", "sync-test@example.invalid").unwrap();
        let (old, new) = {
            let tree = repo.find_tree(tree_id).unwrap();
            let old = repo
                .commit(
                    Some("refs/heads/main"),
                    &signature,
                    &signature,
                    "old",
                    &tree,
                    &[],
                )
                .unwrap();
            let parent = repo.find_commit(old).unwrap();
            let new = repo
                .commit(None, &signature, &signature, "new", &tree, &[&parent])
                .unwrap();
            (old, new)
        };
        repo.set_head("refs/heads/main").unwrap();
        repo.reference("refs/remotes/origin/main", new, false, "test target")
            .unwrap();
        (dir, repo, old, new)
    }
    #[test]
    fn fast_forward_changes_only_the_locked_branch() {
        let (_dir, repo, old, new) = fixture();
        let held = RefLocks::acquire(
            &repo,
            "refs/heads/main",
            "refs/remotes/origin/main",
            old,
            new,
        )
        .unwrap();
        held.verify().unwrap();
        let signature = Signature::now("sync-test", "sync-test@example.invalid").unwrap();
        assert!(matches!(
            held.publish(&signature).unwrap(),
            RefPublication::Committed
        ));
        assert_eq!(
            repo.find_reference("refs/heads/main").unwrap().target(),
            Some(new)
        );
        assert_eq!(
            repo.find_reference("refs/remotes/origin/main")
                .unwrap()
                .target(),
            Some(new)
        );
        assert_eq!(
            repo.find_reference("HEAD")
                .unwrap()
                .symbolic_target()
                .unwrap(),
            Some("refs/heads/main")
        );
    }
    #[test]
    fn recovery_guard_restores_published_ref_and_accepts_original() {
        let (_dir, repo, old, new) = fixture();
        let signature = Signature::now("sync-test", "sync-test@example.invalid").unwrap();
        let held = RefLocks::acquire(
            &repo,
            "refs/heads/main",
            "refs/remotes/origin/main",
            old,
            new,
        )
        .unwrap();
        assert!(matches!(
            held.publish(&signature).unwrap(),
            RefPublication::Committed
        ));
        let guard = crate::checkout_sync_guards::RecoveryRefs::acquire_for_restore(
            &repo,
            "refs/heads/main",
            old,
            new,
        )
        .unwrap();
        guard.restore(old, &signature).unwrap();
        assert_eq!(
            repo.find_reference("refs/heads/main").unwrap().target(),
            Some(old)
        );
        let guard = crate::checkout_sync_guards::RecoveryRefs::acquire_for_restore(
            &repo,
            "refs/heads/main",
            old,
            new,
        )
        .unwrap();
        guard.restore(old, &signature).unwrap();
        assert_eq!(
            repo.find_reference("refs/heads/main").unwrap().target(),
            Some(old)
        );
    }
    #[test]
    fn wrong_old_target_and_unrelated_history_refuse_before_publication() {
        let (_dir, repo, old, new) = fixture();
        assert!(RefLocks::acquire(
            &repo,
            "refs/heads/main",
            "refs/remotes/origin/main",
            new,
            old
        )
        .is_err());
        repo.reference("refs/remotes/origin/main", old, true, "move target")
            .unwrap();
        assert!(RefLocks::acquire(
            &repo,
            "refs/heads/main",
            "refs/remotes/origin/main",
            old,
            new
        )
        .is_err());
        assert_eq!(
            repo.find_reference("refs/heads/main").unwrap().target(),
            Some(old)
        );
    }
    #[test]
    fn rollback_never_moves_a_branch_that_advanced_again() {
        let (_dir, repo, old, new) = fixture();
        let sig = Signature::now("sync-test", "sync-test@example.invalid").unwrap();
        let held = RefLocks::acquire(
            &repo,
            "refs/heads/main",
            "refs/remotes/origin/main",
            old,
            new,
        )
        .unwrap();
        assert!(matches!(
            held.publish(&sig).unwrap(),
            RefPublication::Committed
        ));
        let third = {
            let parent = repo.find_commit(new).unwrap();
            let tree = parent.tree().unwrap();
            repo.commit(
                Some("refs/heads/main"),
                &sig,
                &sig,
                "later",
                &tree,
                &[&parent],
            )
            .unwrap()
        };
        assert!(
            crate::checkout_sync_guards::RecoveryRefs::acquire_for_restore(
                &repo,
                "refs/heads/main",
                old,
                new,
            )
            .is_err()
        );
        assert_eq!(
            repo.find_reference("refs/heads/main").unwrap().target(),
            Some(third)
        );
    }
}
