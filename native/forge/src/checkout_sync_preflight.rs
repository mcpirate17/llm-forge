//! Read-only authority binding before any forward transaction mutation.
use crate::{
    candidate_commit_snapshot, candidate_source::Source, checkout_sync_index,
    checkout_sync_journal::Identity,
};
use anyhow::{ensure, Context, Result};
use git2::{Oid, RepositoryState};
use std::{
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

pub(crate) struct Preflight {
    pub source: Source,
    pub branch: String,
    pub target: String,
    pub old: Oid,
    pub new: Oid,
    pub behind: usize,
    pub root_identity: (u64, u64),
    pub common_identity: (u64, u64),
    pub index_path: PathBuf,
    pub index_parent: (u64, u64),
    pub original_index: Option<Identity>,
}
impl Preflight {
    pub(crate) fn open(root: &Path, target: &str) -> Result<Self> {
        reject_git_overrides()?;
        let source = Source::open(root)?;
        let repo = source.repository();
        ensure!(
            repo.state() == RepositoryState::Clean,
            "active Git operation prevents checkout sync"
        );
        let head = repo.find_reference("HEAD")?;
        let branch = head
            .symbolic_target()?
            .context("detached HEAD prevents checkout sync")?
            .to_owned();
        ensure!(
            branch.starts_with("refs/heads/") && git2::Reference::is_valid_name(&branch),
            "invalid checkout branch"
        );
        ensure!(
            target.starts_with("refs/") && git2::Reference::is_valid_name(target),
            "invalid integration ref"
        );
        let old = repo
            .find_reference(&branch)?
            .target()
            .context("checkout branch must be a direct born ref")?;
        let new = repo
            .find_reference(target)?
            .target()
            .context("integration ref must be direct")?;
        let (ahead, behind) = repo.graph_ahead_behind(old, new)?;
        ensure!(ahead == 0, "checkout has commits not on integration line");
        candidate_commit_snapshot::validate_checkout_index(&source)?;
        let root_meta = std::fs::symlink_metadata(source.root())?;
        let common_meta = std::fs::symlink_metadata(source.common_dir())?;
        ensure!(
            root_meta.is_dir() && common_meta.is_dir(),
            "checkout root or Git common dir changed"
        );
        let parent = repo.path().canonicalize()?;
        let metadata = std::fs::metadata(&parent)?;
        let index_path = parent.join("index");
        let original_index = checkout_sync_index::journal_identity(&index_path)?;
        drop(head);
        Ok(Self {
            source,
            branch,
            target: target.to_owned(),
            old,
            new,
            behind,
            root_identity: (root_meta.dev(), root_meta.ino()),
            common_identity: (common_meta.dev(), common_meta.ino()),
            index_path,
            index_parent: (metadata.dev(), metadata.ino()),
            original_index,
        })
    }
    /// Recheck all mutable authority after acquiring the lease and Git locks.
    pub(crate) fn verify(&self) -> Result<()> {
        reject_git_overrides()?;
        let root = std::fs::symlink_metadata(self.source.root())?;
        let common = std::fs::symlink_metadata(self.source.common_dir())?;
        ensure!(
            root.is_dir()
                && (root.dev(), root.ino()) == self.root_identity
                && common.is_dir()
                && (common.dev(), common.ino()) == self.common_identity,
            "checkout root or Git common dir changed"
        );
        let repo = self.source.repository();
        ensure!(
            repo.state() == RepositoryState::Clean,
            "Git operation began during checkout admission"
        );
        ensure!(
            repo.find_reference("HEAD")?.symbolic_target()? == Some(self.branch.as_str()),
            "HEAD changed during checkout admission"
        );
        ensure!(
            repo.find_reference(&self.branch)?.target() == Some(self.old),
            "branch changed during checkout admission"
        );
        ensure!(
            repo.find_reference(&self.target)?.target() == Some(self.new),
            "integration ref changed during checkout admission"
        );
        let metadata =
            std::fs::metadata(self.index_path.parent().context("index parent missing")?)?;
        ensure!(
            (metadata.dev(), metadata.ino()) == self.index_parent,
            "index parent changed"
        );
        ensure!(
            checkout_sync_index::journal_identity(&self.index_path)? == self.original_index,
            "index changed during checkout admission"
        );
        Ok(())
    }
}
fn reject_git_overrides() -> Result<()> {
    for name in [
        "GIT_DIR",
        "GIT_COMMON_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_NAMESPACE",
        "GIT_CONFIG",
        "GIT_CONFIG_COUNT",
        "GIT_CONFIG_PARAMETERS",
        "GIT_CONFIG_GLOBAL",
        "GIT_CONFIG_SYSTEM",
        "GIT_ATTR_NOSYSTEM",
        "GIT_CEILING_DIRECTORIES",
        "GIT_DISCOVERY_ACROSS_FILESYSTEM",
    ] {
        ensure!(
            std::env::var_os(name).is_none(),
            "Git routing/config override {name} is unsupported"
        );
    }
    Ok(())
}
#[cfg(test)]
#[path = "checkout_sync_preflight_tests.rs"]
mod tests;
