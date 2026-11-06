//! Held Git-blob reads and absence checks for an actual immutable snapshot.
use crate::{
    candidate_inputs::{self, BoundFile},
    candidate_snapshot::Snapshot,
    candidate_source::{Source, TreeEntry},
};
use anyhow::{ensure, Context, Result};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

pub(crate) struct SnapshotInputs<'a> {
    snapshot: &'a Snapshot,
    source_root: PathBuf,
    common_dir: PathBuf,
    entries: BTreeMap<String, TreeEntry>,
    held: Vec<BoundFile>,
    texts: BTreeMap<String, Vec<u8>>,
    absent: Vec<String>,
    bytes: usize,
}
impl<'a> SnapshotInputs<'a> {
    pub(crate) fn new(source: &Source, snapshot: &'a Snapshot, tree_oid: &str) -> Result<Self> {
        ensure!(
            snapshot.report().tree_oid == tree_oid,
            "source tree and input snapshot differ"
        );
        snapshot.directory()?;
        let entries = source
            .list_tree(tree_oid)?
            .into_iter()
            .map(|entry| (entry.path.clone(), entry))
            .collect();
        Ok(Self {
            snapshot,
            source_root: source.root().into(),
            common_dir: source.common_dir().into(),
            entries,
            held: Vec::new(),
            texts: BTreeMap::new(),
            absent: Vec::new(),
            bytes: 0,
        })
    }
    pub(crate) fn entries(&self) -> &BTreeMap<String, TreeEntry> {
        &self.entries
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
    pub(crate) fn check_context(&self, source: &Source, snapshot: &Snapshot) -> Result<()> {
        ensure!(
            self.source_root == source.root()
                && self.common_dir == source.common_dir()
                && self.snapshot.path() == snapshot.path()
                && self.snapshot.report().tree_oid == snapshot.report().tree_oid,
            "snapshot inputs belong to another context"
        );
        self.verify()
    }
    pub(crate) fn verify(&self) -> Result<()> {
        self.snapshot.directory()?;
        for held in &self.held {
            held.verify()?;
        }
        for path in &self.absent {
            self.check_absent(path)?;
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "snapshot_inputs_tests.rs"]
mod tests;
