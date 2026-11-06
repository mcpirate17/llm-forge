//! Streaming, bounded seed inventory/copy. Directory identity never pins content.
use anyhow::{ensure, Context, Result};
use cap_std::fs::{
    Dir, DirBuilder, DirBuilderExt, Metadata, MetadataExt, OpenOptions, OpenOptionsExt,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt as _, PermissionsExt},
    path::{Component, Path},
    time::{Duration, Instant},
};
pub(super) const MAX_ENTRIES: usize = 100_000;
pub(super) const MAX_TOTAL: u64 = 4 * 1024 * 1024 * 1024;
pub(super) const MAX_FILE: u64 = 256 * 1024 * 1024;
pub(super) const MAX_DOCUMENT: usize = 16 * 1024 * 1024;
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Entry {
    pub path: String,
    pub directory: bool,
    pub bytes: u64,
    pub sha256: Option<String>,
    pub executable: bool,
}
#[derive(Clone, Copy)]
enum ReadPolicy {
    StrictSeed,
    PublicCache,
    PrivateGenerated,
}
impl ReadPolicy {
    fn allows(self, metadata: &Metadata) -> bool {
        let uid = metadata.uid();
        let current = unsafe { libc::geteuid() };
        if uid != 0 && uid != current {
            return false;
        }
        match self {
            Self::StrictSeed => metadata.mode() & 0o022 == 0,
            Self::PublicCache => {
                metadata.mode() & 0o002 == 0
                    && if uid == 0 {
                        metadata.mode() & 0o020 == 0
                    } else {
                        uid == current
                    }
            }
            Self::PrivateGenerated => true,
        }
    }
}
pub(crate) struct Budget {
    start: Instant,
    entries: usize,
    bytes: u64,
    document: usize,
    expanded: u64,
    max_entries: usize,
    max_total: u64,
    max_file: u64,
}
impl Budget {
    pub fn new() -> Self {
        Self {
            start: Instant::now(),
            entries: 0,
            bytes: 0,
            document: 0,
            expanded: 0,
            max_entries: MAX_ENTRIES,
            max_total: MAX_TOTAL,
            max_file: MAX_FILE,
        }
    }
    pub(crate) fn guest() -> Self {
        Self {
            max_entries: 10_000,
            max_total: 256 * 1024 * 1024,
            max_file: 32 * 1024 * 1024,
            ..Self::new()
        }
    }
    pub(super) fn deadline(&self) -> Instant {
        self.start + Duration::from_secs(120)
    }
    pub(super) fn check(&self) -> Result<()> {
        ensure!(
            self.start.elapsed() <= Duration::from_secs(120),
            "Cargo seed cooperative I/O deadline exceeded"
        );
        Ok(())
    }
    pub(super) fn add(&mut self, entry: &Entry) -> Result<()> {
        self.check()?;
        self.entries = self.entries.checked_add(1).context("seed entry overflow")?;
        self.bytes = self
            .bytes
            .checked_add(entry.bytes)
            .context("seed byte overflow")?;
        self.document = self
            .document
            .checked_add(serde_json::to_vec(entry)?.len() + 1)
            .context("seed manifest size overflow")?;
        ensure!(
            self.entries <= self.max_entries
                && self.bytes <= self.max_total
                && self.document <= MAX_DOCUMENT / 2,
            "Cargo seed entry/byte/manifest budget exceeded"
        );
        Ok(())
    }
    pub(super) fn expand(&mut self, bytes: u64) -> Result<()> {
        self.check()?;
        self.expanded = self
            .expanded
            .checked_add(bytes)
            .context("expanded seed size overflow")?;
        ensure!(
            self.expanded <= MAX_TOTAL,
            "expanded Cargo seed exceeds 4GiB"
        );
        Ok(())
    }
}
pub(super) fn relative(path: &str) -> Result<()> {
    ensure!(
        !path.is_empty()
            && path.len() <= 4096
            && !path.bytes().any(|b| b.is_ascii_control())
            && Path::new(path)
                .components()
                .all(|c| matches!(c, Component::Normal(_))),
        "unsafe Cargo seed relative path"
    );
    ensure!(
        Path::new(path).components().count() <= 128
            && !Path::new(path)
                .components()
                .any(|c| c.as_os_str() == ".git"),
        "Cargo seed depth exceeds 128"
    );
    Ok(())
}
pub(crate) fn root(path: &Path) -> Result<Dir> {
    ensure!(
        path.is_absolute() && path.canonicalize()? == path,
        "Cargo seed root must be absolute/canonical"
    );
    let meta = fs::symlink_metadata(path)?;
    use std::os::unix::fs::MetadataExt as _;
    ensure!(
        meta.is_dir()
            && !meta.file_type().is_symlink()
            && meta.mode() & 0o022 == 0
            && (meta.uid() == 0 || meta.uid() == unsafe { libc::geteuid() }),
        "unsafe Cargo seed root ownership/mode"
    );
    Ok(Dir::open_ambient_dir(path, cap_std::ambient_authority())?)
}
pub(crate) fn verify_root(path: &Path, held: &Dir) -> Result<()> {
    use std::os::unix::fs::MetadataExt as _;
    ensure!(
        path.canonicalize()? == path,
        "named seed root became aliased"
    );
    let current = fs::symlink_metadata(path)?;
    let before = held.dir_metadata()?;
    ensure!(
        current.is_dir()
            && current.dev() == before.dev()
            && current.ino() == before.ino()
            && current.uid() == before.uid()
            && current.mode() == before.mode(),
        "named seed root identity changed"
    );
    Ok(())
}
pub(super) fn manifest(target: &Dir, bytes: &[u8]) -> Result<()> {
    ensure!(bytes.len() <= MAX_DOCUMENT, "seed manifest exceeds bound");
    let mut output = target.open_with(
        "seed-manifest.json",
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC),
    )?;
    output.write_all(bytes)?;
    output.flush()?;
    output.sync_all()?;
    Ok(())
}
pub(super) fn new_root(path: &Path, exclusions: &[&Path]) -> Result<Dir> {
    ensure!(
        path.is_absolute()
            && path
                .components()
                .all(|c| matches!(c, Component::RootDir | Component::Normal(_))),
        "seed destination must be absolute/normalized"
    );
    ensure!(
        path.parent()
            .context("seed destination parent missing")?
            .canonicalize()?
            == path.parent().unwrap(),
        "seed destination parent is aliased"
    );
    for excluded in exclusions {
        ensure!(
            !path.starts_with(excluded) && !excluded.starts_with(path),
            "Cargo seed destination aliases an input"
        );
    }
    fs::DirBuilder::new()
        .mode(0o700)
        .create(path)
        .context("seed destination must be new")?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    root(path)
}
fn dir_options() -> OpenOptions {
    let mut opts = OpenOptions::new();
    opts.read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY | libc::O_NONBLOCK | libc::O_CLOEXEC);
    opts
}
pub(super) fn open_dir(root: &Dir, path: &str) -> Result<Dir> {
    open_dir_mode(root, path, ReadPolicy::StrictSeed)
}
fn open_dir_mode(root: &Dir, path: &str, policy: ReadPolicy) -> Result<Dir> {
    relative(path)?;
    let mut current = root.try_clone()?;
    for component in Path::new(path).components() {
        current = child(&current, component.as_os_str(), policy, false)?;
    }
    Ok(current)
}
fn child(
    parent: &Dir,
    name: &std::ffi::OsStr,
    policy: ReadPolicy,
    sticky_ancestor: bool,
) -> Result<Dir> {
    let before = parent.symlink_metadata(name)?;
    let sticky = sticky_ancestor && before.uid() == 0 && before.mode() & libc::S_ISVTX != 0;
    ensure!(
        before.is_dir() && (policy.allows(&before) || sticky),
        "unsafe seed directory ownership/mode"
    );
    let opened = Dir::from_std_file(parent.open_with(name, &dir_options())?.into_std());
    ensure!(
        stamp(&before) == stamp(&opened.dir_metadata()?)
            && stamp(&before) == stamp(&parent.symlink_metadata(name)?),
        "opened/named seed directory identity changed"
    );
    Ok(opened)
}
pub(super) fn mkdir(root: &Dir, path: &str) -> Result<()> {
    relative(path)?;
    let mut current = root.try_clone()?;
    for component in Path::new(path).components() {
        match current.create_dir_with(component.as_os_str(), DirBuilder::new().mode(0o700)) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e.into()),
        }
        current = Dir::from_std_file(
            current
                .open_with(component.as_os_str(), &dir_options())?
                .into_std(),
        );
    }
    Ok(())
}
/// Create only a fresh private extracted file; never restore archive metadata.
pub(super) fn extracted_file(
    root: &Dir,
    path: &str,
    executable: bool,
) -> Result<cap_std::fs::File> {
    relative(path)?;
    let path = Path::new(path);
    let parent = path.parent().context("extracted file parent missing")?;
    let directory = if parent.as_os_str().is_empty() {
        root.try_clone()?
    } else {
        let parent = parent.to_str().context("extracted parent is not UTF8")?;
        mkdir(root, parent)?;
        open_dir(root, parent)?
    };
    Ok(directory.open_with(
        path.file_name().context("extracted file name missing")?,
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(if executable { 0o700 } else { 0o600 })
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC),
    )?)
}
pub(super) type Stamp = (u64, u64, u64, u32, u32, u64, i64, i64, i64, i64);
pub(super) fn stamp(m: &Metadata) -> Stamp {
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
fn file(
    source: &Dir,
    name: &str,
    path: &str,
    destination: Option<&Dir>,
    budget: &mut Budget,
    policy: ReadPolicy,
) -> Result<Entry> {
    budget.check()?;
    let before = source.symlink_metadata(name)?;
    ensure!(
        before.is_file()
            && before.nlink() == 1
            && before.len() <= budget.max_file
            && policy.allows(&before),
        "seed file must be bounded, safely owned, singly linked and regular"
    );
    ensure!(
        !matches!(policy, ReadPolicy::PublicCache) || before.mode() & 0o004 != 0,
        "public cache import requires world-readable regular files"
    );
    ensure!(
        budget.entries < budget.max_entries
            && budget
                .bytes
                .checked_add(before.len())
                .is_some_and(|n| n <= budget.max_total),
        "Cargo seed aggregate budget exceeded before copy"
    );
    let mut input = source.open_with(
        name,
        OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC),
    )?;
    ensure!(
        stamp(&before) == stamp(&input.metadata()?),
        "seed file changed before read"
    );
    let mut output = destination
        .map(|dir| {
            dir.open_with(
                name,
                OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC),
            )
        })
        .transpose()?;
    let mut hash = Sha256::new();
    let mut total = 0u64;
    let mut bytes = [0u8; 65536];
    loop {
        budget.check()?;
        let n = input.read(&mut bytes)?;
        if n == 0 {
            break;
        }
        total = total
            .checked_add(n as u64)
            .context("seed file length overflow")?;
        ensure!(
            total <= before.len() && total <= budget.max_file,
            "seed file grew while reading"
        );
        hash.update(&bytes[..n]);
        if let Some(output) = &mut output {
            output.write_all(&bytes[..n])?;
        }
    }
    ensure!(
        total == before.len()
            && stamp(&before) == stamp(&input.metadata()?)
            && stamp(&before) == stamp(&source.symlink_metadata(name)?),
        "seed file changed during read"
    );
    if let Some(output) = &mut output {
        output.flush()?;
    }
    let entry = Entry {
        path: path.into(),
        directory: false,
        bytes: total,
        sha256: Some(format!("{:x}", hash.finalize())),
        executable: before.mode() & 0o111 != 0,
    };
    budget.add(&entry)?;
    Ok(entry)
}

fn walk(
    source: &Dir,
    target: Option<&Dir>,
    prefix: &str,
    entries: &mut Vec<Entry>,
    budget: &mut Budget,
    policy: ReadPolicy,
) -> Result<()> {
    budget.check()?;
    let before = source.dir_metadata()?;
    let mut names = Vec::new();
    for entry in source.entries()? {
        let name = entry?
            .file_name()
            .into_string()
            .map_err(|_| anyhow::anyhow!("seed name must be UTF8"))?;
        ensure!(
            names.len() + entries.len() < budget.max_entries,
            "Cargo seed entry budget exceeded"
        );
        names.push(name);
    }
    names.sort();
    for name in names {
        let path = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        relative(&path)?;
        if path == "seed-manifest.json" {
            continue;
        }
        let m = source.symlink_metadata(&name)?;
        if m.is_dir() {
            let entry = Entry {
                path: path.clone(),
                directory: true,
                bytes: 0,
                sha256: None,
                executable: false,
            };
            budget.add(&entry)?;
            entries.push(entry);
            let child = open_dir_mode(source, &name, policy)?;
            let dest = if let Some(target) = target {
                mkdir(target, &name)?;
                Some(open_dir(target, &name)?)
            } else {
                None
            };
            walk(&child, dest.as_ref(), &path, entries, budget, policy)?;
            ensure!(
                stamp(&m) == stamp(&source.symlink_metadata(&name)?),
                "named source directory changed during copy"
            );
        } else {
            entries.push(file(source, &name, &path, target, budget, policy)?);
        }
    }
    ensure!(
        stamp(&before) == stamp(&source.dir_metadata()?),
        "seed directory changed during enumeration"
    );
    Ok(())
}
pub(crate) fn tree(
    source: &Dir,
    target: Option<&Dir>,
    prefix: &str,
    budget: &mut Budget,
) -> Result<Vec<Entry>> {
    let source = if prefix.is_empty() {
        source.try_clone()?
    } else {
        open_dir(source, prefix)?
    };
    let target = if let Some(root) = target {
        if prefix.is_empty() {
            Some(root.try_clone()?)
        } else {
            mkdir(root, prefix)?;
            Some(open_dir(root, prefix)?)
        }
    } else {
        None
    };
    let mut entries = Vec::new();
    if !prefix.is_empty() {
        let e = Entry {
            path: prefix.into(),
            directory: true,
            bytes: 0,
            sha256: None,
            executable: false,
        };
        budget.add(&e)?;
        entries.push(e);
    }
    walk(
        &source,
        target.as_ref(),
        prefix,
        &mut entries,
        budget,
        ReadPolicy::StrictSeed,
    )?;
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(entries)
}

pub(super) fn generated_tree(root: &Dir, prefix: &str, budget: &mut Budget) -> Result<Vec<Entry>> {
    let source = open_dir_mode(root, prefix, ReadPolicy::PrivateGenerated)?;
    let mut entries = Vec::new();
    walk(
        &source,
        None,
        prefix,
        &mut entries,
        budget,
        ReadPolicy::PrivateGenerated,
    )?;
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(entries)
}

/// Public source import is intentionally separate from verified seed/runtime
/// reads. Group writers can choose stable index bytes: only the observed copy,
/// never its registry authenticity, is asserted by the generated manifest.
pub(super) struct PublicCache {
    root: Dir,
    path: std::path::PathBuf,
    before: Stamp,
}
impl PublicCache {
    pub fn open(path: &Path) -> Result<Self> {
        ensure!(
            path.is_absolute()
                && path
                    .to_str()
                    .is_some_and(|s| s.len() <= 4096 && !s.bytes().any(|b| b.is_ascii_control()))
                && path.canonicalize()? == path,
            "public cache root must be explicit, bounded, absolute and canonical"
        );
        let components: Vec<_> = path
            .components()
            .filter(|c| !matches!(c, Component::RootDir))
            .collect();
        ensure!(
            components.len() <= 128 && components.iter().all(|c| matches!(c, Component::Normal(_))),
            "unsafe public cache root path"
        );
        let mut root = Dir::open_ambient_dir("/", cap_std::ambient_authority())?;
        for (i, component) in components.iter().enumerate() {
            // Existing root-owned sticky ancestors protect an owned child from
            // other-UID rename; final public cache root never gets this exception.
            root = child(
                &root,
                component.as_os_str(),
                ReadPolicy::PublicCache,
                i + 1 < components.len(),
            )?;
        }
        ensure!(
            ReadPolicy::PublicCache.allows(&root.dir_metadata()?),
            "unsafe public cache root mode"
        );
        let before = stamp(&root.dir_metadata()?);
        let result = Self {
            root,
            path: path.into(),
            before,
        };
        result.verify()?;
        Ok(result)
    }
    pub fn verify(&self) -> Result<()> {
        verify_root(&self.path, &self.root)?;
        ensure!(
            stamp(&self.root.dir_metadata()?) == self.before,
            "public cache root changed during import"
        );
        Ok(())
    }
    pub fn copy_file(&self, path: &str, target: &Dir, budget: &mut Budget) -> Result<Entry> {
        relative(path)?;
        let p = Path::new(path);
        let parent = p.parent().unwrap().to_str().unwrap();
        let source = open_dir_mode(&self.root, parent, ReadPolicy::PublicCache)?;
        let before = stamp(&source.dir_metadata()?);
        mkdir(target, parent)?;
        let dest = open_dir(target, parent)?;
        let copied = file(
            &source,
            p.file_name()
                .unwrap()
                .to_str()
                .context("cache name not UTF8")?,
            path,
            Some(&dest),
            budget,
            ReadPolicy::PublicCache,
        )?;
        let named = open_dir_mode(&self.root, parent, ReadPolicy::PublicCache)?;
        ensure!(
            stamp(&source.dir_metadata()?) == before && stamp(&named.dir_metadata()?) == before,
            "public cache parent changed during copy"
        );
        Ok(copied)
    }
}
