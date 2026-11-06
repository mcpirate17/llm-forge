//! Candidate policy/date/link authority around shared held snapshot reads.
use crate::{
    candidate_policy::{self, Policy},
    candidate_snapshot::Snapshot,
    candidate_source::{Candidate, Source, TreeEntry},
    snapshot_inputs::SnapshotInputs,
};
use anyhow::{ensure, Result};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, path::Path};

pub(crate) struct Inputs<'a> {
    snapshot: &'a Snapshot,
    policy: &'a Policy,
    today: &'a str,
    policy_resolved: String,
    policy_links: Vec<(String, String)>,
    tree: SnapshotInputs<'a>,
}
impl<'a> Inputs<'a> {
    pub fn new(
        source: &Source,
        snapshot: &'a Snapshot,
        candidate: &Candidate,
        policy: &'a Policy,
        today: &'a str,
    ) -> Result<Self> {
        ensure!(
            snapshot.report().tree_oid == candidate.tree_oid,
            "candidate and selection snapshot differ"
        );
        let tree = SnapshotInputs::new(source, snapshot, &candidate.tree_oid)?;
        let (policy_resolved, policy_links) =
            crate::candidate_snapshot::resolve_captured_file(source, tree.entries(), &policy.path)?;
        let mut inputs = Self {
            snapshot,
            policy,
            today,
            policy_resolved,
            policy_links,
            tree,
        };
        inputs.check_policy()?;
        let resolved = inputs.policy_resolved.clone();
        let captured = inputs.read(&resolved)?;
        ensure!(
            format!("{:x}", Sha256::digest(captured.as_bytes())) == policy.digest,
            "selection policy differs from the captured Git blob"
        );
        Ok(inputs)
    }
    pub(crate) fn entries(&self) -> &BTreeMap<String, TreeEntry> {
        self.tree.entries()
    }
    pub(crate) fn tree_mut(&mut self) -> &mut SnapshotInputs<'a> {
        &mut self.tree
    }
    pub fn absent(&mut self, path: &str) -> Result<()> {
        self.tree.absent(path)
    }
    pub fn external_registry(&mut self, path: &Path, sha256: &str) -> Result<String> {
        self.tree.external_registry(path, sha256)
    }
    fn check_policy(&self) -> Result<()> {
        let directory = self.snapshot.directory()?;
        for (path, target) in &self.policy_links {
            ensure!(
                directory.symlink_metadata(path)?.is_symlink(),
                "captured policy link changed type: {path}"
            );
            ensure!(
                directory
                    .read_link_contents(path)?
                    .as_os_str()
                    .as_encoded_bytes()
                    == target.as_bytes(),
                "captured policy link changed: {path}"
            );
        }
        ensure!(
            directory.canonicalize(&self.policy.path)? == Path::new(&self.policy_resolved),
            "captured policy resolution changed"
        );
        let actual =
            candidate_policy::load(&self.snapshot.path(), Some(&self.policy.path), self.today)?;
        ensure!(
            actual.digest == self.policy.digest && actual.document == self.policy.document,
            "selection policy changed or differs from captured policy"
        );
        Ok(())
    }
    pub fn read(&mut self, path: &str) -> Result<&str> {
        self.read_with_limit(path, 1024 * 1024)
    }
    pub(crate) fn read_with_limit(&mut self, path: &str, limit: usize) -> Result<&str> {
        self.tree.read_with_limit(path, limit)
    }
    pub(crate) fn read_bytes_with_limit(&mut self, path: &str, limit: usize) -> Result<&[u8]> {
        self.tree.read_bytes_with_limit(path, limit)
    }
    pub(crate) fn check_context(
        &self,
        source: &Source,
        snapshot: &Snapshot,
        policy: &Policy,
    ) -> Result<()> {
        self.tree.check_context(source, snapshot)?;
        ensure!(
            self.policy.path == policy.path
                && self.policy.digest == policy.digest
                && self.policy.document == policy.document,
            "test selection belongs to another review context"
        );
        self.check_policy()
    }
    pub fn verify(&self) -> Result<()> {
        self.tree.verify()?;
        self.check_policy()
    }
}
