//! Opt-in committed preimage verification, not protection against a same-UID writer.
use super::{paths, Snapshot};
use crate::candidate_source::{Source, TreeEntry};
use anyhow::{ensure, Context, Result};
use cap_std::fs::{Dir, Metadata, MetadataExt, OpenOptions, OpenOptionsExt};
use std::{
    collections::{btree_map::Entry, BTreeMap},
    io::Read,
};

const MAX_ENTRIES: usize = 100_000;
const MAX_PATH_BYTES: usize = 64 * 1024 * 1024;
type Stamp = (u64, u64, u32, u32, u32, u64, u64, i64, i64, i64, i64);
fn stamp(m: &Metadata) -> Stamp {
    (
        m.dev(),
        m.ino(),
        m.mode(),
        m.uid(),
        m.gid(),
        m.nlink(),
        m.len(),
        m.mtime(),
        m.mtime_nsec(),
        m.ctime(),
        m.ctime_nsec(),
    )
}
fn directory(m: &Metadata) -> Result<()> {
    ensure!(
        m.is_dir() && m.uid() == unsafe { libc::geteuid() } && m.mode() & 0o022 == 0,
        "unsafe materialized snapshot directory"
    );
    Ok(())
}
fn expected(entries: &[TreeEntry]) -> Result<BTreeMap<String, Option<&TreeEntry>>> {
    let mut out = BTreeMap::new();
    let mut bytes = 0usize;
    for entry in entries {
        let mut path = entry.path.as_str();
        loop {
            let count = out.len();
            if let Entry::Vacant(slot) = out.entry(path.to_owned()) {
                bytes = bytes
                    .checked_add(path.len())
                    .context("snapshot path budget overflow")?;
                ensure!(
                    count < MAX_ENTRIES && bytes <= MAX_PATH_BYTES,
                    "materialized snapshot path budget exceeded"
                );
                slot.insert(if path == entry.path {
                    Some(entry)
                } else {
                    None
                });
            }
            match path.rsplit_once('/') {
                Some((parent, _)) => path = parent,
                None => break,
            }
        }
    }
    Ok(out)
}
pub(super) fn materialized(snapshot: &Snapshot, source: &Source) -> Result<()> {
    snapshot.verify_staging()?;
    let before = snapshot.directory.dir_metadata()?;
    directory(&before)?;
    ensure!(
        (before.dev(), before.ino()) == snapshot.identity,
        "held snapshot root changed"
    );
    let plan = paths::prepare(source, &snapshot.report.tree_oid, snapshot.limits)?;
    ensure!(
        plan.skipped == 0,
        "materialized verification does not support skipped gitlinks"
    );
    let mut expected = expected(&plan.entries)?;
    walk(&snapshot.directory, "", 0, &mut expected, source)?;
    ensure!(
        expected.is_empty(),
        "materialized snapshot has missing paths"
    );
    ensure!(
        stamp(&before) == stamp(&snapshot.directory.dir_metadata()?),
        "snapshot root changed during verification"
    );
    snapshot.verify_staging()
}
fn walk(
    dir: &Dir,
    prefix: &str,
    depth: usize,
    expected: &mut BTreeMap<String, Option<&TreeEntry>>,
    source: &Source,
) -> Result<()> {
    ensure!(depth <= 256, "materialized snapshot depth exceeds 256");
    let before = dir.dir_metadata()?;
    directory(&before)?;
    for item in dir.entries()? {
        let name = item?.file_name();
        let name = name
            .to_str()
            .context("materialized snapshot path must be UTF-8")?;
        crate::candidate_source::safe_path(name.as_bytes())?;
        ensure!(!name.contains('/'), "invalid materialized snapshot name");
        let path = if prefix.is_empty() {
            name.to_owned()
        } else {
            format!("{prefix}/{name}")
        };
        let entry = expected
            .remove(&path)
            .with_context(|| format!("extra materialized path: {path}"))?;
        let named = dir.symlink_metadata(name)?;
        if let Some(entry) = entry {
            leaf(dir, name, &named, entry, source)?;
        } else {
            directory(&named)?;
            let child = Dir::from_std_file(
                dir.open_with(
                    name,
                    OpenOptions::new()
                        .read(true)
                        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW),
                )?
                .into_std(),
            );
            ensure!(
                stamp(&named) == stamp(&child.dir_metadata()?),
                "snapshot directory replaced: {path}"
            );
            walk(&child, &path, depth + 1, expected, source)?;
        }
        ensure!(
            stamp(&named) == stamp(&dir.symlink_metadata(name)?),
            "snapshot entry changed during verification: {path}"
        );
    }
    ensure!(
        stamp(&before) == stamp(&dir.dir_metadata()?),
        "snapshot directory changed: {prefix}"
    );
    Ok(())
}
fn leaf(dir: &Dir, name: &str, named: &Metadata, entry: &TreeEntry, source: &Source) -> Result<()> {
    ensure!(
        named.uid() == unsafe { libc::geteuid() } && named.nlink() == 1,
        "materialized snapshot leaf ownership or hardlink changed: {}",
        entry.path
    );
    ensure!(
        Some(named.len()) == entry.size,
        "materialized snapshot size changed: {}",
        entry.path
    );
    let blob = source
        .repository()
        .find_blob(git2::Oid::from_str(&entry.oid)?)?;
    ensure!(
        Some(blob.size() as u64) == entry.size,
        "snapshot Git blob size changed"
    );
    if entry.mode == "120000" {
        ensure!(
            named.is_symlink() && named.len() <= 4096,
            "materialized symlink type changed"
        );
        let target = dir.read_link_contents(name)?;
        ensure!(
            target.as_os_str().as_encoded_bytes() == blob.content(),
            "materialized symlink changed: {}",
            entry.path
        );
    } else {
        let mode = if entry.mode == "100755" { 0o755 } else { 0o644 };
        ensure!(
            named.is_file() && named.mode() & 0o7777 == mode,
            "materialized file mode changed: {}",
            entry.path
        );
        let mut file = dir.open_with(
            name,
            OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC),
        )?;
        ensure!(
            stamp(named) == stamp(&file.metadata()?),
            "materialized file replaced before read"
        );
        let mut buffer = [0u8; 64 * 1024];
        for expected in blob.content().chunks(buffer.len()) {
            file.read_exact(&mut buffer[..expected.len()])?;
            ensure!(
                &buffer[..expected.len()] == expected,
                "materialized content changed: {}",
                entry.path
            );
        }
        ensure!(
            file.read(&mut buffer[..1])? == 0 && stamp(named) == stamp(&file.metadata()?),
            "materialized file changed while reading: {}",
            entry.path
        );
    }
    Ok(())
}
