//! Copy-on-write launcher publication through pinned directory capabilities.
//! An exchange retains the displaced inode until post-publication validation.
use super::paths::{Bin, Stamp, MAX_FILE};
use anyhow::{ensure, Context, Result};
use cap_std::fs::{File, OpenOptions, OpenOptionsExt, Permissions, PermissionsExt};
use std::{
    ffi::{CString, OsStr, OsString},
    io::{Read, Seek, SeekFrom, Write},
    os::{fd::AsRawFd, unix::ffi::OsStrExt},
};

fn read_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    options
}
pub(super) struct Script {
    file: File,
    pub stamp: Stamp,
}
impl Script {
    pub fn open(bin: &Bin, name: &OsStr, expected: &Stamp) -> Result<Self> {
        let file = bin.dir.open_with(name, &read_options())?;
        let metadata = file.metadata()?;
        ensure!(
            metadata.is_file() && Stamp::capture(&metadata) == *expected,
            "launcher changed before open"
        );
        Ok(Self {
            file,
            stamp: expected.clone(),
        })
    }
    pub fn verify(&self, bin: &Bin, name: &OsStr) -> Result<()> {
        ensure!(
            self.stamp == Stamp::capture(&self.file.metadata()?)
                && self.stamp == Stamp::capture(&bin.dir.symlink_metadata(name)?),
            "launcher changed during relocation capture"
        );
        Ok(())
    }
    pub fn prefix(&mut self) -> Result<Vec<u8>> {
        let mut prefix = Vec::new();
        (&mut self.file).take(4097).read_to_end(&mut prefix)?;
        if let Some(newline) = prefix.iter().position(|byte| *byte == b'\n') {
            prefix.truncate(newline + 1);
        }
        Ok(prefix)
    }
    pub fn bytes(&mut self, bin: &Bin, name: &OsStr) -> Result<Vec<u8>> {
        ensure!(
            self.stamp.length <= MAX_FILE as u64,
            "matching launcher exceeds 64 MiB"
        );
        self.file.seek(SeekFrom::Start(0))?;
        let mut bytes = Vec::new();
        (&mut self.file)
            .take(MAX_FILE as u64 + 1)
            .read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() <= MAX_FILE && bytes.len() as u64 == self.stamp.length,
            "launcher changed size or exceeds bound"
        );
        self.verify(bin, name)?;
        Ok(bytes)
    }
}
struct Stage {
    name: OsString,
    file: File,
    stamp: Stamp,
}
impl Stage {
    fn verify(&self, bin: &Bin) -> Result<()> {
        let metadata = self.file.metadata()?;
        ensure!(
            self.stamp == Stamp::capture(&metadata)
                && self.stamp == Stamp::capture(&bin.dir.symlink_metadata(&self.name)?),
            "relocation staging inode changed; retain all artifacts"
        );
        Ok(())
    }
    fn remove(&self, bin: &Bin) -> Result<()> {
        self.verify(bin)?;
        bin.dir.remove_file(&self.name)?;
        bin.sync()
    }
}
fn stage(bin: &Bin, bytes: &[u8], mode: u32) -> Result<Stage> {
    let name = OsString::from(format!(".{}", crate::resource_id("runtime-relocation")?));
    let mut file = bin.dir.open_with(
        &name,
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC),
    )?;
    // Once a staging inode exists, retain it on any setup error. Its exact
    // path is reported rather than deleting a possibly replaced foreign entry.
    let result = (|| -> Result<Stamp> {
        file.write_all(bytes)?;
        file.set_permissions(Permissions::from_mode(mode & 0o7777))?;
        file.sync_all()?;
        Ok(Stamp::capture(&file.metadata()?))
    })();
    let stamp = result
        .with_context(|| format!("staging failed; retain {}", bin.path.join(&name).display()))?;
    Ok(Stage { name, file, stamp })
}
fn exchange(bin: &Bin, left: &OsStr, right: &OsStr) -> Result<()> {
    let left = CString::new(left.as_bytes())?;
    let right = CString::new(right.as_bytes())?;
    // SAFETY: both C strings are live and both names are relative to this held directory.
    let result = unsafe {
        libc::renameat2(
            bin.dir.as_raw_fd(),
            left.as_ptr(),
            bin.dir.as_raw_fd(),
            right.as_ptr(),
            libc::RENAME_EXCHANGE,
        )
    };
    if result != 0 {
        return Err(std::io::Error::last_os_error()).context("atomic launcher exchange");
    }
    Ok(())
}
fn verify_content(bin: &Bin, name: &OsStr, expected: &Stamp, bytes: &[u8]) -> Result<()> {
    let metadata = bin.dir.symlink_metadata(name)?;
    let current = Stamp::capture(&metadata);
    ensure!(
        metadata.is_file()
            && current.identity() == expected.identity()
            && current.mode == expected.mode
            && current.length == bytes.len() as u64,
        "post-publication launcher identity differs; retain displaced inode"
    );
    let mut file = Script::open(bin, name, &current)?;
    ensure!(
        file.bytes(bin, name)? == bytes,
        "post-publication launcher content differs; retain displaced inode"
    );
    Ok(())
}
pub(super) fn replace(
    bin: &Bin,
    name: &OsStr,
    original: &Script,
    bytes: &[u8],
    rewritten: &[u8],
    before_publish: &mut dyn FnMut() -> Result<()>,
) -> Result<()> {
    replace_inner(
        bin,
        name,
        original,
        bytes,
        rewritten,
        before_publish,
        &mut |_| Ok(()),
    )
}
fn replace_inner(
    bin: &Bin,
    name: &OsStr,
    original: &Script,
    bytes: &[u8],
    rewritten: &[u8],
    before_publish: &mut dyn FnMut() -> Result<()>,
    hook: &mut dyn FnMut(&str) -> Result<()>,
) -> Result<()> {
    ensure!(
        rewritten.len() <= MAX_FILE,
        "rewritten launcher exceeds 64 MiB"
    );
    original.verify(bin, name)?;
    let staged = stage(bin, rewritten, original.stamp.mode)?;
    let admitted = (|| -> Result<()> {
        hook("staged")?;
        bin.verify()?;
        original.verify(bin, name)?;
        staged.verify(bin)?;
        before_publish()?;
        // Recheck after callbacks and immediately before the atomic exchange.
        bin.verify()?;
        original.verify(bin, name)?;
        staged.verify(bin)?;
        hook("before_exchange")?;
        exchange(bin, &staged.name, name)
    })();
    if let Err(error) = admitted {
        return match staged.remove(bin) {
            Ok(()) => Err(error),
            Err(cleanup) => Err(error.context(format!(
                "staging cleanup refused; retain {}: {cleanup:#}",
                bin.path.join(&staged.name).display()
            ))),
        };
    }
    // Cancellation is intentionally suppressed after exchange until validation
    // and cleanup finish. On uncertainty, never discard the displaced entry.
    let finalized = (|| -> Result<()> {
        hook("exchanged")?;
        bin.sync()?;
        bin.verify()?;
        verify_content(bin, name, &staged.stamp, rewritten)?;
        verify_content(bin, &staged.name, &original.stamp, bytes)?;
        bin.dir.remove_file(&staged.name)?;
        bin.sync()
    })();
    finalized.with_context(|| format!("launcher publication may have occurred; retain any displaced entry at {} (held bin inode {:?})", bin.path.join(&staged.name).display(), bin.identity()))
}

#[cfg(test)]
#[path = "worktree_runtime_publish_tests.rs"]
mod tests;
