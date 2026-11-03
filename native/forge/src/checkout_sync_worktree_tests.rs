use super::*;
#[test]
fn populated_directory_identity_is_bound_and_symlink_replacement_refuses() {
    let f = Fixture::new();
    let lease = f.lease();
    let root = Root::open(&f.repo).unwrap();
    std::fs::create_dir(f.repo.join("node")).unwrap();
    let original = root.observe("node", &lease).unwrap().unwrap().identity;
    std::fs::write(f.repo.join("node/child"), b"keep").unwrap();
    assert!(root.observe("node", &lease).is_err());
    assert_eq!(
        root.observe_directory_identity("node", &lease).unwrap(),
        original
    );
    std::fs::rename(f.repo.join("node"), f.repo.join("retained")).unwrap();
    std::os::unix::fs::symlink("retained", f.repo.join("node")).unwrap();
    assert!(root.observe_directory_identity("node", &lease).is_err());
    assert_eq!(
        std::fs::read(f.repo.join("retained/child")).unwrap(),
        b"keep"
    );
}
use crate::checkout_sync_journal::Event;
use std::os::unix::fs::{symlink, MetadataExt as _, PermissionsExt as _};

struct Fixture {
    _temp: tempfile::TempDir,
    repo: PathBuf,
    recovery: PathBuf,
    depot_name: String,
    depot_identity: (u64, u64),
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        let git = git2::Repository::init(&repo).unwrap();
        let sig = git2::Signature::now("sync-test", "sync@example.invalid").unwrap();
        let tree = git.treebuilder(None).unwrap().write().unwrap();
        let commit = {
            let tree = git.find_tree(tree).unwrap();
            git.commit(Some("refs/heads/main"), &sig, &sig, "base", &tree, &[])
                .unwrap()
        };
        git.set_head("refs/heads/main").unwrap();
        git.reference("refs/snapshots/test/recovery", commit, false, "test")
            .unwrap();
        let parent = temp.path().join("journals");
        std::fs::create_dir(&parent).unwrap();
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut journal = Journal::create(&parent).unwrap();
        let root = std::fs::canonicalize(&repo).unwrap();
        let common = git.commondir().canonicalize().unwrap();
        let rm = std::fs::metadata(&root).unwrap();
        let cm = std::fs::metadata(&common).unwrap();
        journal
            .append(Event::Begin {
                root: root.display().to_string(),
                root_device: rm.dev(),
                root_inode: rm.ino(),
                common_dir: common.display().to_string(),
                common_device: cm.dev(),
                common_inode: cm.ino(),
                index_parent_device: std::fs::metadata(git.path()).unwrap().dev(),
                index_parent_inode: std::fs::metadata(git.path()).unwrap().ino(),
                original_index: None,
                index_path: git
                    .path()
                    .canonicalize()
                    .unwrap()
                    .join("index")
                    .display()
                    .to_string(),
                recovery_oid: commit.to_string(),
                branch: "refs/heads/main".into(),
                old: commit.to_string(),
                new: commit.to_string(),
                target: "refs/remotes/origin/main".into(),
                recovery_ref: "refs/snapshots/test/recovery".into(),
            })
            .unwrap();
        let lease = SyncLease::acquire(&repo, &mut || Ok(())).unwrap();
        let root_handle = Root::open(&repo).unwrap();
        let depot = Depot::create(&root_handle, &lease, &mut journal).unwrap();
        let depot_name = depot.name.clone();
        let depot_identity = depot.identity;
        let recovery = journal.path().to_path_buf();
        drop(depot);
        drop(root_handle);
        drop(lease);
        drop(journal);
        drop(git);
        Self {
            _temp: temp,
            repo,
            recovery,
            depot_name,
            depot_identity,
        }
    }
    fn lease(&self) -> SyncLease {
        SyncLease::acquire(&self.repo, &mut || Ok(())).unwrap()
    }
}
#[test]
fn crash_after_exchange_recovers_original_binary_and_mode() {
    let f = Fixture::new();
    let path = f.repo.join("a");
    std::fs::write(&path, b"old\0binary").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o751)).unwrap();
    let lease = f.lease();
    let root = Root::open(&f.repo).unwrap();
    let depot = Depot::open(&root, &lease, &f.depot_name, f.depot_identity).unwrap();
    let (mut journal, _) = Journal::reopen(&f.recovery).unwrap();
    let old = root.observe("a", &lease).unwrap().map(|v| v.identity);
    let change = Prepared::prepare(
        &root,
        &depot,
        &lease,
        &mut journal,
        "a",
        old,
        Some(Content::File {
            bytes: b"new\0binary".to_vec(),
            mode: 0o640,
        }),
    )
    .unwrap();
    assert!(change
        .publish_inner(&root, &lease, &mut journal, |point| {
            if point == "after_rename" {
                anyhow::bail!("injected late failure")
            } else {
                Ok(())
            }
        })
        .is_err());
    assert_eq!(std::fs::read(&path).unwrap(), b"new\0binary");
    drop(change);
    drop(journal);
    drop(lease);
    drop(depot);
    drop(root);
    let mut checked_writers = false;
    crate::checkout_sync_recovery::recover(&f.recovery, &mut || {
        if f.repo.join(".git/index.lock").exists() {
            let repo = git2::Repository::open(&f.repo)?;
            assert!(
                crate::checkout_sync_index::IndexLock::acquire(&f.repo.join(".git/index")).is_err()
            );
            let mut rival = repo.transaction()?;
            assert!(rival.lock_ref("refs/heads/main").is_err());
            checked_writers = true;
        }
        Ok(())
    })
    .unwrap();
    assert!(checked_writers);
    assert_eq!(std::fs::read(&path).unwrap(), b"old\0binary");
    assert_eq!(std::fs::metadata(&path).unwrap().mode() & 0o777, 0o751);
}
#[test]
fn concurrent_edit_after_publication_is_never_overwritten_by_restart() {
    let f = Fixture::new();
    let path = f.repo.join("a");
    std::fs::write(&path, b"old").unwrap();
    let lease = f.lease();
    let root = Root::open(&f.repo).unwrap();
    let depot = Depot::open(&root, &lease, &f.depot_name, f.depot_identity).unwrap();
    let (mut journal, _) = Journal::reopen(&f.recovery).unwrap();
    let expected = root.observe("a", &lease).unwrap().map(|v| v.identity);
    let change = Prepared::prepare(
        &root,
        &depot,
        &lease,
        &mut journal,
        "a",
        expected,
        Some(Content::File {
            bytes: b"ours".to_vec(),
            mode: 0o600,
        }),
    )
    .unwrap();
    change.publish(&root, &lease, &mut journal).unwrap();
    std::fs::write(&path, b"external").unwrap();
    drop(change);
    drop(journal);
    drop(lease);
    drop(depot);
    drop(root);
    assert!(crate::checkout_sync_recovery::recover(&f.recovery, &mut || Ok(())).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), b"external");
    assert!(f.recovery.exists());
}
#[test]
fn crash_before_rename_preserves_original_and_restarts_idempotently() {
    let f = Fixture::new();
    let path = f.repo.join("a");
    std::fs::write(&path, b"old").unwrap();
    let lease = f.lease();
    let root = Root::open(&f.repo).unwrap();
    let depot = Depot::open(&root, &lease, &f.depot_name, f.depot_identity).unwrap();
    let (mut journal, _) = Journal::reopen(&f.recovery).unwrap();
    let expected = root.observe("a", &lease).unwrap().map(|v| v.identity);
    let change = Prepared::prepare(
        &root,
        &depot,
        &lease,
        &mut journal,
        "a",
        expected,
        Some(Content::File {
            bytes: b"new".to_vec(),
            mode: 0o600,
        }),
    )
    .unwrap();
    assert!(change
        .publish_inner(&root, &lease, &mut journal, |point| {
            if point == "before_rename" {
                anyhow::bail!("injected crash")
            } else {
                Ok(())
            }
        })
        .is_err());
    drop(change);
    drop(journal);
    drop(lease);
    drop(depot);
    drop(root);
    crate::checkout_sync_recovery::recover(&f.recovery, &mut || Ok(())).unwrap();
    crate::checkout_sync_recovery::recover(&f.recovery, &mut || Ok(())).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"old");
}
#[test]
fn symlink_and_empty_directory_transitions_restore_without_following_links() {
    let f = Fixture::new();
    let outside = f._temp.path().join("sentinel");
    std::fs::write(&outside, b"safe").unwrap();
    symlink(&outside, f.repo.join("link")).unwrap();
    std::fs::create_dir(f.repo.join("empty")).unwrap();
    let lease = f.lease();
    let root = Root::open(&f.repo).unwrap();
    let depot = Depot::open(&root, &lease, &f.depot_name, f.depot_identity).unwrap();
    let (mut journal, _) = Journal::reopen(&f.recovery).unwrap();
    let mut changes = Vec::new();
    for (path, content) in [
        (
            "link",
            Content::File {
                bytes: b"file".to_vec(),
                mode: 0o600,
            },
        ),
        ("empty", Content::Symlink(b"target".to_vec())),
    ] {
        let expected = root.observe(path, &lease).unwrap().map(|v| v.identity);
        let change = Prepared::prepare(
            &root,
            &depot,
            &lease,
            &mut journal,
            path,
            expected,
            Some(content),
        )
        .unwrap();
        changes.push(change);
    }
    for change in &changes {
        change.publish(&root, &lease, &mut journal).unwrap();
    }
    drop(changes);
    drop(journal);
    drop(lease);
    drop(depot);
    drop(root);
    crate::checkout_sync_recovery::recover(&f.recovery, &mut || Ok(())).unwrap();
    assert_eq!(std::fs::read_link(f.repo.join("link")).unwrap(), outside);
    assert!(f.repo.join("empty").is_dir());
    assert_eq!(std::fs::read(&outside).unwrap(), b"safe");
}
#[test]
fn interrupted_rollback_with_unresolved_guards_preserves_restored_bytes() {
    let f = Fixture::new();
    let path = f.repo.join("a");
    std::fs::write(&path, b"original").unwrap();
    let lease = f.lease();
    let root = Root::open(&f.repo).unwrap();
    let depot = Depot::open(&root, &lease, &f.depot_name, f.depot_identity).unwrap();
    let (mut journal, _) = Journal::reopen(&f.recovery).unwrap();
    let old = root.observe("a", &lease).unwrap().map(|v| v.identity);
    let change = Prepared::prepare(
        &root,
        &depot,
        &lease,
        &mut journal,
        "a",
        old,
        Some(Content::File {
            bytes: b"new".to_vec(),
            mode: 0o600,
        }),
    )
    .unwrap();
    change.publish(&root, &lease, &mut journal).unwrap();
    for event in [
        Event::GuardAcquire {
            subject: "refs".into(),
        },
        Event::GuardHeld {
            subject: "refs".into(),
        },
        Event::GuardAcquire {
            subject: "index".into(),
        },
        Event::GuardHeld {
            subject: "index".into(),
        },
    ] {
        journal.append(event).unwrap();
    }
    journal
        .append(Event::BeforeRollback {
            subject: "path:a".into(),
        })
        .unwrap();
    rename(
        &change.depot.dir,
        &change.slot,
        &change.parent,
        &change.name,
        libc::RENAME_EXCHANGE,
    )
    .unwrap();
    sync_dir(&change.parent).unwrap();
    sync_dir(&change.depot.dir).unwrap();
    drop(change);
    drop(journal);
    drop(lease);
    drop(depot);
    drop(root);
    assert!(crate::checkout_sync_recovery::recover(&f.recovery, &mut || Ok(())).is_err());
    assert_eq!(std::fs::read(path).unwrap(), b"original");
}
#[test]
fn bounded_many_path_recovery_restores_each_original() {
    let f = Fixture::new();
    let lease = f.lease();
    let root = Root::open(&f.repo).unwrap();
    let depot = Depot::open(&root, &lease, &f.depot_name, f.depot_identity).unwrap();
    let (mut journal, _) = Journal::reopen(&f.recovery).unwrap();
    for i in 0..64 {
        std::fs::write(f.repo.join(format!("file-{i}")), format!("old-{i}")).unwrap();
    }
    let mut changes = Vec::new();
    for i in 0..64 {
        let name = format!("file-{i}");
        let expected = root.observe(&name, &lease).unwrap().map(|v| v.identity);
        changes.push(
            Prepared::prepare(
                &root,
                &depot,
                &lease,
                &mut journal,
                &name,
                expected,
                Some(Content::File {
                    bytes: format!("new-{i}").into_bytes(),
                    mode: 0o600,
                }),
            )
            .unwrap(),
        );
    }
    for change in &changes {
        change.publish(&root, &lease, &mut journal).unwrap();
    }
    drop(changes);
    drop(journal);
    drop(lease);
    drop(depot);
    drop(root);
    crate::checkout_sync_recovery::recover(&f.recovery, &mut || Ok(())).unwrap();
    for i in 0..64 {
        assert_eq!(
            std::fs::read(f.repo.join(format!("file-{i}"))).unwrap(),
            format!("old-{i}").as_bytes()
        );
    }
}
#[test]
fn crash_during_staging_preserves_unknown_slot_and_reports_uncertainty() {
    let f = Fixture::new();
    let path = f.repo.join("a");
    std::fs::write(&path, b"old").unwrap();
    let lease = f.lease();
    let root = Root::open(&f.repo).unwrap();
    let depot = Depot::open(&root, &lease, &f.depot_name, f.depot_identity).unwrap();
    let (mut journal, _) = Journal::reopen(&f.recovery).unwrap();
    let old = root.observe("a", &lease).unwrap().unwrap();
    let (parent, _) = root.parent("a").unwrap();
    let m = parent.dir_metadata().unwrap();
    let slot = "checkout-sync-slot-crash";
    journal.save_blob(slot, &old.bytes).unwrap();
    journal
        .append(Event::BeforeStage {
            path: "a".into(),
            parent_device: m.dev(),
            parent_inode: m.ino(),
            slot: slot.into(),
            original: Some(old.identity),
            original_blob: Some(slot.into()),
            has_staged: true,
        })
        .unwrap();
    std::fs::write(f.repo.join(&f.depot_name).join(slot), b"partial").unwrap();
    drop(journal);
    drop(lease);
    drop(depot);
    drop(root);
    let error = crate::checkout_sync_recovery::recover(&f.recovery, &mut || Ok(())).unwrap_err();
    assert!(format!("{error:#}").contains("unrecorded staging/depot inode"));
    assert_eq!(std::fs::read(path).unwrap(), b"old");
    assert_eq!(
        std::fs::read(f.repo.join(&f.depot_name).join(slot)).unwrap(),
        b"partial"
    );
}
#[test]
fn addition_and_deletion_restore_with_no_replace_and_retain_slots() {
    let f = Fixture::new();
    std::fs::write(f.repo.join("deleted"), b"old").unwrap();
    let lease = f.lease();
    let root = Root::open(&f.repo).unwrap();
    let depot = Depot::open(&root, &lease, &f.depot_name, f.depot_identity).unwrap();
    let (mut journal, _) = Journal::reopen(&f.recovery).unwrap();
    let original = root.observe("deleted", &lease).unwrap().map(|v| v.identity);
    let added = Prepared::prepare(
        &root,
        &depot,
        &lease,
        &mut journal,
        "added",
        None,
        Some(Content::File {
            bytes: b"new".to_vec(),
            mode: 0o600,
        }),
    )
    .unwrap();
    let removed = Prepared::prepare(
        &root,
        &depot,
        &lease,
        &mut journal,
        "deleted",
        original,
        None,
    )
    .unwrap();
    added.publish(&root, &lease, &mut journal).unwrap();
    removed.publish(&root, &lease, &mut journal).unwrap();
    drop(added);
    drop(removed);
    drop(journal);
    drop(lease);
    drop(depot);
    drop(root);
    let outcome = crate::checkout_sync_recovery::recover(&f.recovery, &mut || Ok(())).unwrap();
    assert!(matches!(
        outcome,
        crate::checkout_sync_recovery::RecoveryOutcome::Recovered { .. }
    ));
    assert!(!f.repo.join("added").exists());
    assert_eq!(std::fs::read(f.repo.join("deleted")).unwrap(), b"old");
}
#[test]
fn directory_to_file_transition_restores_parent_before_child() {
    let f = Fixture::new();
    std::fs::create_dir(f.repo.join("dir")).unwrap();
    std::fs::write(f.repo.join("dir/child"), b"child").unwrap();
    let lease = f.lease();
    let root = Root::open(&f.repo).unwrap();
    let depot = Depot::open(&root, &lease, &f.depot_name, f.depot_identity).unwrap();
    let (mut journal, _) = Journal::reopen(&f.recovery).unwrap();
    let child = root
        .observe("dir/child", &lease)
        .unwrap()
        .map(|v| v.identity);
    let child = Prepared::prepare(
        &root,
        &depot,
        &lease,
        &mut journal,
        "dir/child",
        child,
        None,
    )
    .unwrap();
    child.publish(&root, &lease, &mut journal).unwrap();
    let dir = root.observe("dir", &lease).unwrap().map(|v| v.identity);
    let dir = Prepared::prepare(
        &root,
        &depot,
        &lease,
        &mut journal,
        "dir",
        dir,
        Some(Content::File {
            bytes: b"replacement".to_vec(),
            mode: 0o600,
        }),
    )
    .unwrap();
    dir.publish(&root, &lease, &mut journal).unwrap();
    drop(dir);
    drop(child);
    drop(journal);
    drop(lease);
    drop(depot);
    drop(root);
    crate::checkout_sync_recovery::recover(&f.recovery, &mut || Ok(())).unwrap();
    assert_eq!(std::fs::read(f.repo.join("dir/child")).unwrap(), b"child");
}
#[test]
fn file_to_directory_transition_recovers_child_before_parent() {
    let f = Fixture::new();
    std::fs::write(f.repo.join("node"), b"original").unwrap();
    let lease = f.lease();
    let root = Root::open(&f.repo).unwrap();
    let depot = Depot::open(&root, &lease, &f.depot_name, f.depot_identity).unwrap();
    let (mut journal, _) = Journal::reopen(&f.recovery).unwrap();
    let old = root.observe("node", &lease).unwrap().map(|v| v.identity);
    let parent = Prepared::prepare(
        &root,
        &depot,
        &lease,
        &mut journal,
        "node",
        old,
        Some(Content::EmptyDirectory { mode: 0o755 }),
    )
    .unwrap();
    parent.publish(&root, &lease, &mut journal).unwrap();
    let child = Prepared::prepare(
        &root,
        &depot,
        &lease,
        &mut journal,
        "node/child",
        None,
        Some(Content::File {
            bytes: b"child".to_vec(),
            mode: 0o600,
        }),
    )
    .unwrap();
    child.publish(&root, &lease, &mut journal).unwrap();
    drop(child);
    drop(parent);
    drop(journal);
    drop(depot);
    drop(lease);
    drop(root);
    crate::checkout_sync_recovery::recover(&f.recovery, &mut || Ok(())).unwrap();
    assert_eq!(std::fs::read(f.repo.join("node")).unwrap(), b"original");
}
