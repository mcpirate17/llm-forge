//! Fixed-path, bounded graph projection cache storage.
//! Directory capabilities constrain accesses; no custom SQLite/filesystem VFS
//! or protection against an unconfined same-UID writer after the last check is
//! claimed. Configured-host ancestors and the mount namespace are trusted.
//! Lock admission uses 51 attempts with at most 250ms total retry sleep, not
//! an I/O deadline. File operations and fsync have no operation-wide deadline.
//! Once renamed, failures report that
//! publication is visible and verification/pruning/durability may be incomplete.
use anyhow::{ensure, Context, Result};
use cap_std::fs::{
    Dir, DirBuilder, DirBuilderExt, File, Metadata, MetadataExt, OpenOptions, OpenOptionsExt,
};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

const MAX_BYTES: usize = 128 * 1024;
const MAX_SCAN: usize = 4096;
const MAX_RETAINED: usize = 128;
const LOCK: &str = ".storage.lock";
struct Storage {
    host: PathBuf,
    root: Dir,
    root_identity: Metadata,
    forge: Dir,
    directory: Dir,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Boundary {
    BeforeRead,
    Read,
    Staged,
    BeforeRename,
    BeforePrune,
    Pruned,
    Renamed,
}
struct Candidate {
    name: String,
    metadata: Metadata,
}

fn key(key: &str) -> Result<String> {
    ensure!(
        key.len() == 64
            && key
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "graph cache key must be exactly 64 lowercase hexadecimal characters"
    );
    Ok(format!("{key}.json"))
}
fn directory_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    options
}
fn file_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    options
}
fn absolute_directory(path: &Path) -> Result<Dir> {
    ensure!(path.is_absolute(), "graph cache host must be absolute");
    let mut directory = Dir::open_ambient_dir("/", cap_std::ambient_authority())?;
    for component in path.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(name) => {
                directory =
                    Dir::from_std_file(directory.open_with(name, &directory_options())?.into_std())
            }
            _ => anyhow::bail!("unsafe graph cache host path"),
        }
    }
    Ok(directory)
}
fn child(parent: &Dir, name: &str, create: bool) -> Result<Option<Dir>> {
    if create {
        let mut builder = DirBuilder::new();
        builder.mode(0o700);
        match parent.create_dir_with(name, &builder) {
            Ok(()) => parent.open(".")?.sync_all()?,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error).context("create private graph cache directory"),
        }
    }
    match parent.open_with(name, &directory_options()) {
        Ok(file) => Ok(Some(Dir::from_std_file(file.into_std()))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).context("open graph cache directory without symlinks"),
    }
}
fn same_inode(a: &Metadata, b: &Metadata) -> bool {
    a.dev() == b.dev() && a.ino() == b.ino()
}
fn stable(a: &Metadata, b: &Metadata) -> bool {
    same_inode(a, b)
        && a.len() == b.len()
        && a.mtime() == b.mtime()
        && a.mtime_nsec() == b.mtime_nsec()
        && a.ctime() == b.ctime()
        && a.ctime_nsec() == b.ctime_nsec()
}
fn regular(metadata: &Metadata) -> Result<()> {
    ensure!(
        metadata.is_file() && metadata.nlink() == 1 && metadata.uid() == unsafe { libc::geteuid() },
        "unsafe graph cache file: expected owned single-link regular inode"
    );
    Ok(())
}
impl Storage {
    fn open(host: &Path, create: bool) -> Result<Option<Self>> {
        ensure!(
            host.as_os_str().len() <= 4096,
            "graph cache host exceeds path limit"
        );
        let host = host.canonicalize().context("resolve graph cache host")?;
        ensure!(
            host.as_os_str().len() <= 4096,
            "graph cache host exceeds path limit"
        );
        let root = absolute_directory(&host)?;
        let root_identity = root.dir_metadata()?;
        let Some(forge) = child(&root, ".forge", create)? else {
            return Ok(None);
        };
        let Some(directory) = child(&forge, "context-cache", create)? else {
            return Ok(None);
        };
        let storage = Self {
            host,
            root,
            root_identity,
            forge,
            directory,
        };
        storage.check()?;
        Ok(Some(storage))
    }
    fn path(&self) -> PathBuf {
        self.host.join(".forge/context-cache")
    }
    fn check(&self) -> Result<()> {
        let current = absolute_directory(&self.host).context("graph cache host changed")?;
        ensure!(
            same_inode(&self.root_identity, &current.dir_metadata()?),
            "graph cache host changed"
        );
        let forge =
            child(&self.root, ".forge", false)?.context("graph cache parent disappeared")?;
        ensure!(
            same_inode(&self.forge.dir_metadata()?, &forge.dir_metadata()?),
            "graph cache parent changed"
        );
        let cache =
            child(&forge, "context-cache", false)?.context("graph cache directory disappeared")?;
        ensure!(
            same_inode(&self.directory.dir_metadata()?, &cache.dir_metadata()?),
            "graph cache directory changed"
        );
        Ok(())
    }
    fn target(&self, name: &str) -> Result<Option<Metadata>> {
        match self.directory.symlink_metadata(name) {
            Ok(metadata) => {
                regular(&metadata)?;
                Ok(Some(metadata))
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error).context("inspect graph cache target"),
        }
    }
    fn lock(&self) -> Result<crate::file_lock::Exclusive> {
        for attempt in 0..51 {
            let file = self.directory.open_with(
                LOCK,
                OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create(true)
                    .truncate(false)
                    .mode(0o600)
                    .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC),
            )?;
            let metadata = file.metadata()?;
            regular(&metadata)?;
            ensure!(
                metadata.mode() & 0o077 == 0,
                "graph cache lock must be private"
            );
            match crate::file_lock::Exclusive::try_new(file.into_std(), "graph cache publication") {
                Ok(lease) => {
                    let current = self.directory.symlink_metadata(LOCK)?;
                    ensure!(stable(&metadata, &current), "graph cache lock path changed");
                    self.check()?;
                    return Ok(lease);
                }
                Err(error) if attempt == 50 => {
                    return Err(error).context("bounded graph cache lock admission")
                }
                Err(_) => std::thread::sleep(Duration::from_millis(5)),
            }
        }
        unreachable!("bounded lock loop always returns")
    }
    fn scan(&self, target: &str, maximum: usize) -> Result<Vec<Candidate>> {
        let mut candidates = Vec::new();
        for (index, entry) in self.directory.entries()?.enumerate() {
            ensure!(
                index < maximum,
                "graph cache directory entry scan limit exceeded"
            );
            let entry = entry?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            let Some(stem) = name.strip_suffix(".json") else {
                continue;
            };
            if name == target || key(stem).is_err() {
                continue;
            }
            let metadata = self.directory.symlink_metadata(name)?;
            // Never follow or prune foreign, linked, symlink or nonregular entries.
            if regular(&metadata).is_ok() {
                candidates.push(Candidate {
                    name: name.to_owned(),
                    metadata,
                });
            }
        }
        candidates.sort_by(|a, b| {
            (a.metadata.mtime(), a.metadata.mtime_nsec(), &a.name).cmp(&(
                b.metadata.mtime(),
                b.metadata.mtime_nsec(),
                &b.name,
            ))
        });
        Ok(candidates)
    }
    fn prune(&self, candidates: Vec<Candidate>, maximum: usize) -> Result<()> {
        let remove = candidates.len().saturating_sub(maximum - 1);
        for candidate in candidates.into_iter().take(remove) {
            let current = self.directory.symlink_metadata(&candidate.name)?;
            regular(&current)?;
            ensure!(
                stable(&candidate.metadata, &current),
                "graph cache prune candidate changed"
            );
            self.directory
                .remove_file(&candidate.name)
                .context("prune owned graph cache entry")?;
        }
        Ok(())
    }
}

/// Missing cache reads never create directories or lock files.
pub fn read(host: &Path, key_value: &str, maximum: usize) -> Result<Option<Vec<u8>>> {
    read_checked(host, key_value, maximum, |_, _| Ok(()))
}
fn read_checked(
    host: &Path,
    key_value: &str,
    maximum: usize,
    mut boundary: impl FnMut(Boundary, &Path) -> Result<()>,
) -> Result<Option<Vec<u8>>> {
    let name = key(key_value)?;
    ensure!(
        (1..=MAX_BYTES).contains(&maximum),
        "invalid graph cache read limit"
    );
    let Some(storage) = Storage::open(host, false)? else {
        return Ok(None);
    };
    storage.check()?;
    let Some(before) = storage.target(&name)? else {
        return Ok(None);
    };
    ensure!(
        before.len() <= maximum as u64,
        "graph cache exceeds read byte limit"
    );
    let mut file = storage.directory.open_with(&name, &file_options())?;
    ensure!(
        stable(&before, &file.metadata()?),
        "graph cache path changed before read"
    );
    boundary(Boundary::BeforeRead, &storage.path())?;
    let mut bytes = Vec::with_capacity(before.len() as usize);
    (&mut file)
        .take(maximum as u64 + 1)
        .read_to_end(&mut bytes)?;
    boundary(Boundary::Read, &storage.path())?;
    ensure!(
        bytes.len() <= maximum,
        "graph cache exceeds read byte limit"
    );
    let current = storage
        .target(&name)?
        .context("graph cache disappeared during read")?;
    ensure!(
        stable(&before, &current)
            && stable(&before, &file.metadata()?)
            && bytes.len() as u64 == before.len(),
        "graph cache changed during read"
    );
    storage.check()?;
    Ok(Some(bytes))
}
fn stage(storage: &Storage, bytes: &[u8]) -> Result<(String, File, Metadata)> {
    let name = format!(".{}.tmp", crate::resource_id("graph-context-cache")?);
    let mut file = storage.directory.open_with(
        &name,
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC),
    )?;
    match file
        .write_all(bytes)
        .and_then(|()| file.sync_all())
        .and_then(|()| file.metadata())
    {
        Ok(metadata) => Ok((name, file, metadata)),
        Err(error) => match remove_stage(storage, &name, &file) {
            Ok(()) => Err(error.into()),
            Err(cleanup) => Err(anyhow::anyhow!(
                "graph cache stage write failed: {error}; cleanup failed: {cleanup:#}"
            )),
        },
    }
}
fn verify_stage(storage: &Storage, name: &str, file: &File, original: &Metadata) -> Result<()> {
    let held = file.metadata()?;
    let current = storage.directory.symlink_metadata(name)?;
    regular(&held)?;
    regular(&current)?;
    ensure!(
        held.mode() & 0o077 == 0 && stable(original, &held) && stable(original, &current),
        "owned graph cache stage path changed"
    );
    Ok(())
}
fn remove_stage(storage: &Storage, name: &str, file: &File) -> Result<()> {
    let held = file.metadata()?;
    let current = storage.directory.symlink_metadata(name)?;
    regular(&held)?;
    regular(&current)?;
    ensure!(
        held.mode() & 0o077 == 0 && same_inode(&held, &current),
        "owned graph cache stage path changed"
    );
    storage.directory.remove_file(name)?;
    Ok(())
}
/// Atomically replace one validated cache key; success retains at most 128 managed entries.
/// Unrelated JSON, symlinks and special files are never pruning targets.
pub fn publish(host: &Path, key_value: &str, bytes: &[u8], max_entries: usize) -> Result<()> {
    publish_checked(host, key_value, bytes, max_entries, MAX_SCAN, |_, _| Ok(()))
}
fn publish_checked(
    host: &Path,
    key_value: &str,
    bytes: &[u8],
    max_entries: usize,
    scan_limit: usize,
    mut boundary: impl FnMut(Boundary, &Path) -> Result<()>,
) -> Result<()> {
    let name = key(key_value)?;
    ensure!(
        bytes.len() <= MAX_BYTES,
        "graph cache exceeds write byte limit"
    );
    ensure!(
        (1..=MAX_RETAINED).contains(&max_entries),
        "graph cache retention must be 1..128"
    );
    ensure!(
        (1..=MAX_SCAN).contains(&scan_limit),
        "invalid graph cache scan limit"
    );
    let storage = Storage::open(host, true)?.context("graph cache parent unavailable")?;
    let _lease = storage.lock()?;
    storage.target(&name)?;
    let candidates = storage.scan(&name, scan_limit)?;
    let (stage_name, file, original) = stage(&storage, bytes)?;
    let mut renamed = false;
    let result = (|| -> Result<()> {
        boundary(Boundary::Staged, &storage.path())?;
        storage.check()?;
        storage.target(&name)?;
        verify_stage(&storage, &stage_name, &file, &original)?;
        boundary(Boundary::BeforeRename, &storage.path())?;
        storage
            .directory
            .rename(&stage_name, &storage.directory, &name)?;
        renamed = true;
        boundary(Boundary::Renamed, &storage.path())?;
        let current = storage
            .target(&name)?
            .context("published graph cache disappeared")?;
        ensure!(
            same_inode(&file.metadata()?, &current),
            "published graph cache path changed"
        );
        storage.check()?;
        storage
            .directory
            .open(".")?
            .sync_all()
            .context("first published-directory sync incomplete")?;
        boundary(Boundary::BeforePrune, &storage.path())?;
        storage.check()?;
        storage.prune(candidates, max_entries).context(
            "pruning incomplete; some prior managed entries may already have been removed",
        )?;
        boundary(Boundary::Pruned, &storage.path())?;
        storage.check()?;
        storage
            .directory
            .open(".")?
            .sync_all()
            .context("post-prune directory sync incomplete")?;
        Ok(())
    })();
    let cleanup = if renamed {
        Ok(())
    } else {
        remove_stage(&storage, &stage_name, &file)
    };
    match (result, cleanup) {
        (Err(error), Err(cleanup)) => Err(anyhow::anyhow!(
            "graph cache publication failed: {error:#}; stage cleanup failed: {cleanup:#}"
        )),
        (Err(error), Ok(())) if renamed => {
            Err(error).context("graph cache published but cleanup/prune/sync incomplete")
        }
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Err(cleanup)) => Err(cleanup).context("graph cache stage cleanup failed"),
        (Ok(()), Ok(())) => Ok(()),
    }
}

#[cfg(test)]
#[path = "graph_context_cache_storage_tests.rs"]
mod tests;
