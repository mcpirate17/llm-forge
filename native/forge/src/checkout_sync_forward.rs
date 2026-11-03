//! Guarded shared-checkout fast-forward executor. No CLI is wired here.
use crate::{
    candidate_commit_snapshot::{self, Limits},
    checkout_sync_eol::Policy,
    checkout_sync_index::IndexLock,
    checkout_sync_index_build, checkout_sync_inventory,
    checkout_sync_journal::{Event, Identity, Journal},
    checkout_sync_plan::{self, Op, Plan},
    checkout_sync_preflight::Preflight,
    checkout_sync_reconcile::Entry,
    checkout_sync_recovery::{self, RecoveryOutcome},
    checkout_sync_refs::{RefLocks, RefPublication},
    checkout_sync_worktree::{Content, Depot, Root, SyncLease},
};
use anyhow::{bail, ensure, Context, Result};
use git2::Signature;
use std::{
    collections::BTreeMap,
    fs::File,
    path::{Path, PathBuf},
};

#[derive(Debug)]
pub(crate) enum Outcome {
    Even,
    Published { recovery: PathBuf, snapshot: String },
}

/// The recovery parent must be an existing trusted private directory. No
/// checkout path is published until planning, collision and index checks pass.
pub(crate) fn execute(
    preflight: &Preflight,
    recovery_parent: &Path,
    cancel: &mut dyn FnMut() -> Result<()>,
) -> Result<Outcome> {
    if preflight.behind == 0 {
        preflight.verify()?;
        return Ok(Outcome::Even);
    }
    preflight.verify()?;
    let recovery_parent = validate_recovery_parent(preflight, recovery_parent)?;
    let lease = SyncLease::acquire(preflight.source.root(), cancel)?;
    preflight.verify()?;
    let mut journal = Journal::create(&recovery_parent)?;
    let recovery = journal.path().to_owned();
    let snapshot = format!(
        "refs/snapshots/forge/{}",
        crate::resource_id("checkout-sync")?
    );
    begin(preflight, &snapshot, &mut journal)?;
    let mut state = ForwardState::default();
    let result = run_forward(
        preflight,
        &lease,
        &mut journal,
        &snapshot,
        &mut state,
        cancel,
    );
    if let Err(error) = result {
        let cleanup = release_held(
            preflight,
            &mut journal,
            &mut state.index,
            &mut state.refs,
            state.index_held,
            state.ref_held,
        );
        drop(state.index);
        drop(state.refs);
        drop(journal);
        drop(lease);
        if !state.uncertain && cleanup.is_ok() {
            match checkout_sync_recovery::recover(&recovery, &mut || Ok(())) {
                Ok(outcome) => return Err(recovery_result_error(&recovery, &error, outcome)),
                Err(recovery_error) => bail!("checkout sync failed; conditional recovery retained at {}: {error:#}; recovery: {recovery_error:#}", recovery.display()),
            }
        }
        bail!("checkout sync outcome uncertain; retain recovery at {}: {error:#}; release={cleanup:?}", recovery.display());
    }
    Ok(Outcome::Published { recovery, snapshot })
}

fn validate_recovery_parent(preflight: &Preflight, parent: &Path) -> Result<PathBuf> {
    let parent = parent
        .canonicalize()
        .context("canonicalize recovery parent")?;
    let git_dir = preflight.source.repository().path().canonicalize()?;
    for protected in [
        preflight.source.root(),
        preflight.source.common_dir(),
        git_dir.as_path(),
    ] {
        ensure!(
            !parent.starts_with(protected) && !protected.starts_with(&parent),
            "recovery parent overlaps checkout or Git directory"
        );
    }
    Ok(parent)
}

fn recovery_result_error(
    recovery: &Path,
    error: &anyhow::Error,
    outcome: RecoveryOutcome,
) -> anyhow::Error {
    match outcome {
        RecoveryOutcome::JournalMarkedComplete => anyhow::anyhow!(
            "checkout sync returned an error; journal marks publication complete and no rollback was performed; recovery={}: {error:#}", recovery.display()),
        RecoveryOutcome::Recovered { .. } | RecoveryOutcome::JournalMarkedRecovered => anyhow::anyhow!(
            "checkout sync failed and conditional recovery completed; recovery={}: {error:#}", recovery.display()),
    }
}

#[derive(Default)]
struct ForwardState<'repo> {
    refs: Option<RefLocks<'repo>>,
    index: Option<IndexLock>,
    ref_held: bool,
    index_held: bool,
    uncertain: bool,
}

fn capture_locked<'repo>(
    preflight: &'repo Preflight,
    journal: &mut Journal,
    snapshot: &str,
    state: &mut ForwardState<'repo>,
) -> Result<candidate_commit_snapshot::Recovery> {
    forward(journal, "refs", "acquire")?;
    state.refs = Some(RefLocks::acquire(
        preflight.source.repository(),
        &preflight.branch,
        &preflight.target,
        preflight.old,
        preflight.new,
    )?);
    sync_ref_dirs(preflight)?;
    forward(journal, "refs", "held")?;
    state.ref_held = true;
    forward(journal, "index", "acquire")?;
    state.index = Some(IndexLock::acquire(&preflight.index_path)?);
    sync_index_dir(preflight)?;
    forward(journal, "index", "held")?;
    state.index_held = true;
    preflight.verify()?;
    let idx = state.index.as_mut().context("index lock missing")?;
    idx.revalidate()?;
    journal.append(Event::ObjectIntent {
        purpose: "snapshot capture".into(),
    })?;
    let captured = candidate_commit_snapshot::create_named(
        &preflight.source,
        Limits::default(),
        snapshot,
        |name, oid| {
            journal.append(Event::BeforeSnapshot {
                reference: name.into(),
                oid: oid.to_string(),
            })
        },
    )?;
    let oid = preflight
        .source
        .repository()
        .find_reference(snapshot)?
        .target()
        .context("snapshot ref is not direct")?;
    journal.append(Event::SnapshotReady {
        oid: oid.to_string(),
    })?;
    state.refs.as_ref().context("ref lock missing")?.verify()?;
    idx.revalidate()?;
    preflight.verify()?;
    Ok(captured)
}

fn run_forward<'repo>(
    preflight: &'repo Preflight,
    lease: &SyncLease,
    journal: &mut Journal,
    snapshot: &str,
    state: &mut ForwardState<'repo>,
    cancel: &mut dyn FnMut() -> Result<()>,
) -> Result<()> {
    let captured = capture_locked(preflight, journal, snapshot, state)?;
    let plan = checkout_sync_plan::build(
        &preflight.source,
        lease,
        &captured,
        preflight.old,
        preflight.new,
    )?;
    journal.append(Event::ObjectIntent {
        purpose: "replacement index blobs".into(),
    })?;
    let next = checkout_sync_index_build::build(
        &preflight.source,
        journal.path(),
        state
            .index
            .as_ref()
            .context("index lock missing")?
            .original_bytes(),
        &plan.index,
    )?;
    check_plan(preflight, lease, &plan)?;
    let root = Root::open(preflight.source.root())?;
    for op in &plan.operations {
        if op.before.is_some() {
            validate_observed(op, &root.observe(&op.path, lease)?)?;
        }
    }
    let depot = Depot::create(&root, lease, journal)?;
    let mut published_paths = BTreeMap::new();
    for op in &plan.operations {
        cancel()?;
        state.refs.as_ref().context("ref lock missing")?.verify()?;
        state
            .index
            .as_ref()
            .context("index lock missing")?
            .revalidate()?;
        lease.verify()?;
        publish_op(&root, &depot, lease, journal, op)?;
        published_paths.insert(
            op.path.clone(),
            root.observe(&op.path, lease)?.map(|v| v.identity),
        );
    }
    state.refs.as_ref().context("ref lock missing")?.verify()?;
    state
        .index
        .as_ref()
        .context("index lock missing")?
        .revalidate()?;
    check_policies(preflight, lease, &plan.policies)?;
    publish_final(
        preflight,
        lease,
        journal,
        state,
        &root,
        Publication {
            plan: &plan,
            index: &next,
            paths: &published_paths,
        },
        cancel,
    )
}

struct Publication<'a> {
    plan: &'a Plan,
    index: &'a [u8],
    paths: &'a BTreeMap<String, Option<Identity>>,
}
fn publish_final(
    preflight: &Preflight,
    lease: &SyncLease,
    journal: &mut Journal,
    state: &mut ForwardState<'_>,
    root: &Root,
    publication: Publication<'_>,
    cancel: &mut dyn FnMut() -> Result<()>,
) -> Result<()> {
    let idx = state.index.as_mut().context("index lock missing")?;
    let original_blob = if let Some(bytes) = idx.original_bytes() {
        journal.save_blob("original-index", bytes)?;
        Some("original-index".into())
    } else {
        None
    };
    let staged = idx.prepare(publication.index)?;
    journal.append(Event::BeforeIndex {
        parent_device: preflight.index_parent.0,
        parent_inode: preflight.index_parent.1,
        original: preflight.original_index.clone(),
        original_blob,
        staged,
    })?;
    forward(journal, "index", "release")?;
    state.index_held = false;
    state.uncertain = true;
    let published = idx.publish_prepared()?.journal_identity();
    sync_index_dir(preflight)?;
    forward(journal, "index", "released")?;
    journal.append(Event::AfterIndex {
        published: published.clone(),
    })?;
    drop(state.index.take());
    // The lock name disappeared at rename. Reacquire it and validate the exact
    // published image before advancing HEAD; interruption retains the journal.
    cancel()?;
    forward(journal, "index", "acquire")?;
    state.index = Some(IndexLock::acquire(&preflight.index_path)?);
    sync_index_dir(preflight)?;
    forward(journal, "index", "held")?;
    state.index_held = true;
    let guard = state.index.as_ref().context("index guard missing")?;
    ensure!(
        guard.original().map(|image| image.journal_identity()) == Some(published),
        "index changed after publication"
    );
    guard.revalidate()?;
    validate_published_paths(root, lease, &publication.plan.operations, publication.paths)?;
    check_policies(preflight, lease, &publication.plan.policies)?;
    state.uncertain = false;
    state.refs.as_ref().context("ref lock missing")?.verify()?;
    journal.append(Event::BeforeRef {
        branch: preflight.branch.clone(),
        old: preflight.old.to_string(),
        new: preflight.new.to_string(),
    })?;
    forward(journal, "refs", "release")?;
    state.ref_held = false;
    state.uncertain = true;
    let sig = Signature::now("forge-checkout-sync", "forge-checkout-sync@localhost")?;
    match state
        .refs
        .take()
        .context("ref lock missing")?
        .publish(&sig)?
    {
        RefPublication::Committed => (),
        RefPublication::CommitError { error, observed } => {
            bail!("ref publication uncertain: {error}; observed branch={observed:?}")
        }
    }
    sync_ref_dirs(preflight)?;
    forward(journal, "refs", "released")?;
    state.uncertain = false;
    journal.append(Event::AfterRef {
        branch: preflight.branch.clone(),
        new: preflight.new.to_string(),
    })?;
    forward(journal, "index", "release")?;
    state.index_held = false;
    state.uncertain = true;
    drop(state.index.take());
    sync_index_dir(preflight)?;
    forward(journal, "index", "released")?;
    state.uncertain = false;
    cancel()?;
    journal.append(Event::Complete)?;
    Ok(())
}

fn validate_published_paths(
    root: &Root,
    lease: &SyncLease,
    operations: &[Op],
    published: &BTreeMap<String, Option<Identity>>,
) -> Result<()> {
    let by_path: BTreeMap<&str, &Op> = operations.iter().map(|op| (op.path.as_str(), op)).collect();
    for op in operations {
        // Descendants of a final non-directory no longer have a traversable parent.
        let hidden = op.path.match_indices('/').any(|(at, _)| {
            by_path.get(&op.path[..at]).is_some_and(|parent| {
                !matches!(&parent.after, Some(Content::EmptyDirectory { .. }))
            })
        });
        if hidden {
            continue;
        }
        let observed = if matches!(&op.after, Some(Content::EmptyDirectory { .. })) {
            Some(root.observe_directory_identity(&op.path, lease)?)
        } else {
            root.observe(&op.path, lease)?.map(|value| value.identity)
        };
        ensure!(
            published.get(&op.path) == Some(&observed),
            "checkout path changed after publication: {}",
            op.path
        );
    }
    Ok(())
}
fn begin(preflight: &Preflight, snapshot: &str, journal: &mut Journal) -> Result<()> {
    journal.append(Event::Begin {
        root: preflight.source.root().display().to_string(),
        root_device: preflight.root_identity.0,
        root_inode: preflight.root_identity.1,
        common_dir: preflight.source.common_dir().display().to_string(),
        common_device: preflight.common_identity.0,
        common_inode: preflight.common_identity.1,
        index_path: preflight.index_path.display().to_string(),
        index_parent_device: preflight.index_parent.0,
        index_parent_inode: preflight.index_parent.1,
        original_index: preflight.original_index.clone(),
        recovery_oid: String::new(),
        branch: preflight.branch.clone(),
        old: preflight.old.to_string(),
        new: preflight.new.to_string(),
        target: preflight.target.clone(),
        recovery_ref: snapshot.into(),
    })
}
fn forward(journal: &mut Journal, subject: &str, phase: &str) -> Result<()> {
    let subject = subject.to_owned();
    journal.append(match phase {
        "acquire" => Event::ForwardAcquire { subject },
        "held" => Event::ForwardHeld { subject },
        "release" => Event::ForwardRelease { subject },
        "released" => Event::ForwardReleased { subject },
        _ => bail!("invalid forward guard phase"),
    })
}
fn sync_index_dir(preflight: &Preflight) -> Result<()> {
    File::open(
        preflight
            .index_path
            .parent()
            .context("missing index parent")?,
    )?
    .sync_all()?;
    Ok(())
}
fn sync_ref_dirs(preflight: &Preflight) -> Result<()> {
    sync_index_dir(preflight)?;
    for name in [&preflight.branch, &preflight.target] {
        let path = preflight.source.common_dir().join(name);
        File::open(path.parent().context("missing ref parent")?)?.sync_all()?;
    }
    Ok(())
}
fn release_held(
    preflight: &Preflight,
    journal: &mut Journal,
    index: &mut Option<IndexLock>,
    refs: &mut Option<RefLocks<'_>>,
    index_held: bool,
    ref_held: bool,
) -> Result<()> {
    if index_held {
        forward(journal, "index", "release")?;
        drop(index.take());
        sync_index_dir(preflight)?;
        forward(journal, "index", "released")?;
    }
    if ref_held {
        forward(journal, "refs", "release")?;
        drop(refs.take());
        sync_ref_dirs(preflight)?;
        forward(journal, "refs", "released")?;
    }
    Ok(())
}
fn check_policies(
    preflight: &Preflight,
    lease: &SyncLease,
    policies: &BTreeMap<String, Policy>,
) -> Result<()> {
    for (path, policy) in policies {
        ensure!(
            *policy == Policy::capture(&preflight.source, lease, path)?,
            "Git attributes or config changed at {path}"
        );
    }
    Ok(())
}
fn check_plan(preflight: &Preflight, lease: &SyncLease, plan: &Plan) -> Result<()> {
    preflight.verify()?;
    lease.verify()?;
    check_policies(preflight, lease, &plan.policies)?;
    ensure!(
        checkout_sync_inventory::inventory(&preflight.source, lease, &plan.tracked)?
            == plan.inventory,
        "checkout collision inventory changed"
    );
    Ok(())
}
fn validate_observed(
    op: &Op,
    observed: &Option<crate::checkout_sync_worktree::Observed>,
) -> Result<()> {
    match (&op.before, op.before_dir, observed) {
        (None, false, None) => (),
        (None, true, Some(value)) if value.identity.mode & libc::S_IFMT == libc::S_IFDIR => (),
        (Some(Entry::File { executable, bytes }), false, Some(value))
            if value.identity.mode & libc::S_IFMT == libc::S_IFREG
                && (value.identity.mode & 0o111 != 0) == *executable
                && &value.bytes == bytes => {}
        (Some(Entry::Symlink(bytes)), false, Some(value))
            if value.identity.mode & libc::S_IFMT == libc::S_IFLNK && &value.bytes == bytes => {}
        _ => bail!("checkout path changed during admission: {}", op.path),
    }
    Ok(())
}
fn publish_op(
    root: &Root,
    depot: &Depot,
    lease: &SyncLease,
    journal: &mut Journal,
    op: &Op,
) -> Result<()> {
    let observed = root.observe(&op.path, lease)?;
    validate_observed(op, &observed)?;
    let mut content = op.after.clone();
    if let (Some(Content::File { mode, .. }), Some(value)) = (&mut content, &observed) {
        if value.identity.mode & libc::S_IFMT == libc::S_IFREG {
            let old = value.identity.mode & 0o7777;
            *mode = if *mode & 0o111 != 0 {
                old | 0o111
            } else {
                old & !0o111
            };
        }
    }
    let expected = observed.map(|value| value.identity);
    let prepared = crate::checkout_sync_worktree::Prepared::prepare(
        root, depot, lease, journal, &op.path, expected, content,
    )?;
    prepared.publish(root, lease, journal)
}
#[cfg(test)]
#[path = "checkout_sync_forward_tests.rs"]
mod tests;
