//! Bounded regular-file input without following a final symlink or opening FIFOs.
use anyhow::{ensure, Context, Result};
use std::fs::OpenOptions;
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

pub fn read(path: &Path, maximum: usize) -> Result<Vec<u8>> {
    let limit = u64::try_from(maximum)?
        .checked_add(1)
        .context("file read bound overflow")?;
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let metadata = file.metadata()?;
    ensure!(metadata.is_file(), "expected regular file");
    ensure!(metadata.len() < limit, "file exceeds byte limit");
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(limit).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= maximum, "file exceeds byte limit");
    Ok(bytes)
}
