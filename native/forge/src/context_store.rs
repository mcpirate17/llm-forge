//! Bounded, immutable recovery artifacts in an operator-owned private directory.
use anyhow::{ensure, Context, Result};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

pub const MAX_ARTIFACT_BYTES: usize = 8 * 1024 * 1024;
pub const DEFAULT_QUOTA: u64 = 256 * 1024 * 1024;

pub struct Store {
    root: PathBuf,
    quota: u64,
}

impl Store {
    pub fn create(root: &Path, quota: u64) -> Result<Self> {
        ensure!(root.is_absolute(), "context store must be absolute");
        match fs::DirBuilder::new().mode(0o700).create(root) {
            Ok(()) => (),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(e) => return Err(e.into()),
        }
        File::open(root.parent().context("context store has no parent")?)?.sync_all()?;
        Self::open(root, quota)
    }
    pub fn open(root: &Path, quota: u64) -> Result<Self> {
        ensure!(
            (1..=4 * 1024 * 1024 * 1024).contains(&quota),
            "invalid context store quota"
        );
        Ok(Self {
            root: crate::harness::state::owned_directory(root)?,
            quota,
        })
    }
    pub fn put(&self, bytes: &[u8]) -> Result<String> {
        ensure!(
            bytes.len() <= MAX_ARTIFACT_BYTES,
            "recovery artifact exceeds 8 MiB"
        );
        let hash = format!("{:x}", Sha256::digest(bytes));
        let _guard = self.lock()?;
        let path = self.path(&hash)?;
        match fs::symlink_metadata(&path) {
            Ok(_) => {
                ensure!(self.get(&hash)? == bytes, "recovery artifact conflict");
                OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_NOFOLLOW)
                    .open(&path)?
                    .sync_all()?;
                File::open(&self.root)?.sync_all()?;
                return Ok(hash);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => return Err(e.into()),
        }
        let (size, count) = self.usage()?;
        ensure!(
            count < 4096 && size + bytes.len() as u64 <= self.quota,
            "context recovery quota exhausted"
        );
        let temporary = self.root.join(crate::native_id::new("pending")?);
        // Only clean up a staging file after create_new proves this writer owns it.
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        let result = self.publish(&mut file, &temporary, &path, bytes);
        if result.is_err() {
            if let Err(error) = fs::remove_file(&temporary) {
                if error.kind() != std::io::ErrorKind::NotFound {
                    eprintln!("forge: recovery staging cleanup: {error}");
                }
            }
        }
        result?;
        Ok(hash)
    }
    pub fn get(&self, hash: &str) -> Result<Vec<u8>> {
        let path = self.path(hash)?;
        let meta = fs::symlink_metadata(&path)?;
        ensure!(
            meta.is_file()
                && meta.uid() == unsafe { libc::geteuid() }
                && meta.nlink() == 1
                && meta.mode() & 0o777 == 0o400,
            "recovery artifact must be owned, sealed and singly linked"
        );
        let bytes = crate::harness::state::read(&path, MAX_ARTIFACT_BYTES)?;
        ensure!(
            format!("{:x}", Sha256::digest(&bytes)) == hash,
            "recovery artifact checksum mismatch"
        );
        Ok(bytes)
    }
    fn path(&self, hash: &str) -> Result<PathBuf> {
        conductor_native::sha256_hex::parse(hash).map_err(anyhow::Error::msg)?;
        Ok(self.root.join(format!("{hash}.json")))
    }
    fn lock(&self) -> Result<File> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(self.root.join("writer.lock"))?;
        let meta = file.metadata()?;
        ensure!(
            meta.is_file()
                && meta.uid() == unsafe { libc::geteuid() }
                && meta.nlink() == 1
                && meta.mode() & 0o777 == 0o600,
            "invalid context recovery lock"
        );
        ensure!(
            unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0,
            "context recovery writer is busy; retry"
        );
        Ok(file)
    }
    fn usage(&self) -> Result<(u64, usize)> {
        let mut size = 0u64;
        let mut count = 0;
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            if entry.file_name() == "writer.lock" {
                continue;
            }
            let meta = fs::symlink_metadata(entry.path())?;
            ensure!(meta.is_file(), "unexpected non-file in context store");
            size = size
                .checked_add(meta.len())
                .context("context store size overflow")?;
            count += 1;
            ensure!(count <= 4096, "context store entry limit exceeded");
        }
        Ok((size, count))
    }
    fn publish(&self, file: &mut File, temporary: &Path, path: &Path, bytes: &[u8]) -> Result<()> {
        file.write_all(bytes)?;
        file.set_permissions(fs::Permissions::from_mode(0o400))?;
        file.sync_all()?;
        // The exclusive writer lock and immutable content-addressed name serialize
        // cooperating publishers. Same-UID unconfined writers remain trusted.
        fs::rename(temporary, path)?;
        File::open(&self.root)?
            .sync_all()
            .context("recovery artifact published, directory sync failed")?;
        Ok(())
    }
}
