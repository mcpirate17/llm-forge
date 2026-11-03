//! Exact, bounded Git object identities for candidate review, without a Git child.
mod changes;
pub mod lines;
mod resolve;
pub(crate) use resolve::parse_index;
mod tool_context;
pub use tool_context::ToolBinding;
#[cfg(test)]
mod tests;

use anyhow::{ensure, Context, Result};
use git2::{ObjectType, Oid, Repository};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";
const MAX_ENTRIES: usize = 100_000;
const MAX_PATH: usize = 4096;
const MAX_OUTPUT: usize = 64 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Index,
    Commit,
    Range,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Change {
    pub status: String,
    pub path: String,
    pub old_path: Option<String>,
    pub old_mode: String,
    pub new_mode: String,
    pub old_oid: String,
    pub new_oid: String,
    pub classes: Vec<String>,
    pub risk: String,
}

impl Change {
    pub fn deleted(&self) -> bool {
        self.new_mode == "000000"
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Candidate {
    pub kind: Kind,
    pub tree_oid: String,
    pub base_tree_oid: String,
    pub base_commit_oid: Option<String>,
    pub commit_oid: Option<String>,
    pub target_ref: Option<String>,
    pub changes: Vec<Change>,
    pub integration_base_oid: Option<String>,
    pub integration_base_detail: String,
}

impl Candidate {
    pub fn waiver_base(&self) -> Option<&str> {
        self.integration_base_oid.as_deref().or({
            if self.integration_base_detail.is_empty() {
                self.base_commit_oid.as_deref()
            } else {
                None
            }
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TreeEntry {
    pub path: String,
    pub mode: String,
    pub object_type: String,
    pub oid: String,
    pub size: Option<u64>,
}

pub struct Source {
    repository: Repository,
    root: PathBuf,
    common_dir: PathBuf,
}

pub(crate) fn safe_path(raw: &[u8]) -> Result<&str> {
    let path = std::str::from_utf8(raw).context("non-UTF-8 Git paths are unsupported")?;
    ensure!(
        !path.is_empty()
            && path.len() <= MAX_PATH
            && !path.contains('\0')
            && !path.starts_with('/'),
        "invalid Git path or path exceeds 4096 bytes"
    );
    ensure!(
        path.split('/')
            .all(|part| !part.is_empty() && part != "." && part != ".."),
        "unsafe Git tree path"
    );
    Ok(path)
}

pub(super) fn budget(total: &mut usize, bytes: usize) -> Result<()> {
    *total = total
        .checked_add(bytes)
        .context("candidate metadata size overflow")?;
    ensure!(*total <= MAX_OUTPUT, "candidate metadata exceeds 64 MiB");
    Ok(())
}

impl Source {
    pub fn open(path: &Path) -> Result<Self> {
        ensure!(
            path.is_dir(),
            "candidate repository path must be a directory"
        );
        let repository = Repository::discover(path).context("discover candidate repository")?;
        let root = repository
            .workdir()
            .context("candidate review requires a working repository")?
            .canonicalize()?;
        let common_dir = repository.commondir().canonicalize()?;
        Ok(Self {
            repository,
            root,
            common_dir,
        })
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn common_dir(&self) -> &Path {
        &self.common_dir
    }
    pub(crate) fn repository(&self) -> &Repository {
        &self.repository
    }

    pub fn commit_tree(&self, commit_oid: &str) -> Result<String> {
        Ok(self
            .repository
            .find_commit(Oid::from_str(commit_oid)?)?
            .tree_id()
            .to_string())
    }

    pub fn list_tree(&self, tree_oid: &str) -> Result<Vec<TreeEntry>> {
        if tree_oid == EMPTY_TREE {
            return Ok(Vec::new());
        }
        let odb = self.repository.odb()?;
        let mut pending = vec![(String::new(), Oid::from_str(tree_oid)?)];
        let mut entries = Vec::new();
        let mut examined = 0usize;
        let mut bytes = 0usize;
        while let Some((prefix, oid)) = pending.pop() {
            let (size, kind) = odb.read_header(oid)?;
            ensure!(kind == ObjectType::Tree, "candidate object is not a tree");
            budget(&mut bytes, size)?;
            let tree = self.repository.find_tree(oid)?;
            examined = examined
                .checked_add(tree.len())
                .context("candidate tree entry count overflow")?;
            ensure!(
                examined <= MAX_ENTRIES,
                "candidate tree exceeds 100000 entries"
            );
            for entry in tree.iter() {
                let name = safe_path(entry.name_bytes())?;
                ensure!(!name.contains('/'), "Git tree entry name contains a slash");
                let path = if prefix.is_empty() {
                    name.to_owned()
                } else {
                    format!("{prefix}/{name}")
                };
                safe_path(path.as_bytes())?;
                budget(&mut bytes, path.len() + 160)?;
                match entry.kind() {
                    Some(ObjectType::Tree) => pending.push((path, entry.id())),
                    Some(ObjectType::Blob) => {
                        let (size, kind) = odb.read_header(entry.id())?;
                        ensure!(
                            kind == ObjectType::Blob,
                            "candidate blob has wrong object type"
                        );
                        entries.push(TreeEntry {
                            path,
                            mode: format!("{:06o}", entry.filemode_raw()),
                            object_type: "blob".into(),
                            oid: entry.id().to_string(),
                            size: Some(size as u64),
                        });
                    }
                    Some(ObjectType::Commit) => entries.push(TreeEntry {
                        path,
                        mode: format!("{:06o}", entry.filemode_raw()),
                        object_type: "commit".into(),
                        oid: entry.id().to_string(),
                        size: None,
                    }),
                    _ => anyhow::bail!("unsupported Git tree entry type"),
                }
            }
        }
        entries.sort_by(|left, right| left.path.cmp(&right.path));
        Ok(entries)
    }
}
