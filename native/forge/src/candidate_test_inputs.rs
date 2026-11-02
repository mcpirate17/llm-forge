//! Captured Git-bound inputs for review test selection.
use crate::{
    candidate_inputs::{self, BoundFile},
    candidate_policy::{self, Policy},
    candidate_snapshot::Snapshot,
    candidate_source::{Candidate, Source, TreeEntry},
};
use anyhow::{ensure, Context, Result};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub(crate) struct Inputs<'a> {
    snapshot: &'a Snapshot,
    policy: &'a Policy,
    today: &'a str,
    source_root: PathBuf,
    common_dir: PathBuf,
    policy_resolved: String,
    policy_links: Vec<(String, String)>,
    pub entries: BTreeMap<String, TreeEntry>,
    held: Vec<BoundFile>,
    texts: BTreeMap<String, Vec<u8>>,
    absent: Vec<String>,
    bytes: usize,
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
        snapshot.directory()?;
        let tree_entries = source.list_tree(&candidate.tree_oid)?;
        let (policy_resolved, policy_links) =
            crate::candidate_snapshot::resolve_captured_file(source, &tree_entries, &policy.path)?;
        let entries = tree_entries
            .into_iter()
            .map(|entry| (entry.path.clone(), entry))
            .collect();
        let mut inputs = Self {
            snapshot,
            policy,
            today,
            source_root: source.root().to_owned(),
            common_dir: source.common_dir().to_owned(),
            policy_resolved,
            policy_links,
            entries,
            held: Vec::new(),
            texts: BTreeMap::new(),
            absent: Vec::new(),
            bytes: 0,
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
    pub fn absent(&mut self, path: &str) -> Result<()> {
        ensure!(
            !self.entries.contains_key(path),
            "cannot assert absence of tracked selection input"
        );
        self.check_absent(path)?;
        self.absent.push(path.into());
        Ok(())
    }
    pub fn external_registry(&mut self, path: &Path, sha256: &str) -> Result<String> {
        let resolved = path
            .canonicalize()
            .context("resolve external contract registry")?;
        ensure!(
            !resolved.starts_with(&self.source_root) && !resolved.starts_with(&self.common_dir),
            "external contract registry must be outside source and Git metadata"
        );
        let (held, bytes) =
            candidate_inputs::external(path, sha256, 1024 * 1024, &self.snapshot.path())?;
        self.bytes = self
            .bytes
            .checked_add(bytes.len())
            .context("selection byte overflow")?;
        ensure!(
            self.bytes <= 32 * 1024 * 1024,
            "test-selection inputs exceed 32 MiB"
        );
        let text = String::from_utf8(bytes).context("external contract registry must be UTF-8")?;
        ensure!(
            !text.contains('\0'),
            "external contract registry contains NUL"
        );
        self.held.push(held);
        Ok(text)
    }
    fn check_absent(&self, path: &str) -> Result<()> {
        ensure!(
            conductor_native::candidate_cargo_lint::read_optional(
                &self.snapshot.directory()?,
                path,
                1024 * 1024
            )
            .map_err(anyhow::Error::msg)?
            .is_none(),
            "absent selection input appeared: {path}"
        );
        Ok(())
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
        let bytes = self.read_bytes_with_limit(path, limit)?;
        let text = std::str::from_utf8(bytes).context("test-selection input must be UTF-8")?;
        ensure!(!text.contains('\0'), "test-selection input contains NUL");
        Ok(text)
    }
    pub(crate) fn read_bytes_with_limit(&mut self, path: &str, limit: usize) -> Result<&[u8]> {
        ensure!(
            (1..=2 * 1024 * 1024).contains(&limit),
            "invalid captured read limit"
        );
        if !self.texts.contains_key(path) {
            ensure!(
                self.texts.len() < 4096,
                "test-selection captured file count exceeds 4096"
            );
            let entry = self
                .entries
                .get(path)
                .context("selection input absent from captured Git tree")?;
            ensure!(
                matches!(entry.mode.as_str(), "100644" | "100755")
                    && entry.object_type == "blob"
                    && entry.size.is_some_and(|size| size <= limit as u64),
                "selection input must be a regular Git blob within {limit} bytes: {path}"
            );
            let (held, bytes) =
                candidate_inputs::snapshot(&self.snapshot.path(), path, &entry.oid, limit)?;
            self.bytes = self
                .bytes
                .checked_add(bytes.len())
                .context("test-selection bytes overflow")?;
            ensure!(
                self.bytes <= 32 * 1024 * 1024,
                "test-selection inputs exceed 32 MiB"
            );
            self.held.push(held);
            self.texts.insert(path.into(), bytes);
        }
        ensure!(
            self.texts.get(path).is_some_and(|text| text.len() <= limit),
            "cached captured input exceeds requested read limit"
        );
        self.texts
            .get(path)
            .map(Vec::as_slice)
            .context("captured selection input missing")
    }
    pub(crate) fn check_context(
        &self,
        source: &Source,
        snapshot: &Snapshot,
        policy: &Policy,
    ) -> Result<()> {
        ensure!(
            self.source_root == source.root()
                && self.common_dir == source.common_dir()
                && self.snapshot.path() == snapshot.path()
                && self.snapshot.report().tree_oid == snapshot.report().tree_oid
                && self.policy.path == policy.path
                && self.policy.digest == policy.digest
                && self.policy.document == policy.document,
            "test selection belongs to another review context"
        );
        self.verify()
    }
    pub fn verify(&self) -> Result<()> {
        self.snapshot.directory()?;
        for held in &self.held {
            held.verify()?;
        }
        for path in &self.absent {
            self.check_absent(path)?;
        }
        self.check_policy()
    }
}
