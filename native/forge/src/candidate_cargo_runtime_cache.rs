//! Bounded copies of explicitly granted Cargo caches; never hardlink host files.
use anyhow::{bail, ensure, Context, Result};
use cap_std::fs::{
    Dir, DirBuilder, DirBuilderExt, Metadata, MetadataExt, OpenOptions, OpenOptionsExt,
};
use std::{
    ffi::OsStr,
    io::{self, Read, Write},
    path::Path,
};
const MAX_ENTRIES: usize = 100_000;
const MAX_TOTAL: u64 = 4 * 1024 * 1024 * 1024;
const MAX_FILE: u64 = 256 * 1024 * 1024;
struct Budget {
    entries: usize,
    total: u64,
}
fn options_dir() -> OpenOptions {
    let mut value = OpenOptions::new();
    value
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    value
}
fn open_dir(parent: &Dir, name: &OsStr) -> Result<Dir> {
    Ok(Dir::from_std_file(
        parent.open_with(name, &options_dir())?.into_std(),
    ))
}
fn stable(a: &Metadata, b: &Metadata) -> bool {
    a.is_file()
        && b.is_file()
        && (
            a.dev(),
            a.ino(),
            a.len(),
            a.mode(),
            a.mtime(),
            a.mtime_nsec(),
            a.ctime(),
            a.ctime_nsec(),
        ) == (
            b.dev(),
            b.ino(),
            b.len(),
            b.mode(),
            b.mtime(),
            b.mtime_nsec(),
            b.ctime(),
            b.ctime_nsec(),
        )
}
fn copy_file(
    source: &Dir,
    target: &Dir,
    name: &OsStr,
    meta: &Metadata,
    budget: &mut Budget,
) -> Result<()> {
    ensure!(meta.len() <= MAX_FILE, "Cargo cache file exceeds 256 MiB");
    budget.total = budget
        .total
        .checked_add(meta.len())
        .context("Cargo cache size overflow")?;
    ensure!(budget.total <= MAX_TOTAL, "Cargo cache exceeds 4 GiB");
    let mut input = source.open_with(
        name,
        OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC),
    )?;
    ensure!(
        stable(meta, &input.metadata()?),
        "Cargo cache file changed before read"
    );
    let mode = if meta.mode() & 0o111 != 0 {
        0o700
    } else {
        0o600
    };
    let mut output = target.open_with(
        name,
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC),
    )?;
    let copied = io::copy(&mut (&mut input).take(meta.len() + 1), &mut output)?;
    output.flush()?;
    ensure!(
        copied == meta.len()
            && stable(meta, &input.metadata()?)
            && stable(meta, &source.symlink_metadata(name)?),
        "Cargo cache file changed while copying"
    );
    Ok(())
}
fn walk(
    source: &Dir,
    target: &Dir,
    depth: usize,
    path_bytes: usize,
    budget: &mut Budget,
) -> Result<()> {
    ensure!(depth <= 128, "Cargo cache depth exceeds 128");
    for entry in source.entries()? {
        let entry = entry?;
        budget.entries = budget
            .entries
            .checked_add(1)
            .context("Cargo cache entry overflow")?;
        ensure!(
            budget.entries <= MAX_ENTRIES,
            "Cargo cache entries exceed 100000"
        );
        let name = entry.file_name();
        let len = path_bytes
            .checked_add(name.len() + 1)
            .context("Cargo cache path overflow")?;
        ensure!(len <= 4096, "Cargo cache path exceeds 4096 bytes");
        let meta = source.symlink_metadata(&name)?;
        if meta.is_dir() {
            ensure!(depth < 128, "Cargo cache depth exceeds 128");
            let child = open_dir(source, &name)?;
            target.create_dir_with(&name, DirBuilder::new().mode(0o700))?;
            let dest = open_dir(target, &name)?;
            walk(&child, &dest, depth + 1, len, budget)?;
        } else if meta.is_file() {
            copy_file(source, target, &name, &meta, budget)?;
        } else {
            bail!("Cargo cache refuses symlinks and special files");
        }
    }
    Ok(())
}
pub(super) fn copy_seed(seed: &Path, target: &Path) -> Result<()> {
    let root = Dir::open_ambient_dir(seed, cap_std::ambient_authority())?;
    let dest = Dir::open_ambient_dir(target, cap_std::ambient_authority())?;
    let mut budget = Budget {
        entries: 0,
        total: 0,
    };
    for group in ["registry", "git"] {
        let group_dir = match root.symlink_metadata(group) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Ok(meta) if meta.is_dir() => open_dir(&root, OsStr::new(group))?,
            Ok(_) => bail!("unsafe Cargo cache group"),
            Err(error) => return Err(error.into()),
        };
        dest.create_dir_with(group, DirBuilder::new().mode(0o700))?;
        let dest_group = open_dir(&dest, OsStr::new(group))?;
        for name in if group == "registry" {
            ["cache", "index"]
        } else {
            ["db", "checkouts"]
        } {
            match group_dir.symlink_metadata(name) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Ok(meta) if meta.is_dir() => (),
                Ok(_) => bail!("unsafe Cargo cache section"),
                Err(error) => return Err(error.into()),
            }
            let source = open_dir(&group_dir, OsStr::new(name))?;
            dest_group.create_dir_with(name, DirBuilder::new().mode(0o700))?;
            let target = open_dir(&dest_group, OsStr::new(name))?;
            walk(
                &source,
                &target,
                1,
                group.len() + name.len() + 1,
                &mut budget,
            )?;
        }
    }
    Ok(())
}
