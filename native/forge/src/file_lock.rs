//! Nonblocking exclusive flock ownership. Explicit unlock matters even with
//! CLOEXEC: a duplicate or a concurrent fork can keep the open-file description
//! alive after the owner closes its descriptor.
use anyhow::{ensure, Context, Result};
use std::fs::{File, OpenOptions};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;

pub struct Lease {
    file: File,
    purpose: &'static str,
}

pub type Exclusive = Lease;

impl Lease {
    pub fn try_new(file: File, purpose: &'static str) -> Result<Self> {
        ensure!(
            unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0,
            "{purpose} is busy or cannot be locked: {}; retry",
            std::io::Error::last_os_error()
        );
        Ok(Self { file, purpose })
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        if unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) } != 0 {
            eprintln!(
                "forge: unlock {}: {}",
                self.purpose,
                std::io::Error::last_os_error()
            );
        }
    }
}

/// Open an owned private lock inode and acquire a shared or exclusive lease.
pub fn try_owned(path: &Path, exclusive: bool) -> Result<Option<Lease>> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)?;
    let meta = file.metadata()?;
    ensure!(
        meta.is_file()
            && meta.nlink() == 1
            && meta.uid() == unsafe { libc::geteuid() }
            && meta.mode() & 0o077 == 0,
        "invalid private lock inode"
    );
    let operation = libc::LOCK_NB
        | if exclusive {
            libc::LOCK_EX
        } else {
            libc::LOCK_SH
        };
    if unsafe { libc::flock(file.as_raw_fd(), operation) } != 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::WouldBlock {
            return Ok(None);
        }
        return Err(error.into());
    }
    let lease = Lease {
        file,
        purpose: "private state",
    };
    verify_identity(&lease.file, path)?;
    Ok(Some(lease))
}

fn verify_identity(file: &File, path: &Path) -> Result<()> {
    let held = file.metadata()?;
    let current = std::fs::symlink_metadata(path)?;
    ensure!(
        current.is_file()
            && current.nlink() == 1
            && held.nlink() == 1
            && current.uid() == unsafe { libc::geteuid() }
            && current.mode() & 0o077 == 0
            && (current.dev(), current.ino()) == (held.dev(), held.ino()),
        "private lock path changed while acquiring lease"
    );
    Ok(())
}

pub fn owned(path: &Path, exclusive: bool) -> Result<Lease> {
    try_owned(path, exclusive)?.context("state publication, builder, or generation is busy; retry")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::OpenOptions;
    use std::os::unix::fs::OpenOptionsExt;

    #[test]
    fn dropping_owner_releases_lock_even_while_a_duplicate_descriptor_survives() {
        let path = std::env::temp_dir().join(crate::resource_id("forge-lock-test").unwrap());
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .unwrap();
        let duplicate = file.try_clone().unwrap();
        let owner = Exclusive::try_new(file, "test").unwrap();
        assert!(Exclusive::try_new(File::open(&path).unwrap(), "other").is_err());
        drop(owner);
        let replacement = Exclusive::try_new(File::open(&path).unwrap(), "replacement").unwrap();
        drop(duplicate);
        // Closing the non-owning duplicate must not unlock the replacement.
        assert!(Exclusive::try_new(File::open(&path).unwrap(), "other").is_err());
        drop(replacement);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn replaced_lock_path_does_not_match_held_inode() {
        let path = std::env::temp_dir().join(crate::resource_id("stale-lock-test").unwrap());
        let lease = owned(&path, true).unwrap();
        std::fs::remove_file(&path).unwrap();
        let replacement = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .unwrap();
        assert!(verify_identity(&lease.file, &path).is_err());
        drop(replacement);
        drop(lease);
        std::fs::remove_file(path).unwrap();
    }
}
