//! Held, bounded package enumeration for opt-in Cargo test-value evidence.
//! This binds supported discovery inputs, not compiler or runtime-data closure.
use crate::candidate_inputs::{self, BoundFile, DirectoryBinding};
use anyhow::{ensure, Context, Result};
use conductor_native::mutation_cargo_inventory::{self as inventory, Entry, Inventory};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

const MAX_ENTRIES: usize = 16384;
const MAX_BYTES: usize = 32 * 1024 * 1024;
type CapturedFiles = Vec<(String, BoundFile)>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Directory,
    Regular,
    Other,
}

/// Enumerate names and types, including forbidden inputs; never follow symlinks.
pub(crate) fn names(root: &Path) -> Result<BTreeMap<String, Kind>> {
    let mut result = BTreeMap::new();
    visit(root, "", &mut result)?;
    Ok(result)
}
fn visit(root: &Path, parent: &str, entries: &mut BTreeMap<String, Kind>) -> Result<()> {
    ensure!(
        parent.split('/').count() <= 128,
        "Cargo inventory depth exceeds 128"
    );
    let directory = root.join(parent);
    let before = fs::symlink_metadata(&directory)?;
    ensure!(
        before.is_dir() && !before.file_type().is_symlink(),
        "Cargo inventory directory is unsafe"
    );
    for entry in fs::read_dir(&directory)? {
        let entry = entry?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| anyhow::anyhow!("Cargo input name is not UTF-8"))?;
        let path = if parent.is_empty() {
            name
        } else {
            format!("{parent}/{name}")
        };
        crate::candidate_source::safe_path(path.as_bytes())?;
        ensure!(
            entries.len() < MAX_ENTRIES,
            "Cargo inventory exceeds 16384 entries"
        );
        let ty = entry.file_type()?;
        let kind = if ty.is_dir() {
            Kind::Directory
        } else if ty.is_file() {
            Kind::Regular
        } else {
            Kind::Other
        };
        entries.insert(path.clone(), kind);
        if kind == Kind::Directory && !inventory::excluded(&path) {
            visit(root, &path, entries)?;
        }
    }
    use std::os::unix::fs::MetadataExt;
    let after = fs::symlink_metadata(directory)?;
    ensure!(
        before.dev() == after.dev()
            && before.ino() == after.ino()
            && before.mtime() == after.mtime()
            && before.mtime_nsec() == after.mtime_nsec()
            && before.ctime() == after.ctime()
            && before.ctime_nsec() == after.ctime_nsec(),
        "Cargo inventory changed during enumeration"
    );
    Ok(())
}

pub(crate) struct Held {
    named: PathBuf,
    root: PathBuf,
    directory: DirectoryBinding,
    declaration: String,
    declaration_bytes: Vec<u8>,
    selected: Vec<String>,
    files: CapturedFiles,
    inventory: Inventory,
    max_files: usize,
}
impl Held {
    pub(crate) fn capture(root: &Path, declaration: &str, selected: &[String]) -> Result<Self> {
        Self::capture_with_limit(root, declaration, selected, 4096)
    }
    pub(crate) fn capture_with_limit(
        root: &Path,
        declaration: &str,
        selected: &[String],
        max_files: usize,
    ) -> Result<Self> {
        ensure!(
            (1..=4096).contains(&max_files),
            "Cargo capture file budget must be 1..4096"
        );
        crate::candidate_source::safe_path(declaration.as_bytes())?;
        ensure!(
            !inventory::excluded(declaration),
            "Cargo declaration is in an excluded directory"
        );
        let named = root.to_path_buf();
        let root = root.canonicalize()?;
        let directory = DirectoryBinding::repository(&root, None)?;
        let (entries, files) = read_entries(&root, declaration, max_files)?;
        let bytes = match entries.get(declaration) {
            Some(Entry::Regular(Some(bytes))) => bytes,
            _ => anyhow::bail!("Cargo value declaration is not a regular captured file"),
        };
        let inventory = inventory::build(bytes, selected, &entries).map_err(anyhow::Error::msg)?;
        let held = Self {
            named,
            root,
            directory,
            declaration: declaration.into(),
            declaration_bytes: bytes.clone(),
            selected: selected.into(),
            files,
            inventory,
            max_files,
        };
        held.verify()?;
        Ok(held)
    }
    pub(crate) fn inventory(&self) -> &Inventory {
        &self.inventory
    }
    pub(crate) fn file_count(&self) -> usize {
        self.files.len()
    }
    pub(crate) fn declaration_bytes(&self) -> &[u8] {
        &self.declaration_bytes
    }
    pub(crate) fn verify(&self) -> Result<()> {
        ensure!(
            self.named.canonicalize()? == self.root,
            "named Cargo package root changed"
        );
        self.directory.verify()?;
        for (_, file) in &self.files {
            file.verify()?;
        }
        let (entries, _held) = read_entries(&self.root, &self.declaration, self.max_files)?;
        let Some(Entry::Regular(Some(bytes))) = entries.get(&self.declaration) else {
            anyhow::bail!("Cargo value declaration disappeared");
        };
        let current =
            inventory::build(bytes, &self.selected, &entries).map_err(anyhow::Error::msg)?;
        ensure!(
            current == self.inventory,
            "Cargo test-discovery inputs changed"
        );
        self.directory.verify()
    }

    /// Generated artifacts must never replace a captured input, including aliases.
    pub(crate) fn check_output(&self, path: &Path) -> Result<()> {
        self.verify()?;
        use std::os::unix::fs::MetadataExt;
        for (name, _) in &self.files {
            let input = self.root.join(name);
            ensure!(
                path != input,
                "generated output overlaps Cargo input: {name}"
            );
            match fs::metadata(path) {
                Ok(output) => {
                    let source = fs::metadata(&input)?;
                    ensure!(
                        (output.dev(), output.ino()) != (source.dev(), source.ino()),
                        "generated output aliases Cargo input: {name}"
                    );
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "mutation_cargo_capture_tests.rs"]
mod tests;

fn read_entries(
    root: &Path,
    declaration: &str,
    max_files: usize,
) -> Result<(BTreeMap<String, Entry>, CapturedFiles)> {
    let before = names(root)?;
    let mut result = BTreeMap::new();
    let mut files = Vec::new();
    let mut total = 0usize;
    for (path, kind) in &before {
        let entry = match kind {
            Kind::Directory => Entry::Directory,
            Kind::Other => Entry::SymlinkOrSpecial,
            Kind::Regular
                if !inventory::excluded(path)
                    && (inventory::relevant(path) || path == declaration) =>
            {
                ensure!(
                    files.len() < max_files,
                    "Cargo capture file budget exceeded"
                );
                let (held, bytes) =
                    candidate_inputs::repository_live(&root.join(path), 2 * 1024 * 1024)?;
                total = total
                    .checked_add(bytes.len())
                    .context("Cargo inventory byte overflow")?;
                ensure!(
                    total <= MAX_BYTES && files.len() < 4096,
                    "Cargo inventory exceeds file or byte budget"
                );
                files.push((path.clone(), held));
                Entry::Regular(Some(bytes))
            }
            Kind::Regular => Entry::Regular(None),
        };
        result.insert(path.clone(), entry);
    }
    ensure!(
        before == names(root)?,
        "Cargo inventory changed while reading"
    );
    Ok((result, files))
}
