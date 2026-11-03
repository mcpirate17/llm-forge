//! Bounded ignored-path inventory under the cooperative checkout lease.
//! Directory descriptors and no-follow metadata prevent traversal through a
//! symlink. The lease, not a pathname check, is the cooperative writer boundary.
use crate::{candidate_source::Source, checkout_sync_worktree::SyncLease};
use anyhow::{bail, ensure, Context, Result};
use cap_std::fs::{Dir, MetadataExt};
use std::{collections::BTreeSet, path::Path};

const LIMIT: usize = 100_000;
const DEPTH: usize = 256;

pub(crate) fn inventory(
    source: &Source,
    lease: &SyncLease,
    tracked: &BTreeSet<String>,
) -> Result<Inventory> {
    lease.verify()?;
    let root = Dir::open_ambient_dir(source.root(), cap_std::ambient_authority())?;
    let before = root.dir_metadata()?;
    let mut state = Scan {
        source,
        tracked,
        visited: 0,
        ignored: BTreeSet::new(),
        directories: BTreeSet::new(),
        empty: BTreeSet::new(),
        leaves: BTreeSet::new(),
    };
    state.walk(&root, "", 0)?;
    let after = root.dir_metadata()?;
    let named = std::fs::symlink_metadata(source.root())?;
    use std::os::unix::fs::MetadataExt as _;
    ensure!(
        (before.dev(), before.ino()) == (after.dev(), after.ino())
            && (after.dev(), after.ino()) == (named.dev(), named.ino()),
        "checkout root changed during inventory"
    );
    lease.verify()?;
    Ok(Inventory {
        ignored: state.ignored,
        directories: state.directories,
        empty: state.empty,
        leaves: state.leaves,
    })
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Inventory {
    pub ignored: BTreeSet<String>,
    pub directories: BTreeSet<String>,
    pub empty: BTreeSet<String>,
    /// All regular-file and symlink names, including ignored leaves.
    pub leaves: BTreeSet<String>,
}
struct Scan<'a> {
    source: &'a Source,
    tracked: &'a BTreeSet<String>,
    visited: usize,
    ignored: BTreeSet<String>,
    directories: BTreeSet<String>,
    empty: BTreeSet<String>,
    leaves: BTreeSet<String>,
}
impl Scan<'_> {
    fn walk(&mut self, dir: &Dir, prefix: &str, depth: usize) -> Result<bool> {
        ensure!(depth <= DEPTH, "checkout directory depth exceeds limit");
        let before = dir.dir_metadata()?;
        let mut nonempty = false;
        for entry in dir.read_dir(".")? {
            let entry = entry?;
            self.visited = self
                .visited
                .checked_add(1)
                .context("checkout inventory overflow")?;
            ensure!(self.visited <= LIMIT, "checkout inventory exceeds limit");
            let name = entry.file_name();
            let name = name.to_str().context("non-UTF-8 checkout path")?;
            if prefix.is_empty() && name == ".git" {
                continue;
            }
            ensure!(
                !name.eq_ignore_ascii_case(".git"),
                "nested Git metadata is unsupported"
            );
            let path = if prefix.is_empty() {
                name.to_owned()
            } else {
                format!("{prefix}/{name}")
            };
            crate::candidate_source::safe_path(path.as_bytes())?;
            let metadata = dir.symlink_metadata(name)?;
            nonempty = true;
            if metadata.is_dir() {
                self.directories.insert(path.clone());
                let child = dir.open_dir(name)?;
                let opened = child.dir_metadata()?;
                ensure!(
                    (metadata.dev(), metadata.ino()) == (opened.dev(), opened.ino()),
                    "checkout directory replaced during inventory"
                );
                let contains_children = self.walk(&child, &path, depth + 1)?;
                if !contains_children {
                    self.empty.insert(path.clone());
                }
                if !contains_children
                    && self
                        .source
                        .repository()
                        .status_should_ignore(Path::new(&path))?
                {
                    self.ignored.insert(path);
                }
            } else if metadata.is_file() || metadata.file_type().is_symlink() {
                self.leaves.insert(path.clone());
                if !self.tracked.contains(&path)
                    && self
                        .source
                        .repository()
                        .status_should_ignore(Path::new(&path))?
                {
                    self.ignored.insert(path);
                }
            } else {
                bail!("unsupported checkout path type: {path}");
            }
        }
        let after = dir.dir_metadata()?;
        ensure!(
            (before.dev(), before.ino()) == (after.dev(), after.ino()),
            "checkout directory identity changed during inventory"
        );
        Ok(nonempty)
    }
}
#[cfg(test)]
#[path = "checkout_sync_inventory_tests.rs"]
mod tests;
