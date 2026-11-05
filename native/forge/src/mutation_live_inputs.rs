//! Bounded, read-only live corpus inputs. No Git objects, refs, or index are written.
use crate::{
    candidate_inputs::{self, BoundFile},
    candidate_source,
    source_file::SourceRoot,
};
use anyhow::{ensure, Context, Result};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};
const MAX_FILES: usize = 4096;
const MAX_BYTES: usize = 32 * 1024 * 1024;
const MAX_FILE: usize = 2 * 1024 * 1024;
#[derive(Clone, Debug, PartialEq, Eq)]
struct Directory {
    identity: (u64, u64),
    names: Vec<String>,
}
pub(crate) struct LiveInputs {
    named: PathBuf,
    canonical: PathBuf,
    root: SourceRoot,
    identity: (u64, u64),
    files: BTreeMap<String, (BoundFile, Vec<u8>)>,
    absent: BTreeSet<String>,
    directories: BTreeMap<String, Option<Directory>>,
    native_inventory: Option<Vec<PathBuf>>,
    cargo_inventories: Vec<crate::mutation_cargo_capture::Held>,
    supporting: Option<crate::mutation_cargo_supporting::Held>,
    native_fingerprint: Option<String>,
    total: usize,
}
impl LiveInputs {
    pub(crate) fn open(named: &Path) -> Result<Self> {
        let canonical = named.canonicalize().context("resolve named corpus root")?;
        let root = SourceRoot::open(&canonical)?;
        let identity = root.identity()?;
        let this = Self {
            named: named.into(),
            canonical,
            root,
            identity,
            files: BTreeMap::new(),
            absent: BTreeSet::new(),
            directories: BTreeMap::new(),
            native_inventory: None,
            cargo_inventories: Vec::new(),
            supporting: None,
            native_fingerprint: None,
            total: 0,
        };
        this.verify_root()?;
        Ok(this)
    }
    fn verify_root(&self) -> Result<()> {
        ensure!(
            self.named.canonicalize()? == self.canonical,
            "named corpus root changed"
        );
        let reopened = SourceRoot::open(&self.named)?;
        let meta = fs::metadata(&self.named)?;
        ensure!(
            reopened.identity()? == self.identity
                && self.root.identity()? == self.identity
                && (meta.dev(), meta.ino()) == self.identity,
            "named corpus root identity changed"
        );
        Ok(())
    }
    fn path(&self, relative: &str) -> Result<PathBuf> {
        candidate_source::safe_path(relative.as_bytes())?;
        let path = self.canonical.join(relative);
        let mut parent = path.parent().context("corpus input parent missing")?;
        loop {
            match std::fs::symlink_metadata(parent) {
                Ok(metadata) => {
                    ensure!(
                        metadata.is_dir() && !metadata.file_type().is_symlink(),
                        "input parent is not a safe directory"
                    );
                    break;
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    parent = parent.parent().context("input ancestor missing")?;
                }
                Err(error) => return Err(error.into()),
            }
        }
        ensure!(
            parent.starts_with(&self.canonical) && parent.canonicalize()? == parent,
            "corpus parent has a symlink or escapes root"
        );
        Ok(path)
    }
    fn missing(&self, relative: &str) -> Result<bool> {
        let path = self.path(relative)?;
        match fs::symlink_metadata(path) {
            Ok(_) => Ok(false),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
            Err(error) => Err(error.into()),
        }
    }
    pub(crate) fn read(&mut self, relative: &str) -> Result<Option<&[u8]>> {
        if self.absent.contains(relative) {
            ensure!(
                self.missing(relative)?,
                "previously absent corpus input appeared"
            );
            return Ok(None);
        }
        if !self.files.contains_key(relative) {
            ensure!(
                self.files.len() + self.absent.len() < MAX_FILES,
                "corpus input count exceeds 4096"
            );
            if self.missing(relative)? {
                self.absent.insert(relative.into());
                return Ok(None);
            }
            ensure!(
                self.files.len() + self.absent.len() < MAX_FILES,
                "corpus input count exceeds 4096"
            );
            let path = self.path(relative)?;
            let (held, bytes) = candidate_inputs::repository_live(&path, MAX_FILE)?;
            self.total = self
                .total
                .checked_add(bytes.len())
                .context("corpus byte count overflow")?;
            ensure!(self.total <= MAX_BYTES, "corpus inputs exceed 32 MiB");
            self.files.insert(relative.into(), (held, bytes));
        }
        Ok(Some(
            self.files
                .get(relative)
                .context("held corpus input missing")?
                .1
                .as_slice(),
        ))
    }
    /// Hold absence of every Forge source root through final verification.
    pub(crate) fn native_inventory_absent(&mut self) -> Result<bool> {
        for root in [
            "native/forge",
            "native/forge-cli",
            "native/conductor-native",
            "native/slop-core",
        ] {
            if !self.missing(root)? {
                return Ok(false);
            }
        }
        for root in [
            "native/forge",
            "native/forge-cli",
            "native/conductor-native",
            "native/slop-core",
        ] {
            ensure!(self.read(root)?.is_none(), "native source root appeared");
        }
        Ok(true)
    }
    pub(crate) fn native_source_fingerprint(&mut self) -> Result<Option<String>> {
        use conductor_native::source_fingerprint::{self, Fingerprint};
        if let Some(value) = &self.native_fingerprint {
            return Ok(Some(value.clone()));
        }
        let paths = match source_fingerprint::files(&self.canonical) {
            Ok(paths) => paths,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let mut digest = Fingerprint::new();
        for path in &paths {
            let relative = path.to_str().context("native source path must be UTF-8")?;
            let bytes = self.read(relative)?.context("native source disappeared")?;
            digest.add(path, bytes)?;
        }
        ensure!(
            source_fingerprint::files(&self.canonical)? == paths,
            "native source inventory changed while capturing"
        );
        self.native_inventory = Some(paths);
        let value = digest.finish();
        self.native_fingerprint = Some(value.clone());
        Ok(Some(value))
    }
    pub(crate) fn json_files(&mut self, relative: &str) -> Result<Vec<String>> {
        if !self.directories.contains_key(relative) {
            ensure!(
                self.directories.len() < 64,
                "corpus directory inventory exceeds 64"
            );
            self.directories
                .insert(relative.into(), self.directory(relative)?);
        }
        Ok(self.directories[relative]
            .as_ref()
            .map_or_else(Vec::new, |value| value.names.clone()))
    }
    fn directory(&self, relative: &str) -> Result<Option<Directory>> {
        let path = self.path(relative)?;
        let before = match fs::symlink_metadata(&path) {
            Ok(meta) => meta,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        ensure!(
            before.is_dir() && path.canonicalize()? == path,
            "corpus inventory must be a non-symlink directory"
        );
        let mut names = Vec::new();
        let mut visited = 0usize;
        for entry in fs::read_dir(&path)? {
            visited += 1;
            ensure!(
                visited <= 100_000,
                "corpus directory scan exceeds 100000 entries"
            );
            let entry = entry?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| anyhow::anyhow!("corpus inventory names must be UTF-8"))?;
            if name.ends_with(".json") {
                names.push(format!("{relative}/{name}"));
            }
            ensure!(
                names.len() <= MAX_FILES,
                "corpus JSON inventory exceeds 4096"
            );
        }
        names.sort();
        let after = fs::symlink_metadata(&path)?;
        ensure!(
            before.dev() == after.dev()
                && before.ino() == after.ino()
                && before.mtime() == after.mtime()
                && before.mtime_nsec() == after.mtime_nsec()
                && before.ctime() == after.ctime()
                && before.ctime_nsec() == after.ctime_nsec(),
            "corpus inventory changed while reading"
        );
        Ok(Some(Directory {
            identity: (before.dev(), before.ino()),
            names,
        }))
    }
    pub(crate) fn cargo_inventory(
        &mut self,
        declaration: &str,
        selected: &[String],
        files: &mut BTreeMap<String, Vec<u8>>,
    ) -> Result<crate::mutation_cargo_capture::Held> {
        let budget =
            crate::candidate_review_mutation::inputs::cargo_budget(&self.cargo_inventories)?;
        let held = crate::mutation_cargo_capture::Held::capture_with_limit(
            &self.canonical,
            declaration,
            selected,
            budget,
        )?;
        ensure!(
            held.file_count() <= budget,
            "Cargo held file budget exceeded"
        );
        let mut paths: Vec<String> = held.inventory().files().keys().cloned().collect();
        paths.push(declaration.to_owned());
        for path in paths {
            if let std::collections::btree_map::Entry::Vacant(entry) = files.entry(path.clone()) {
                let bytes = self
                    .read(&path)?
                    .with_context(|| format!("Cargo input disappeared: {path}"))?
                    .to_vec();
                entry.insert(bytes);
            }
        }
        ensure!(
            files
                .get(declaration)
                .is_some_and(|bytes| bytes == held.declaration_bytes()),
            "Cargo declaration changed during capture"
        );
        Ok(held)
    }
    pub(crate) fn retain_cargo(&mut self, held: crate::mutation_cargo_capture::Held) {
        self.cargo_inventories.push(held);
    }
    pub(crate) fn supporting_inventory(
        &mut self,
        required: &BTreeMap<String, String>,
    ) -> Result<conductor_native::mutation_cargo_supporting_binding::SupportingInputs> {
        let budget =
            crate::candidate_review_mutation::inputs::cargo_budget(&self.cargo_inventories)?;
        let held =
            crate::mutation_cargo_supporting::Held::capture(&self.canonical, budget, required)?;
        let value = held.inventory().clone();
        self.supporting = Some(held);
        Ok(value)
    }
    pub(crate) fn verify(&self) -> Result<()> {
        self.verify_root()?;
        if let Some(paths) = &self.native_inventory {
            ensure!(
                &conductor_native::source_fingerprint::files(&self.canonical)? == paths,
                "native source inventory changed"
            );
        }
        for (path, (held, _)) in &self.files {
            held.verify()
                .with_context(|| format!("corpus input changed: {path}"))?;
        }
        for path in &self.absent {
            ensure!(
                self.missing(path)?,
                "previously absent corpus input appeared: {path}"
            );
        }
        for (path, expected) in &self.directories {
            ensure!(
                &self.directory(path)? == expected,
                "corpus inventory changed: {path}"
            );
        }
        for held in &self.cargo_inventories {
            held.verify()?;
        }
        if let Some(held) = &self.supporting {
            held.verify()?;
        }
        self.verify_root()
    }
}
#[cfg(test)]
#[path = "mutation_live_inputs_tests.rs"]
mod tests;
