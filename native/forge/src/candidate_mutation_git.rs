//! Immutable Git object inputs for base and fixed review anchors.
use crate::candidate_source::{Source, TreeEntry};
use anyhow::{ensure, Context, Result};
use git2::{ObjectType, Oid};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
pub(super) struct GitInputs<'a> {
    source: &'a Source,
    trees: BTreeMap<String, BTreeMap<String, TreeEntry>>,
    blobs: BTreeMap<(String, String), Vec<u8>>,
    anchors: BTreeMap<String, String>,
    bytes: usize,
    pub evidence: Vec<Value>,
}
impl<'a> GitInputs<'a> {
    pub fn new(source: &'a Source) -> Self {
        Self {
            source,
            trees: BTreeMap::new(),
            blobs: BTreeMap::new(),
            anchors: BTreeMap::new(),
            bytes: 0,
            evidence: Vec::new(),
        }
    }
    pub fn anchor(&mut self, commit: &str, tree: &str) -> Result<()> {
        ensure!(
            self.source.commit_tree(commit)? == tree,
            "review anchor commit/tree binding differs"
        );
        self.anchors.insert(commit.into(), tree.into());
        Ok(())
    }
    pub fn entries(&mut self, tree: &str) -> Result<&BTreeMap<String, TreeEntry>> {
        if !self.trees.contains_key(tree) {
            ensure!(self.trees.len() < 4, "mutation Git capture exceeds4 trees");
            self.trees.insert(
                tree.into(),
                self.source
                    .list_tree(tree)?
                    .into_iter()
                    .map(|entry| (entry.path.clone(), entry))
                    .collect(),
            );
        }
        self.trees
            .get(tree)
            .context("mutation Git tree capture disappeared")
    }
    pub fn read(&mut self, tree: &str, path: &str) -> Result<&[u8]> {
        crate::candidate_source::safe_path(path.as_bytes())?;
        let key = (tree.to_owned(), path.to_owned());
        if !self.blobs.contains_key(&key) {
            ensure!(
                self.blobs.len() < 4096,
                "mutation Git capture exceeds4096 blobs"
            );
            let entry = self
                .entries(tree)?
                .get(path)
                .context("mutation anchor/base file absent")?
                .clone();
            ensure!(
                entry.object_type == "blob"
                    && matches!(entry.mode.as_str(), "100644" | "100755")
                    && entry.size.is_some_and(|size| size <= 1024 * 1024),
                "mutation Git input must be a regular blob at most1MiB"
            );
            let id = Oid::from_str(&entry.oid)?;
            let odb = self.source.repository().odb()?;
            let (size, kind) = odb.read_header(id)?;
            ensure!(
                kind == ObjectType::Blob && size <= 1024 * 1024 && Some(size as u64) == entry.size,
                "mutation Git blob header mismatch"
            );
            self.bytes = self
                .bytes
                .checked_add(size)
                .context("mutation Git byte overflow")?;
            ensure!(
                self.bytes <= 32 * 1024 * 1024,
                "mutation Git capture exceeds32MiB"
            );
            let blob = self.source.repository().find_blob(id)?;
            ensure!(
                blob.size() == size && Oid::hash_object(ObjectType::Blob, blob.content())? == id,
                "mutation Git blob identity mismatch"
            );
            self.evidence
                .push(json!({"tree":tree,"path":path,"oid":entry.oid,
                "sha256":format!("{:x}",Sha256::digest(blob.content())),"bytes":size}));
            self.blobs.insert(key.clone(), blob.content().to_vec());
        }
        self.blobs
            .get(&key)
            .map(Vec::as_slice)
            .context("mutation Git blob capture disappeared")
    }
    pub fn verify(&self) -> Result<()> {
        for (commit, tree) in &self.anchors {
            ensure!(
                self.source.commit_tree(commit)? == *tree,
                "review anchor changed or disappeared"
            );
        }
        Ok(())
    }
}
