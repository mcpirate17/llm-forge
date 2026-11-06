//! Bounded cooperative capture of dead-test source bytes and import lookups.
//! Observations are rechecked, not an atomic snapshot or same-UID attestation.
use anyhow::{ensure, Context, Result};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::Read,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Component, Path, PathBuf},
};
pub(super) const MAX_FILES: usize = 100_000;
const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_TOTAL_BYTES: u64 = 1024 * 1024 * 1024;
pub(super) fn valid_path(path: &str) -> bool {
    !path.is_empty()
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
        && path.len() <= 4096
        && Path::new(path)
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
}
pub(super) fn read_raw(
    root: &Path,
    path: &str,
    total: &mut u64,
) -> Result<(Vec<u8>, fs::Metadata)> {
    ensure!(valid_path(path), "invalid import-closure path: {path}");
    let absolute = root.join(path);
    let canonical = absolute
        .canonicalize()
        .with_context(|| format!("resolve closure source {path}"))?;
    ensure!(
        canonical == absolute,
        "closure source contains a symlink: {path}"
    );
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(&absolute)?;
    let before = file.metadata()?;
    ensure!(
        before.is_file() && before.len() <= MAX_FILE_BYTES,
        "closure source is not a bounded file: {path}"
    );
    *total = total
        .checked_add(before.len())
        .context("closure byte count overflow")?;
    ensure!(
        *total <= MAX_TOTAL_BYTES,
        "closure source byte budget exceeded"
    );
    let mut bytes = Vec::new();
    file.by_ref()
        .take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= MAX_FILE_BYTES,
        "closure source grew beyond limit: {path}"
    );
    let after = file.metadata()?;
    let named = fs::symlink_metadata(&absolute)?;
    ensure!(
        named.is_file() && (named.dev(), named.ino()) == (after.dev(), after.ino()),
        "closure source name changed while reading: {path}"
    );
    ensure!(
        (
            before.dev(),
            before.ino(),
            before.len(),
            before.mtime(),
            before.mtime_nsec(),
            before.ctime(),
            before.ctime_nsec()
        ) == (
            after.dev(),
            after.ino(),
            after.len(),
            after.mtime(),
            after.mtime_nsec(),
            after.ctime(),
            after.ctime_nsec()
        ) && absolute.canonicalize()? == absolute,
        "closure source changed while reading: {path}"
    );
    Ok((bytes, after))
}
pub(super) fn safe_lookup(
    root: &Path,
    path: &str,
    seen: &mut BTreeMap<String, bool>,
    bytes: &mut usize,
) -> Result<bool> {
    ensure!(valid_path(path), "invalid import lookup path: {path}");
    let absolute = root.join(path);
    let mut parent = absolute.parent().context("import lookup parent missing")?;
    loop {
        match fs::symlink_metadata(parent) {
            Ok(meta) => {
                ensure!(
                    meta.is_dir() && !meta.file_type().is_symlink(),
                    "unsafe import parent"
                );
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                parent = parent.parent().context("import ancestor missing")?
            }
            Err(error) => return Err(error.into()),
        }
    }
    ensure!(
        parent.starts_with(root) && parent.canonicalize()? == parent,
        "import parent contains symlink"
    );
    let present = match fs::symlink_metadata(&absolute) {
        Ok(meta) => {
            ensure!(
                !meta.file_type().is_symlink(),
                "symlink import candidate refused: {path}"
            );
            meta.is_file()
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => return Err(error.into()),
    };
    if let Some(previous) = seen.get(path) {
        ensure!(
            *previous == present,
            "import lookup changed during scan: {path}"
        );
    } else {
        *bytes = bytes
            .checked_add(path.len())
            .context("import lookup bytes overflow")?;
        ensure!(
            seen.len() < 100_000 && *bytes <= 16 * 1024 * 1024,
            "import lookup budget exceeded"
        );
        seen.insert(path.into(), present);
    }
    Ok(present)
}
#[derive(Debug, PartialEq, Eq)]
struct Stamp {
    dev: u64,
    ino: u64,
    mode: u32,
    uid: u32,
    links: u64,
    file_len: u64,
    bytes: u64,
    mtime: (i64, i64),
    ctime: (i64, i64),
    sha256: String,
}
fn stamp(meta: &fs::Metadata, bytes: &[u8]) -> Stamp {
    Stamp {
        dev: meta.dev(),
        ino: meta.ino(),
        mode: meta.mode(),
        uid: meta.uid(),
        links: meta.nlink(),
        file_len: meta.len(),
        bytes: bytes.len() as u64,
        mtime: (meta.mtime(), meta.mtime_nsec()),
        ctime: (meta.ctime(), meta.ctime_nsec()),
        sha256: format!("{:x}", Sha256::digest(bytes)),
    }
}
pub struct Capture {
    named: PathBuf,
    root: PathBuf,
    identity: (u64, u64),
    sources: BTreeMap<String, Stamp>,
    lookups: BTreeMap<String, bool>,
    total: u64,
    lookup_bytes: usize,
}
impl Capture {
    pub fn open(named: &Path) -> Result<Self> {
        let root = named.canonicalize().context("resolve closure repository")?;
        let meta = fs::metadata(&root)?;
        ensure!(meta.is_dir(), "closure repository must be a directory");
        let held = Self {
            named: named.to_owned(),
            root,
            identity: (meta.dev(), meta.ino()),
            sources: BTreeMap::new(),
            lookups: BTreeMap::new(),
            total: 0,
            lookup_bytes: 0,
        };
        held.verify_root()?;
        Ok(held)
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    /// Observed source path, SHA-256 and byte length, in deterministic path order.
    pub fn sources(&self) -> impl Iterator<Item = (&str, &str, u64)> {
        self.sources
            .iter()
            .map(|(path, value)| (path.as_str(), value.sha256.as_str(), value.bytes))
    }
    /// Positive and negative file-existence observations.
    pub fn lookups(&self) -> &BTreeMap<String, bool> {
        &self.lookups
    }
    pub fn root_identity(&self) -> (u64, u64) {
        self.identity
    }
    fn verify_root(&self) -> Result<()> {
        let meta = fs::metadata(&self.named)?;
        ensure!(
            self.named.canonicalize()? == self.root
                && meta.is_dir()
                && (meta.dev(), meta.ino()) == self.identity,
            "closure named root changed"
        );
        Ok(())
    }
    pub fn read_bytes(&mut self, path: &str) -> Result<Vec<u8>> {
        ensure!(
            self.sources.contains_key(path) || self.sources.len() < MAX_FILES,
            "closure file budget exceeded"
        );
        let mut measured = 0;
        let (bytes, meta) = read_raw(&self.root, path, &mut measured)?;
        let observed = stamp(&meta, &bytes);
        if let Some(previous) = self.sources.get(path) {
            ensure!(*previous == observed, "captured source changed: {path}");
        } else {
            self.total = self
                .total
                .checked_add(observed.bytes)
                .context("closure byte count overflow")?;
            ensure!(
                self.total <= MAX_TOTAL_BYTES,
                "closure source byte budget exceeded"
            );
            self.sources.insert(path.into(), observed);
        }
        Ok(bytes)
    }
    pub fn is_file(&mut self, path: &str) -> Result<bool> {
        safe_lookup(&self.root, path, &mut self.lookups, &mut self.lookup_bytes)
    }
    pub fn verify(&self) -> Result<()> {
        self.verify_root()?;
        let mut total = 0;
        let mut actual_total = 0u64;
        for (path, expected) in &self.sources {
            let (bytes, meta) = read_raw(&self.root, path, &mut total)?;
            actual_total = actual_total
                .checked_add(bytes.len() as u64)
                .context("closure byte count overflow")?;
            ensure!(
                actual_total <= MAX_TOTAL_BYTES,
                "closure source byte budget exceeded"
            );
            ensure!(
                stamp(&meta, &bytes) == *expected,
                "captured source changed: {path}"
            );
        }
        let mut lookups = BTreeMap::new();
        let mut lookup_bytes = 0;
        for (path, expected) in &self.lookups {
            let present = safe_lookup(&self.root, path, &mut lookups, &mut lookup_bytes)?;
            ensure!(
                present == *expected,
                "captured import lookup changed: {path}"
            );
        }
        self.verify_root()
    }
}
impl super::report::Inputs for Capture {
    fn read(&mut self, path: &str) -> Result<Vec<u8>> {
        self.read_bytes(path)
    }
    fn is_file(&mut self, path: &str) -> Result<bool> {
        Capture::is_file(self, path)
    }
}
#[cfg(test)]
#[path = "dead_tests_inputs_tests.rs"]
mod tests;
