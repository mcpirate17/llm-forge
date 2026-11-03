//! Real Git index.lock ownership for a shared checkout transaction.
//!
//! This primitive protects against Git writers that honor index.lock. It is not
//! a complete checkout transaction: caller owns ref locks, worktree publication,
//! durable recovery journal, cancellation and the cooperative writer lease.
use crate::checkout_sync_journal::Identity;
use anyhow::{ensure, Context, Result};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};
const LIMIT: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IndexImage {
    bytes: Vec<u8>,
    device: u64,
    inode: u64,
    mode: u32,
    mtime: (i64, i64),
    ctime: (i64, i64),
}

impl IndexImage {
    pub(crate) fn journal_identity(&self) -> Identity {
        Identity {
            device: self.device,
            inode: self.inode,
            mode: self.mode,
            length: self.bytes.len() as u64,
            sha256: format!("{:x}", Sha256::digest(&self.bytes)),
        }
    }
}

/// A held Git index lock. Parent directories must already be trusted/pinned by
/// the caller; do not give this an attacker-controlled index pathname.
pub(crate) struct IndexLock {
    index: PathBuf,
    lock: PathBuf,
    file: File,
    identity: (u64, u64),
    original: Option<IndexImage>,
    armed: bool,
    attempted: bool,
    staged: Option<IndexImage>,
}

fn image(path: &Path) -> Result<Option<IndexImage>> {
    let mut file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
    {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).context("open Git index"),
    };
    let before = file.metadata()?;
    ensure!(
        before.is_file() && before.nlink() == 1 && before.len() <= LIMIT,
        "Git index is not a bounded singly-linked regular file"
    );
    let mut bytes = Vec::new();
    (&mut file).take(LIMIT + 1).read_to_end(&mut bytes)?;
    let after = file.metadata()?;
    ensure!(
        bytes.len() as u64 <= LIMIT && identity(&before) == identity(&after),
        "Git index changed while reading"
    );
    let named = fs::symlink_metadata(path)?;
    ensure!(
        identity(&named) == identity(&after),
        "Git index path changed while reading"
    );
    Ok(Some(IndexImage {
        bytes,
        device: after.dev(),
        inode: after.ino(),
        mode: after.mode(),
        mtime: (after.mtime(), after.mtime_nsec()),
        ctime: (after.ctime(), after.ctime_nsec()),
    }))
}
fn identity(m: &fs::Metadata) -> (u64, u64, u64, u32, u64, i64, i64, i64, i64) {
    (
        m.dev(),
        m.ino(),
        m.nlink(),
        m.mode(),
        m.len(),
        m.mtime(),
        m.mtime_nsec(),
        m.ctime(),
        m.ctime_nsec(),
    )
}

impl IndexLock {
    /// Acquire before classifying and retain it through final reference decision.
    pub(crate) fn acquire(index: &Path) -> Result<Self> {
        let parent = index.parent().context("index needs a parent")?;
        let name = index.file_name().context("index needs a filename")?;
        let mut lock_name = name.to_os_string();
        lock_name.push(".lock");
        let lock = parent.join(lock_name);
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&lock)
            .context("acquire Git index.lock")?;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
        let m = file.metadata()?;
        let mut held = Self {
            index: index.to_owned(),
            lock,
            file,
            identity: (m.dev(), m.ino()),
            original: None,
            armed: true,
            attempted: false,
            staged: None,
        };
        held.verify_lock()?;
        held.original = image(index)?;
        Ok(held)
    }
    /// Conditionally restore while this exact lock remains held. Publication
    /// consumes the lock name, so the caller records release intent first.
    pub(crate) fn restore_guard(
        mut self,
        original: Option<&Identity>,
        staged: &Identity,
        original_bytes: Option<&[u8]>,
    ) -> Result<()> {
        let actual = self.original().map(IndexImage::journal_identity);
        if actual.as_ref() == original {
            self.revalidate()?;
            return Ok(());
        }
        ensure!(
            actual.as_ref() == Some(staged),
            "index differs from recorded original and publication"
        );
        match (original, original_bytes) {
            (Some(identity), Some(bytes)) => {
                ensure!(
                    format!("{:x}", Sha256::digest(bytes)) == identity.sha256
                        && bytes.len() as u64 == identity.length,
                    "index recovery preimage mismatch"
                );
                self.prepare_mode(bytes, Some(identity.mode))?;
                self.publish_prepared()?;
            }
            (None, None) => {
                self.revalidate()?;
                fs::remove_file(&self.index)?;
                File::open(self.index.parent().context("index parent missing")?)?.sync_all()?;
                self.revalidate_absent_after_delete()?;
            }
            _ => anyhow::bail!("index recovery preimage missing or unexpected"),
        }
        Ok(())
    }
    pub(crate) fn original(&self) -> Option<&IndexImage> {
        self.original.as_ref()
    }
    pub(crate) fn original_bytes(&self) -> Option<&[u8]> {
        self.original.as_ref().map(|image| image.bytes.as_slice())
    }
    fn verify_lock(&self) -> Result<()> {
        let held = self.file.metadata()?;
        let named = fs::symlink_metadata(&self.lock)?;
        ensure!(
            held.is_file()
                && held.nlink() == 1
                && (held.dev(), held.ino()) == self.identity
                && (named.dev(), named.ino()) == self.identity
                && named.is_file(),
            "Git index.lock replaced"
        );
        Ok(())
    }
    pub(crate) fn revalidate(&self) -> Result<()> {
        self.verify_lock()?;
        ensure!(
            image(&self.index)? == self.original,
            "Git index changed while locked"
        );
        Ok(())
    }
    /// Write and sync the replacement lock inode; caller must journal its
    /// identity durably before calling publish_prepared.
    pub(crate) fn prepare(&mut self, bytes: &[u8]) -> Result<Identity> {
        self.prepare_mode(bytes, None)
    }
    fn prepare_mode(&mut self, bytes: &[u8], mode: Option<u32>) -> Result<Identity> {
        ensure!(
            bytes.len() as u64 <= LIMIT,
            "prepared Git index exceeds limit"
        );
        ensure!(!self.attempted, "index publication already attempted");
        self.revalidate()?;
        self.attempted = true;
        if let Some(mode) = mode.or_else(|| self.original.as_ref().map(|i| i.mode)) {
            self.file
                .set_permissions(fs::Permissions::from_mode(mode & 0o7777))?;
        }
        self.file.write_all(bytes)?;
        self.file.sync_all()?;
        self.revalidate()?;
        let staged = image(&self.lock)?.context("prepared index lock vanished")?;
        ensure!(
            staged.bytes == bytes && (staged.device, staged.inode) == self.identity,
            "prepared index lock changed"
        );
        let identity = staged.journal_identity();
        self.staged = Some(staged);
        Ok(identity)
    }
    pub(crate) fn publish_prepared(&mut self) -> Result<IndexImage> {
        self.revalidate()?;
        let staged = self.staged.as_ref().context("index was not prepared")?;
        ensure!(
            image(&self.lock)?.as_ref() == Some(staged),
            "prepared index changed"
        );
        let bytes = staged.bytes.clone();
        fs::rename(&self.lock, &self.index).context("publish Git index")?;
        self.armed = false;
        File::open(self.index.parent().context("index parent missing")?)?
            .sync_all()
            .context("sync Git index directory after publication")?;
        let published = image(&self.index)?.context("published Git index absent")?;
        ensure!(
            published.bytes == bytes && (published.device, published.inode) == self.identity,
            "published Git index differs from prepared inode or bytes"
        );
        Ok(published)
    }
}
impl Drop for IndexLock {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        if fs::symlink_metadata(&self.lock)
            .ok()
            .is_some_and(|m| (m.dev(), m.ino()) == self.identity)
        {
            let _ = fs::remove_file(&self.lock);
        }
    }
}

impl IndexLock {
    fn revalidate_absent_after_delete(&mut self) -> Result<()> {
        self.verify_lock()?;
        ensure!(
            image(&self.index)?.is_none(),
            "Git index appeared during rollback"
        );
        Ok(())
    }
}

/// Inspect a bounded index or index.lock without following a final symlink.
pub(crate) fn journal_identity(path: &Path) -> Result<Option<Identity>> {
    Ok(image(path)?.map(|v| v.journal_identity()))
}
/// Remove only the exact lock inode recorded by a dead cooperative actor. The
/// caller must own the recovery lease and journal rollback intent first.
pub(crate) fn remove_owned_stale_lock(index: &Path, staged: &Identity) -> Result<()> {
    let mut name = index
        .file_name()
        .context("index filename missing")?
        .to_os_string();
    name.push(".lock");
    let parent = index.parent().context("index parent missing")?;
    let path = parent.join(name);
    match image(&path)? {
        None => Ok(()),
        Some(observed) => {
            ensure!(
                observed.journal_identity() == *staged,
                "index.lock belongs to another writer"
            );
            fs::remove_file(&path)?;
            File::open(parent)?.sync_all()?;
            Ok(())
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::checkout_sync_guards::RecoveryIndex;

    fn publish(lock: &mut IndexLock, bytes: &[u8]) -> Result<IndexImage> {
        lock.prepare(bytes)?;
        lock.publish_prepared()
    }

    #[test]
    fn lock_preserves_changed_index_and_does_not_remove_a_foreign_lock() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("index");
        fs::write(&path, b"old").unwrap();
        let mut held = IndexLock::acquire(&path).unwrap();
        fs::write(&path, b"outside").unwrap();
        assert!(publish(&mut held, b"new").is_err());
        assert_eq!(fs::read(&path).unwrap(), b"outside");
        drop(held);
        let held = IndexLock::acquire(&path).unwrap();
        fs::remove_file(dir.path().join("index.lock")).unwrap();
        fs::write(dir.path().join("index.lock"), b"foreign").unwrap();
        drop(held);
        assert_eq!(fs::read(dir.path().join("index.lock")).unwrap(), b"foreign");
    }
    #[test]
    fn rollback_refuses_modified_or_replaced_published_index() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("index");
        fs::write(&path, b"old").unwrap();
        let mut lock = IndexLock::acquire(&path).unwrap();
        let original = lock.original().map(IndexImage::journal_identity);
        let staged = publish(&mut lock, b"new").unwrap().journal_identity();
        drop(lock);
        fs::write(&path, b"external").unwrap();
        assert!(RecoveryIndex::acquire_for_restore(&path, original.as_ref(), &staged).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"external");

        // Identical bytes in a foreign inode are not the actor's staged image.
        let replacement = dir.path().join("foreign");
        fs::write(&replacement, b"new").unwrap();
        fs::rename(&replacement, &path).unwrap();
        assert!(RecoveryIndex::acquire_for_restore(&path, original.as_ref(), &staged).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"new");
    }
    #[test]
    fn recovery_guard_restores_original_and_absent_state() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("index");
        fs::write(&path, b"old").unwrap();
        let mut lock = IndexLock::acquire(&path).unwrap();
        let original = lock.original().map(IndexImage::journal_identity);
        let published = publish(&mut lock, b"new").unwrap().journal_identity();
        drop(lock);
        let guard =
            RecoveryIndex::acquire_for_restore(&path, original.as_ref(), &published).unwrap();
        assert!(guard
            .restore(original.as_ref(), &published, Some(b"bad"))
            .is_err());
        assert_eq!(fs::read(&path).unwrap(), b"new");
        let guard =
            RecoveryIndex::acquire_for_restore(&path, original.as_ref(), &published).unwrap();
        guard
            .restore(original.as_ref(), &published, Some(b"old"))
            .unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"old");
        fs::remove_file(&path).unwrap();
        let mut lock = IndexLock::acquire(&path).unwrap();
        let published = publish(&mut lock, b"new").unwrap().journal_identity();
        drop(lock);
        let guard = RecoveryIndex::acquire_for_restore(&path, None, &published).unwrap();
        guard.restore(None, &published, None).unwrap();
        assert!(!path.exists());
    }
    #[test]
    fn prepared_index_identity_supports_conditional_restart_rollback() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("index");
        fs::write(&path, b"original").unwrap();
        let mut held = IndexLock::acquire(&path).unwrap();
        let original = held.original().map(IndexImage::journal_identity);
        let staged = held.prepare(b"published").unwrap();
        assert_eq!(journal_identity(&path).unwrap(), original);
        let published = held.publish_prepared().unwrap();
        assert_eq!(published.journal_identity(), staged);
        drop(held);
        let guard = RecoveryIndex::acquire_for_restore(&path, original.as_ref(), &staged).unwrap();
        guard
            .restore(original.as_ref(), &staged, Some(b"original"))
            .unwrap();
        assert_eq!(fs::read(path).unwrap(), b"original");
    }
    #[test]
    fn foreign_stale_lock_is_not_removed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("index");
        fs::write(&path, b"old").unwrap();
        let mut held = IndexLock::acquire(&path).unwrap();
        let staged = held.prepare(b"planned").unwrap();
        let lock = dir.path().join("index.lock");
        fs::remove_file(&lock).unwrap();
        fs::write(&lock, b"foreign").unwrap();
        assert!(remove_owned_stale_lock(&path, &staged).is_err());
        drop(held);
        assert_eq!(fs::read(lock).unwrap(), b"foreign");
    }
}
