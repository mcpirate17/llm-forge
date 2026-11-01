//! Private durable state outside all child/source capabilities.
use anyhow::{ensure, Context, Result};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

pub struct State {
    pub root: PathBuf,
    _lock: File,
}

impl State {
    pub fn create(parent: &Path) -> Result<PathBuf> {
        fs::create_dir_all(parent)?;
        let root = parent.canonicalize()?.join(crate::native_id::new("run")?);
        fs::DirBuilder::new().mode(0o700).create(&root)?;
        Ok(root)
    }

    pub fn open(root: &Path) -> Result<Self> {
        let root = owned_directory(root)?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(root.join("owner.lock"))?;
        ensure!(
            lock.metadata()?.is_file() && lock.metadata()?.nlink() == 1,
            "invalid state lock"
        );
        ensure!(
            unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0,
            "another harness owns this state directory"
        );
        Ok(Self { root, _lock: lock })
    }

    pub fn directory(&self, id: &str) -> Result<PathBuf> {
        crate::task_store::validate_id(id)?;
        let path = self.root.join(id);
        fs::DirBuilder::new().mode(0o700).create(&path)?;
        Ok(path)
    }
}

pub fn owned_directory(root: &Path) -> Result<PathBuf> {
    ensure!(root.is_absolute(), "state path must be absolute");
    let meta = fs::symlink_metadata(root)?;
    ensure!(
        meta.is_dir() && meta.uid() == unsafe { libc::geteuid() } && meta.mode() & 0o777 == 0o700,
        "state must be an owned mode-0700 directory, not a symlink"
    );
    Ok(root.canonicalize()?)
}

pub use crate::bounded_file::read;

pub fn write_new(path: &Path, bytes: &[u8]) -> Result<String> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    File::open(path.parent().context("missing parent")?)?.sync_all()?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

pub fn log(path: &Path) -> Result<File> {
    Ok(OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?)
}

pub fn now() -> Result<i64> {
    Ok(std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs()
        .try_into()?)
}
