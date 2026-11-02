//! Review lease over the host's shared Git-common governance mutex.
//! A token is metadata, not an owned flock: token-only inheritance fails closed.
use anyhow::{bail, ensure, Context, Result};
use std::{
    fs::{self, File, OpenOptions},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::{MetadataExt, OpenOptionsExt},
    },
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

pub(crate) struct ReviewLock {
    file: File,
    path: PathBuf,
    parent: (u64, u64),
    borrowed: bool,
}
impl Drop for ReviewLock {
    fn drop(&mut self) {
        if !self.borrowed && unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) } != 0 {
            eprintln!(
                "forge: release review lock: {}",
                std::io::Error::last_os_error()
            );
        }
    }
}
impl ReviewLock {
    pub(crate) fn acquire(repo: &Path, timeout: Duration) -> Result<Self> {
        let path = crate::ownership::checked_claim_path(repo, "commit-review.lock")?
            .context("repository has no Git common directory")?;
        fs::create_dir_all(path.parent().context("governance lock parent missing")?)?;
        let raw = std::env::var_os("LLM_GOVERNANCE_COMMIT_LOCK_FD");
        let token = std::env::var_os("LLM_GOVERNANCE_COMMIT_LOCK_TOKEN");
        choose(&path, raw, token.is_some(), timeout)
    }
    pub(crate) fn verify(&self) -> Result<()> {
        ensure!(
            parent_identity(&self.path)? == self.parent,
            "governance lock directory changed"
        );
        verify(&self.file, &self.path)?;
        if self.borrowed {
            verify_exclusive(&self.file, &self.path)?;
        }
        Ok(())
    }
}
fn parent_identity(path: &Path) -> Result<(u64, u64)> {
    let parent = fs::symlink_metadata(path.parent().context("governance lock parent missing")?)?;
    ensure!(
        parent.is_dir() && parent.uid() == unsafe { libc::geteuid() } && parent.mode() & 0o002 == 0,
        "unsafe governance lock directory"
    );
    Ok((parent.dev(), parent.ino()))
}
fn choose(
    path: &Path,
    raw: Option<std::ffi::OsString>,
    token: bool,
    timeout: Duration,
) -> Result<ReviewLock> {
    if let Some(raw) = raw {
        let text = raw
            .to_str()
            .context("inherited governance fd is not UTF-8")?;
        let fd: i32 = text.parse().context("invalid inherited governance fd")?;
        return inherited(path, fd);
    }
    ensure!(!token, "token-only governance inheritance cannot retain the owning flock; pass the inherited lock descriptor");
    shared(path, timeout)
}
fn open(path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)?;
    verify(&file, path)?;
    Ok(file)
}
fn verify(file: &File, path: &Path) -> Result<()> {
    let held = file.metadata()?;
    let current = fs::symlink_metadata(path)?;
    ensure!(
        held.is_file()
            && current.is_file()
            && held.nlink() == 1
            && current.nlink() == 1
            && held.uid() == unsafe { libc::geteuid() }
            && current.uid() == held.uid()
            && held.mode() & 0o022 == 0
            && current.mode() & 0o022 == 0
            && (held.dev(), held.ino()) == (current.dev(), current.ino()),
        "governance lock identity or permissions changed"
    );
    Ok(())
}
fn shared(path: &Path, timeout: Duration) -> Result<ReviewLock> {
    let parent = parent_identity(path)?;
    let file = open(path)?;
    let deadline = Instant::now()
        .checked_add(timeout)
        .context("lock timeout overflow")?;
    loop {
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_SH | libc::LOCK_NB) } == 0 {
            let held = ReviewLock {
                file,
                path: path.to_owned(),
                parent,
                borrowed: false,
            };
            held.verify()?;
            return Ok(held);
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::WouldBlock
            && error.kind() != std::io::ErrorKind::Interrupted
        {
            return Err(error.into());
        }
        if Instant::now() >= deadline {
            bail!(
                "governance lock remained busy for {:.1}s",
                timeout.as_secs_f64()
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}
fn inherited(path: &Path, raw: i32) -> Result<ReviewLock> {
    let parent = parent_identity(path)?;
    ensure!(raw > 2, "invalid inherited governance fd");
    let fd = unsafe { libc::fcntl(raw, libc::F_DUPFD_CLOEXEC, 3) };
    ensure!(
        fd >= 0,
        "cannot duplicate inherited governance fd: {}",
        std::io::Error::last_os_error()
    );
    let file = unsafe { File::from_raw_fd(fd) };
    verify_exclusive(&file, path)?;
    let held = ReviewLock {
        file,
        path: path.to_owned(),
        parent,
        borrowed: true,
    };
    held.verify()?;
    Ok(held)
}
fn verify_exclusive(file: &File, path: &Path) -> Result<()> {
    verify(file, path)?;
    let verifier = open(path)?;
    let result = unsafe { libc::flock(verifier.as_raw_fd(), libc::LOCK_SH | libc::LOCK_NB) };
    if result == 0 {
        unsafe {
            libc::flock(verifier.as_raw_fd(), libc::LOCK_UN);
        }
        bail!("inherited governance descriptor does not have a held exclusive lease");
    }
    ensure!(
        std::io::Error::last_os_error().kind() == std::io::ErrorKind::WouldBlock,
        "cannot verify inherited governance lease"
    );
    ensure!(
        unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0,
        "inherited descriptor does not own exclusive governance lease"
    );
    verify(file, path)
}
#[cfg(test)]
#[path = "candidate_review_lock_tests.rs"]
mod tests;
