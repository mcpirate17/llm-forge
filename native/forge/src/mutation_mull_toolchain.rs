//! Operator-pinned, bounded Mull runtime inventory. Verification detects persistent
//! drift; it does not claim protection from transient same-UID modification.
use crate::{
    candidate_engine_identity::{executable_file, BinaryIdentity},
    candidate_inputs::DirectoryBinding,
    mutation_cargo_seed::{
        inventory_root, inventory_tree, verify_inventory_root, InventoryBudget, InventoryEntry,
    },
    mutation_execution_grants::Grants,
};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

const MAX_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_FILE: u64 = 256 * 1024 * 1024;
const MAX_FILES: usize = 256;
const MAX_DIRS: usize = 8;
const ALIASES: [&str; 8] = [
    "cmake",
    "ninja",
    "clang",
    "ld",
    "llvm-profdata",
    "mull-runner",
    "sh",
    "uname",
];

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Config {
    pub llvm_major: u32,
    pub plugin: PathBuf,
    pub files: BTreeMap<PathBuf, String>,
    pub directories: BTreeMap<PathBuf, String>,
}

fn valid_hash(hash: &str) -> bool {
    hash.len() == 64
        && hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn validate_paths(
    config: &Config,
    read_only: &[PathBuf],
    programs: &BTreeMap<String, PathBuf>,
) -> Result<()> {
    ensure!(
        config.llvm_major == 18,
        "Mull profile requires LLVM major 18"
    );
    ensure!(
        config.files.len() <= MAX_FILES && config.directories.len() <= MAX_DIRS,
        "Mull pin count exceeds limit"
    );
    ensure!(
        config.files.contains_key(&config.plugin),
        "Mull plugin requires an exact file pin"
    );
    ensure!(
        config
            .files
            .values()
            .chain(config.directories.values())
            .all(|h| valid_hash(h)),
        "Mull pins require lowercase SHA-256"
    );
    let aliases: BTreeSet<_> = programs.keys().map(String::as_str).collect();
    ensure!(
        aliases == ALIASES.into_iter().collect(),
        "Mull requires the exact program alias set"
    );
    let pins: Vec<_> = config
        .files
        .keys()
        .chain(config.directories.keys())
        .collect();
    for (i, path) in pins.iter().enumerate() {
        ensure!(
            path.is_absolute() && path.canonicalize()? == **path,
            "Mull pin must be canonical: {}",
            path.display()
        );
        ensure!(
            read_only.contains(path),
            "Mull pin has no exact read grant: {}",
            path.display()
        );
        for other in pins.iter().skip(i + 1) {
            ensure!(
                !path.starts_with(other) && !other.starts_with(path),
                "overlapping Mull pins"
            );
        }
        ensure!(
            !programs
                .values()
                .any(|p| p.starts_with(path) || path.starts_with(p)),
            "Mull pin overlaps program grant"
        );
    }
    for path in read_only {
        ensure!(
            pins.contains(&path) || programs.values().any(|p| p == path),
            "uncovered Mull read grant: {}",
            path.display()
        );
    }
    Ok(())
}

type Stamp = (u64, u64, u64, u32, u32, u64, i64, i64, i64, i64);
fn stamp(m: &fs::Metadata) -> Stamp {
    (
        m.dev(),
        m.ino(),
        m.len(),
        m.mode(),
        m.uid(),
        m.nlink(),
        m.mtime(),
        m.mtime_nsec(),
        m.ctime(),
        m.ctime_nsec(),
    )
}
fn open_file(path: &Path) -> Result<File> {
    ensure!(
        path.is_absolute() && path.canonicalize()? == path,
        "Mull file must be canonical"
    );
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)?;
    let m = file.metadata()?;
    ensure!(
        m.is_file()
            && m.len() <= MAX_FILE
            && m.nlink() == 1
            && m.mode() & 0o7022 == 0
            && (m.uid() == 0 || m.uid() == unsafe { libc::geteuid() }),
        "unsafe or oversized Mull file: {}",
        path.display()
    );
    ensure!(
        stamp(&m) == stamp(&fs::symlink_metadata(path)?),
        "Mull file changed during open"
    );
    Ok(file)
}
struct HeldFile {
    path: PathBuf,
    parent: DirectoryBinding,
    file: File,
    stamp: Stamp,
    identity: BinaryIdentity,
}
impl HeldFile {
    fn capture(path: &Path, expected: &str, remaining: u64) -> Result<Self> {
        let parent =
            DirectoryBinding::open(path.parent().context("Mull file parent missing")?, None)?;
        let file = open_file(path)?;
        let before = stamp(&file.metadata()?);
        ensure!(
            before.2 <= remaining,
            "Mull total byte budget exceeded before hashing"
        );
        let identity = executable_file(file.try_clone()?, || Ok(()))?;
        ensure!(
            identity.sha256 == expected,
            "Mull file digest mismatch: {}",
            path.display()
        );
        let held = Self {
            path: path.into(),
            parent,
            file,
            stamp: before,
            identity,
        };
        held.verify()?;
        Ok(held)
    }
    fn verify(&self) -> Result<()> {
        self.parent.verify()?;
        let current = open_file(&self.path)?;
        ensure!(
            stamp(&current.metadata()?) == self.stamp
                && stamp(&self.file.metadata()?) == self.stamp,
            "Mull file identity changed: {}",
            self.path.display()
        );
        let identity = executable_file(current, || Ok(()))?;
        ensure!(
            identity == self.identity && stamp(&fs::symlink_metadata(&self.path)?) == self.stamp,
            "Mull file content or name changed: {}",
            self.path.display()
        );
        self.parent.verify()
    }
}

/// Observe a directory's sorted inventory digest for an operator to pin.
/// This observation alone grants no authority to execute its contents.
pub(crate) fn observe_directory(path: &Path) -> Result<String> {
    let root = inventory_root(path)?;
    check_root(path, &root)?;
    let entries = inventory_tree(&root, None, "", &mut InventoryBudget::mull())?;
    check_root(path, &root)?;
    inventory_digest(&entries)
}
fn check_root(path: &Path, root: &cap_std::fs::Dir) -> Result<()> {
    use cap_std::fs::MetadataExt as _;
    verify_inventory_root(path, root)?;
    let meta = root.dir_metadata()?;
    ensure!(
        meta.is_dir()
            && meta.mode() & 0o022 == 0
            && (meta.uid() == 0 || meta.uid() == unsafe { libc::geteuid() }),
        "unsafe opened Mull inventory root"
    );
    match fs::symlink_metadata(path.join("seed-manifest.json")) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
        Err(error) => return Err(error.into()),
        Ok(_) => anyhow::bail!("Mull inventory contains reserved seed-manifest.json"),
    }
    verify_inventory_root(path, root)
}
fn inventory_digest(entries: &[InventoryEntry]) -> Result<String> {
    ensure!(
        entries.windows(2).all(|w| w[0].path < w[1].path),
        "Mull inventory must be globally sorted"
    );
    Ok(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(entries)?)
    ))
}
struct HeldDirectory {
    path: PathBuf,
    root: cap_std::fs::Dir,
    parent: DirectoryBinding,
    entries: Vec<InventoryEntry>,
}
impl HeldDirectory {
    fn capture(path: &Path, expected: &str, budget: &mut InventoryBudget) -> Result<Self> {
        let parent = DirectoryBinding::open(
            path.parent().context("Mull directory parent missing")?,
            None,
        )?;
        let root = inventory_root(path)?;
        check_root(path, &root)?;
        let entries = inventory_tree(&root, None, "", budget)?;
        check_root(path, &root)?;
        ensure!(
            inventory_digest(&entries)? == expected,
            "Mull directory digest mismatch: {}",
            path.display()
        );
        parent.verify()?;
        Ok(Self {
            path: path.into(),
            root,
            parent,
            entries,
        })
    }
    fn verify(&self, budget: &mut InventoryBudget) -> Result<()> {
        self.parent.verify()?;
        check_root(&self.path, &self.root)?;
        let entries = inventory_tree(&self.root, None, "", budget)?;
        ensure!(
            entries == self.entries,
            "Mull directory inventory changed: {}",
            self.path.display()
        );
        check_root(&self.path, &self.root)?;
        self.parent.verify()
    }
}

pub(crate) struct Held {
    plugin: PathBuf,
    files: Vec<HeldFile>,
    directories: Vec<HeldDirectory>,
}
impl Held {
    pub(crate) fn capture(config: &Config, grants: &Grants) -> Result<Self> {
        let programs = grants.programs();
        validate_paths(config, &grants.read_only, &programs)?;
        for alias in ALIASES {
            ensure!(
                valid_hash(grants.program_sha256(alias)?),
                "invalid program digest"
            );
        }
        let mut file_bytes = 0u64;
        let mut files = Vec::with_capacity(config.files.len());
        for (path, hash) in &config.files {
            let file = HeldFile::capture(path, hash, MAX_BYTES - file_bytes)?;
            file_bytes += file.identity.bytes;
            files.push(file);
        }
        let mut budget = InventoryBudget::mull_with_bytes(file_bytes)?;
        let directories = config
            .directories
            .iter()
            .map(|(p, h)| HeldDirectory::capture(p, h, &mut budget))
            .collect::<Result<Vec<_>>>()?;
        let held = Self {
            plugin: config.plugin.clone(),
            files,
            directories,
        };
        held.verify()?;
        Ok(held)
    }
    pub(crate) fn plugin(&self) -> &Path {
        &self.plugin
    }
    pub(crate) fn identity(&self) -> Result<serde_json::Value> {
        let files: BTreeMap<_, _> = self
            .files
            .iter()
            .map(|f| (&f.path, &f.identity.sha256))
            .collect();
        let directories: BTreeMap<_, _> = self
            .directories
            .iter()
            .map(|d| Ok((&d.path, inventory_digest(&d.entries)?)))
            .collect::<Result<_>>()?;
        Ok(serde_json::json!({
            "llvm_major": 18,
            "plugin": serde_json::to_value(&self.plugin)?,
            "files": serde_json::to_value(files)?,
            "directories": serde_json::to_value(directories)?,
            "scope": "point-in-time content binding, not producer attestation"
        }))
    }
    pub(crate) fn verify(&self) -> Result<()> {
        let mut bytes = 0u64;
        for file in &self.files {
            file.verify()?;
            bytes = bytes
                .checked_add(file.identity.bytes)
                .context("Mull byte count overflow")?;
        }
        let mut budget = InventoryBudget::mull_with_bytes(bytes)?;
        for directory in &self.directories {
            directory.verify(&mut budget)?;
            for entry in &directory.entries {
                bytes = bytes
                    .checked_add(entry.bytes)
                    .context("Mull byte count overflow")?;
            }
        }
        ensure!(bytes <= MAX_BYTES, "Mull total byte budget exceeded");
        Ok(())
    }
}
#[cfg(test)]
#[path = "mutation_mull_toolchain_tests.rs"]
mod tests;
