//! Read-only checkout capture. No snapshot, index tree, object or ref is written.
//! Only the cooperative lease's lock directory/file may persist after inspection.
use crate::{
    checkout_sync_eol::Policy,
    checkout_sync_inventory::{self, Inventory},
    checkout_sync_journal::Identity,
    checkout_sync_plan::{self, Plan},
    checkout_sync_preflight::Preflight,
    checkout_sync_reconcile::Entry,
    checkout_sync_tree,
    checkout_sync_worktree::{Root, SyncLease},
};
use anyhow::{ensure, Context, Result};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

type Entries = BTreeMap<String, Entry>;
struct Capture {
    raw: Entries,
    leaves: BTreeMap<String, Identity>,
    directories: BTreeMap<String, Identity>,
}

pub(crate) fn inspect(
    preflight: &Preflight,
    cancel: &mut dyn FnMut() -> Result<()>,
) -> Result<Plan> {
    cancel()?;
    let lease = SyncLease::acquire(preflight.source.root(), cancel)?;
    preflight.verify()?;
    let source = &preflight.source;
    let repo = source.repository();
    let mut total = 0;
    let base = checkout_sync_tree::load(
        source,
        repo.find_commit(preflight.old)?.tree_id(),
        &mut total,
    )?;
    cancel()?;
    let staged = staged(preflight, &mut total, cancel)?;
    let incoming = checkout_sync_tree::load(
        source,
        repo.find_commit(preflight.new)?.tree_id(),
        &mut total,
    )?;
    let tracked: BTreeSet<_> = base.keys().chain(staged.keys()).cloned().collect();
    let inventory = checkout_sync_inventory::inventory(source, &lease, &tracked)?;
    let root = Root::open(source.root())?;
    let capture = capture(&root, &lease, &inventory, &mut total, cancel)?;
    preflight.verify()?;
    // Keep the inventory for validation even if reconciliation reports a conflict.
    let expected = inventory.clone();
    let result = checkout_sync_plan::from_maps(
        source,
        &lease,
        &base,
        &staged,
        &capture.raw,
        &incoming,
        inventory,
    );
    verify_capture(&root, &lease, &capture, cancel)?;
    ensure!(
        checkout_sync_inventory::inventory(source, &lease, &tracked)? == expected,
        "checkout collision inventory changed during preview"
    );
    preflight.verify()?;
    lease.verify()?;
    cancel()?;
    let plan = result?;
    for (path, policy) in &plan.policies {
        ensure!(
            *policy == Policy::capture(source, &lease, path)?,
            "Git attributes or config changed during preview at {path}"
        );
    }
    preflight.verify()?;
    lease.verify()?;
    cancel()?;
    Ok(plan)
}
fn staged(
    preflight: &Preflight,
    total: &mut usize,
    cancel: &mut dyn FnMut() -> Result<()>,
) -> Result<Entries> {
    // parse_index uses a sealed memfd; it does not refresh/write the live index.
    let index = match &preflight.original_index {
        Some(expected) => {
            let raw = crate::bounded_file::read(&preflight.index_path, 64 * 1024 * 1024)?;
            ensure!(
                format!("{:x}", Sha256::digest(&raw)) == expected.sha256,
                "index bytes changed before preview capture"
            );
            let parsed = crate::candidate_source::parse_index(&raw)?;
            // Use the executor's exact extension admission without creating an index.
            drop(crate::checkout_sync_index_build::strip_tree_extension(
                &raw,
            )?);
            parsed
        }
        None => git2::Index::new()?,
    };
    let mut entries = BTreeMap::new();
    for item in index.iter() {
        cancel()?;
        let path = crate::candidate_source::safe_path(&item.path)?.to_owned();
        checkout_sync_tree::validate_path(&path)?;
        ensure!(
            item.flags & 0xf000 == 0 && item.flags_extended == 0,
            "unsupported staged index flags"
        );
        let entry = checkout_sync_tree::blob(&preflight.source, item.id, item.mode, total)?;
        ensure!(
            entries.insert(path, entry).is_none(),
            "duplicate index path"
        );
    }
    Ok(entries)
}
fn capture(
    root: &Root,
    lease: &SyncLease,
    inventory: &Inventory,
    total: &mut usize,
    cancel: &mut dyn FnMut() -> Result<()>,
) -> Result<Capture> {
    let mut captured = Capture {
        raw: BTreeMap::new(),
        leaves: BTreeMap::new(),
        directories: BTreeMap::new(),
    };
    for path in &inventory.directories {
        cancel()?;
        captured
            .directories
            .insert(path.clone(), root.observe_directory_identity(path, lease)?);
    }
    for path in inventory.leaves.difference(&inventory.ignored) {
        cancel()?;
        let observed = root
            .observe(path, lease)?
            .context("preview leaf disappeared")?;
        checkout_sync_tree::charge(total, observed.bytes.len())?;
        let entry = match observed.identity.mode & libc::S_IFMT {
            libc::S_IFREG => Entry::File {
                executable: observed.identity.mode & 0o111 != 0,
                bytes: observed.bytes,
            },
            libc::S_IFLNK => Entry::Symlink(observed.bytes),
            _ => anyhow::bail!("preview leaf changed type: {path}"),
        };
        captured.raw.insert(path.clone(), entry);
        captured.leaves.insert(path.clone(), observed.identity);
    }
    Ok(captured)
}
fn verify_capture(
    root: &Root,
    lease: &SyncLease,
    capture: &Capture,
    cancel: &mut dyn FnMut() -> Result<()>,
) -> Result<()> {
    for (path, expected) in &capture.leaves {
        cancel()?;
        ensure!(
            root.observe(path, lease)?.as_ref().map(|v| &v.identity) == Some(expected),
            "checkout leaf changed during preview: {path}"
        );
    }
    for (path, expected) in &capture.directories {
        cancel()?;
        ensure!(
            root.observe_directory_identity(path, lease)? == *expected,
            "checkout directory changed during preview: {path}"
        );
    }
    Ok(())
}

#[cfg(test)]
#[path = "checkout_sync_preview_tests.rs"]
mod tests;
