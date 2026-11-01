//! Nonblocking exclusive flock ownership. Explicit unlock matters even with
//! CLOEXEC: a duplicate or a concurrent fork can keep the open-file description
//! alive after the owner closes its descriptor.
use anyhow::{ensure, Result};
use std::fs::File;
use std::os::fd::AsRawFd;

pub struct Exclusive {
    file: File,
    purpose: &'static str,
}

impl Exclusive {
    pub fn try_new(file: File, purpose: &'static str) -> Result<Self> {
        ensure!(
            unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0,
            "{purpose} is busy or cannot be locked: {}; retry",
            std::io::Error::last_os_error()
        );
        Ok(Self { file, purpose })
    }
}

impl Drop for Exclusive {
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
}
