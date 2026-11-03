//! Read bounded Git trees/index blobs into the reconciliation representation.
//! This loader never applies filters or writes Git objects.
use crate::{candidate_source::Source, checkout_sync_reconcile::Entry};
use anyhow::{bail, ensure, Context, Result};
use git2::Oid;
use std::collections::BTreeMap;
const MAX_ENTRY: usize = 64 * 1024 * 1024;
const MAX_TOTAL: usize = 2 * 1024 * 1024 * 1024;

pub(crate) fn charge(total: &mut usize, size: usize) -> Result<()> {
    ensure!(size <= MAX_ENTRY, "checkout blob exceeds limit");
    *total = total
        .checked_add(size)
        .context("checkout byte budget overflow")?;
    ensure!(*total <= MAX_TOTAL, "checkout byte budget exceeded");
    Ok(())
}
pub(crate) fn validate_path(path: &str) -> Result<()> {
    crate::candidate_source::safe_path(path.as_bytes())?;
    ensure!(
        !path.split('/').any(|p| p.eq_ignore_ascii_case(".git")),
        "Git metadata in checkout tree"
    );
    Ok(())
}
pub(crate) fn blob(source: &Source, oid: Oid, mode: u32, total: &mut usize) -> Result<Entry> {
    let (size, kind) = source.repository().odb()?.read_header(oid)?;
    ensure!(
        kind == git2::ObjectType::Blob,
        "checkout entry is not a blob"
    );
    charge(total, size)?;
    let blob = source.repository().find_blob(oid)?;
    ensure!(blob.size() == size, "checkout blob size changed");
    let bytes = blob.content().to_vec();
    match mode {
        0o100644 | 0o100755 => Ok(Entry::File {
            executable: mode == 0o100755,
            bytes,
        }),
        0o120000 => {
            ensure!(
                bytes.len() <= 4096 && !bytes.contains(&0),
                "invalid symlink target"
            );
            Ok(Entry::Symlink(bytes))
        }
        _ => bail!("unsupported checkout tree mode {mode:o}"),
    }
}
pub(crate) fn load(
    source: &Source,
    tree: Oid,
    total: &mut usize,
) -> Result<BTreeMap<String, Entry>> {
    let mut out = BTreeMap::new();
    for item in source.list_tree(&tree.to_string())? {
        validate_path(&item.path)?;
        let oid = Oid::from_str(&item.oid)?;
        let mode = u32::from_str_radix(&item.mode, 8)?;
        let entry = blob(source, oid, mode, total)?;
        ensure!(
            out.insert(item.path, entry).is_none(),
            "duplicate tree path"
        );
    }
    Ok(out)
}
