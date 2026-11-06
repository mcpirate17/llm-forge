//! One-shot no-symlink path resolution for held input observations.
//!
//! Linux requires openat2 (kernel 5.6 or newer) and permission to call it.
//! Unsupported or blocked resolution fails closed; descriptors are not cached.
use anyhow::{ensure, Context, Result};
use std::path::Path;

pub(crate) fn check(path: &Path) -> Result<()> {
    use std::{
        ffi::CString,
        os::{
            fd::{FromRawFd, OwnedFd},
            unix::ffi::OsStrExt,
        },
    };
    ensure!(
        path.is_absolute()
            && path.components().all(|part| matches!(
                part,
                std::path::Component::RootDir | std::path::Component::Normal(_)
            )),
        "no-symlink input path must be absolute and normalized"
    );
    let name = CString::new(path.as_os_str().as_bytes()).context("input path contains NUL")?;
    // SAFETY: all-zero open_how is a valid initial state; only defined fields are set.
    let mut how: libc::open_how = unsafe { std::mem::zeroed() };
    how.flags = (libc::O_PATH | libc::O_CLOEXEC) as u64;
    how.resolve = libc::RESOLVE_NO_SYMLINKS | libc::RESOLVE_NO_MAGICLINKS;
    // SAFETY: name and how remain live and correctly sized for this syscall. A
    // successful result is a newly owned descriptor, immediately wrapped below.
    let raw = unsafe {
        libc::syscall(
            libc::SYS_openat2,
            libc::AT_FDCWD,
            name.as_ptr(),
            &how,
            std::mem::size_of::<libc::open_how>(),
        )
    };
    if raw < 0 {
        return Err(std::io::Error::last_os_error()).context(
            "openat2 no-symlink input resolution failed (Linux 5.6+ and syscall access required)",
        );
    }
    // SAFETY: successful openat2 returns an owned nonnegative file descriptor.
    let _held = unsafe { OwnedFd::from_raw_fd(raw as i32) };
    Ok(())
}

#[cfg(test)]
#[path = "nofollow_path_tests.rs"]
mod tests;
