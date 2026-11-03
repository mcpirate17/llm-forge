use super::*;
use git2::{Repository, Signature};
use std::{fs, os::unix::fs::PermissionsExt, path::Path};

fn fixture() -> (tempfile::TempDir, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let recovery = tempfile::tempdir().unwrap();
    fs::set_permissions(recovery.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let repo = Repository::init(dir.path()).unwrap();
    let sig = Signature::now("test", "test@example.invalid").unwrap();
    fs::write(dir.path().join("a"), b"one\ntwo\nthree\nfour\nfive\n").unwrap();
    fs::write(dir.path().join("b"), b"old\n").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("a")).unwrap();
    index.add_path(Path::new("b")).unwrap();
    index.write().unwrap();
    let tree_oid = index.write_tree().unwrap();
    let tree = repo.find_tree(tree_oid).unwrap();
    let old = repo
        .commit(Some("refs/heads/main"), &sig, &sig, "base", &tree, &[])
        .unwrap();
    repo.set_head("refs/heads/main").unwrap();
    let mut builder = repo.treebuilder(Some(&tree)).unwrap();
    let a = repo.blob(b"one\ntwo\nTHREE\nfour\nfive\n").unwrap();
    let b = repo.blob(b"new\n").unwrap();
    builder.insert("a", a, 0o100644).unwrap();
    builder.insert("b", b, 0o100644).unwrap();
    let next_tree = repo.find_tree(builder.write().unwrap()).unwrap();
    let parent = repo.find_commit(old).unwrap();
    let new = repo
        .commit(None, &sig, &sig, "incoming", &next_tree, &[&parent])
        .unwrap();
    repo.reference("refs/remotes/origin/main", new, false, "test")
        .unwrap();
    drop(builder);
    drop(next_tree);
    drop(parent);
    drop(tree);
    drop(index);
    drop(repo);
    (dir, recovery)
}
#[test]
fn success_preserves_staged_and_unstaged_layers() {
    let (dir, recovery) = fixture();
    let repo = Repository::open(dir.path()).unwrap();
    fs::write(dir.path().join("a"), b"ONE\ntwo\nthree\nfour\nfive\n").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("a")).unwrap();
    index.write().unwrap();
    fs::write(dir.path().join("a"), b"ONE\ntwo\nthree\nfour\nFIVE\n").unwrap();
    drop(index);
    drop(repo);
    let bound = Preflight::open(dir.path(), "refs/remotes/origin/main").unwrap();
    assert!(matches!(
        execute(&bound, recovery.path(), &mut || Ok(())).unwrap(),
        Outcome::Published { .. }
    ));
    assert_eq!(
        fs::read(dir.path().join("a")).unwrap(),
        b"ONE\ntwo\nTHREE\nfour\nFIVE\n"
    );
    let repo = Repository::open(dir.path()).unwrap();
    let index = repo.index().unwrap();
    let entry = index.get_path(Path::new("a"), 0).unwrap();
    assert_eq!(
        repo.find_blob(entry.id).unwrap().content(),
        b"ONE\ntwo\nTHREE\nfour\nfive\n"
    );
    assert_eq!(fs::read(dir.path().join("b")).unwrap(), b"new\n");
}
#[test]
fn late_failure_restores_published_paths_and_retains_snapshot() {
    let (dir, recovery) = fixture();
    let bound = Preflight::open(dir.path(), "refs/remotes/origin/main").unwrap();
    let old = bound.old;
    let mut injected = false;
    let err = execute(&bound, recovery.path(), &mut || {
        if fs::read(dir.path().join("a")).ok().as_deref() == Some(b"one\ntwo\nTHREE\nfour\nfive\n")
        {
            injected = true;
            anyhow::bail!("injected late failure");
        }
        Ok(())
    })
    .unwrap_err();
    assert!(injected, "fault must occur after a path was published");
    assert!(format!("{err:#}").contains("injected late failure"));
    assert!(
        err.to_string().contains("conditional recovery completed"),
        "{err:#}"
    );
    assert_eq!(
        fs::read(dir.path().join("a")).unwrap(),
        b"one\ntwo\nthree\nfour\nfive\n"
    );
    assert_eq!(fs::read(dir.path().join("b")).unwrap(), b"old\n");
    assert_eq!(
        Repository::open(dir.path())
            .unwrap()
            .head()
            .unwrap()
            .target(),
        Some(old)
    );
}
#[test]
fn changed_attributes_refuse_before_worktree_publication() {
    let (dir, recovery) = fixture();
    let repo = Repository::open(dir.path()).unwrap();
    let target = repo
        .find_reference("refs/remotes/origin/main")
        .unwrap()
        .target()
        .unwrap();
    let parent = repo.find_commit(target).unwrap();
    let tree = parent.tree().unwrap();
    let mut builder = repo.treebuilder(Some(&tree)).unwrap();
    let attrs = repo.blob(b"*.txt text eol=crlf\n").unwrap();
    builder.insert(".gitattributes", attrs, 0o100644).unwrap();
    let next_tree = repo.find_tree(builder.write().unwrap()).unwrap();
    let sig = Signature::now("test", "test@example.invalid").unwrap();
    let changed = repo
        .commit(None, &sig, &sig, "attrs", &next_tree, &[&parent])
        .unwrap();
    repo.reference("refs/remotes/origin/main", changed, true, "test")
        .unwrap();
    drop(next_tree);
    drop(builder);
    drop(tree);
    drop(parent);
    drop(repo);
    let bound = Preflight::open(dir.path(), "refs/remotes/origin/main").unwrap();
    let error = execute(&bound, recovery.path(), &mut || Ok(())).unwrap_err();
    assert!(format!("{error:#}").contains(".gitattributes"));
    assert!(!dir.path().join(".gitattributes").exists());
    assert_eq!(fs::read(dir.path().join("b")).unwrap(), b"old\n");
}

fn write_foreign_index(root: &Path) {
    fs::write(root.join("foreign"), b"foreign staged bytes\n").unwrap();
    let repo = Repository::open(root).unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("foreign")).unwrap();
    index.write().unwrap();
}

#[test]
fn intervening_index_writer_prevents_ref_publication() {
    let (dir, recovery) = fixture();
    let bound = Preflight::open(dir.path(), "refs/remotes/origin/main").unwrap();
    let mut injected = false;
    let err = execute(&bound, recovery.path(), &mut || {
        if !injected
            && !dir.path().join(".git/index.lock").exists()
            && fs::read(dir.path().join("b")).ok().as_deref() == Some(b"new\n")
        {
            write_foreign_index(dir.path());
            injected = true;
        }
        Ok(())
    })
    .unwrap_err();
    assert!(injected, "writer must run in the index rename gap");
    assert!(format!("{err:#}").contains("index changed after publication"));
    let repo = Repository::open(dir.path()).unwrap();
    assert_eq!(repo.head().unwrap().target(), Some(bound.old));
    assert!(repo
        .index()
        .unwrap()
        .get_path(Path::new("foreign"), 0)
        .is_some());
}

#[test]
fn foreign_index_after_ref_commit_blocks_any_ref_rollback() {
    let (dir, recovery) = fixture();
    let bound = Preflight::open(dir.path(), "refs/remotes/origin/main").unwrap();
    let mut injected = false;
    let err = execute(&bound, recovery.path(), &mut || {
        let repo = Repository::open(dir.path())?;
        if !injected
            && !dir.path().join(".git/index.lock").exists()
            && repo.head()?.target() == Some(bound.new)
        {
            write_foreign_index(dir.path());
            injected = true;
            anyhow::bail!("injected failure after foreign index write");
        }
        Ok(())
    })
    .unwrap_err();
    assert!(injected, "writer must run after ref publication");
    assert!(format!("{err:#}").contains("injected failure after foreign index write"));
    assert!(
        format!("{err:#}").contains("index changed before recovery"),
        "{err:#}"
    );
    let repo = Repository::open(dir.path()).unwrap();
    assert_eq!(repo.head().unwrap().target(), Some(bound.new));
    assert!(repo
        .index()
        .unwrap()
        .get_path(Path::new("foreign"), 0)
        .is_some());
}

#[test]
fn intervening_path_writer_prevents_ref_publication() {
    let (dir, recovery) = fixture();
    let bound = Preflight::open(dir.path(), "refs/remotes/origin/main").unwrap();
    let mut injected = false;
    let err = execute(&bound, recovery.path(), &mut || {
        if !injected
            && !dir.path().join(".git/index.lock").exists()
            && fs::read(dir.path().join("b")).ok().as_deref() == Some(b"new\n")
        {
            fs::write(dir.path().join("b"), b"foreign worktree\n")?;
            injected = true;
        }
        Ok(())
    })
    .unwrap_err();
    assert!(injected, "writer must run in the index rename gap");
    assert!(format!("{err:#}").contains("checkout path changed after publication"));
    assert_eq!(
        fs::read(dir.path().join("b")).unwrap(),
        b"foreign worktree\n"
    );
    assert_eq!(
        Repository::open(dir.path())
            .unwrap()
            .head()
            .unwrap()
            .target(),
        Some(bound.old)
    );
}

#[test]
fn publication_validates_children_of_new_directories() {
    let (dir, recovery) = fixture();
    let repo = Repository::open(dir.path()).unwrap();
    let target = repo
        .find_reference("refs/remotes/origin/main")
        .unwrap()
        .target()
        .unwrap();
    let parent = repo.find_commit(target).unwrap();
    let root = parent.tree().unwrap();
    let mut nested = repo.treebuilder(None).unwrap();
    let blob = repo.blob(b"nested bytes\n").unwrap();
    nested.insert("child", blob, 0o100644).unwrap();
    let nested_oid = nested.write().unwrap();
    let mut builder = repo.treebuilder(Some(&root)).unwrap();
    builder.insert("folder", nested_oid, 0o040000).unwrap();
    let tree = repo.find_tree(builder.write().unwrap()).unwrap();
    let sig = Signature::now("test", "test@example.invalid").unwrap();
    let next = repo
        .commit(None, &sig, &sig, "nested", &tree, &[&parent])
        .unwrap();
    repo.reference("refs/remotes/origin/main", next, true, "test")
        .unwrap();
    drop(tree);
    drop(builder);
    drop(nested);
    drop(root);
    drop(parent);
    drop(repo);
    let bound = Preflight::open(dir.path(), "refs/remotes/origin/main").unwrap();
    assert!(matches!(
        execute(&bound, recovery.path(), &mut || Ok(())).unwrap(),
        Outcome::Published { .. }
    ));
    assert_eq!(
        fs::read(dir.path().join("folder/child")).unwrap(),
        b"nested bytes\n"
    );
}

#[test]
fn recovery_parent_overlap_is_rejected_before_journal_creation() {
    use std::os::unix::fs::symlink;
    let (dir, recovery) = fixture();
    let bound = Preflight::open(dir.path(), "refs/remotes/origin/main").unwrap();
    let nested = dir.path().join("private-recovery");
    fs::create_dir(&nested).unwrap();
    fs::set_permissions(&nested, fs::Permissions::from_mode(0o700)).unwrap();
    let git_nested = dir.path().join(".git/private-recovery");
    fs::create_dir(&git_nested).unwrap();
    fs::set_permissions(&git_nested, fs::Permissions::from_mode(0o700)).unwrap();
    let alias = recovery.path().join("checkout-alias");
    symlink(&nested, &alias).unwrap();
    for parent in [&nested, &git_nested, &alias] {
        let error = execute(&bound, parent, &mut || Ok(())).unwrap_err();
        assert!(format!("{error:#}").contains("recovery parent overlaps"));
    }
    assert_eq!(fs::read_dir(&nested).unwrap().count(), 0);
    assert_eq!(fs::read_dir(&git_nested).unwrap().count(), 0);
    assert_eq!(
        Repository::open(dir.path())
            .unwrap()
            .head()
            .unwrap()
            .target(),
        Some(bound.old)
    );
    assert_eq!(fs::read(dir.path().join("b")).unwrap(), b"old\n");
}

#[test]
fn complete_journal_is_not_reported_as_rollback() {
    let error = anyhow::anyhow!("injected failure after complete record write");
    let result = recovery_result_error(
        Path::new("/recovery"),
        &error,
        RecoveryOutcome::JournalMarkedComplete,
    );
    let message = result.to_string();
    assert!(message.contains("journal marks publication complete"));
    assert!(message.contains("no rollback was performed"));
    assert!(!message.contains("conditional recovery completed"));
    let result = recovery_result_error(
        Path::new("/recovery"),
        &error,
        RecoveryOutcome::JournalMarkedRecovered,
    );
    assert!(result
        .to_string()
        .contains("conditional recovery completed"));
}

#[test]
fn absent_original_index_is_restored_after_post_ref_failure() {
    let (dir, recovery) = fixture();
    let repo = Repository::open(dir.path()).unwrap();
    let old = repo.head().unwrap().target().unwrap();
    let parent = repo.find_commit(old).unwrap();
    let tree = parent.tree().unwrap();
    let sig = Signature::now("test", "test@example.invalid").unwrap();
    let new = repo
        .commit(None, &sig, &sig, "unchanged tree", &tree, &[&parent])
        .unwrap();
    repo.reference("refs/remotes/origin/main", new, true, "test")
        .unwrap();
    drop(tree);
    drop(parent);
    drop(repo);
    fs::remove_file(dir.path().join(".git/index")).unwrap();
    let bound = Preflight::open(dir.path(), "refs/remotes/origin/main").unwrap();
    assert!(bound.original_index.is_none());
    let mut injected = false;
    let error = execute(&bound, recovery.path(), &mut || {
        if !injected
            && !dir.path().join(".git/index.lock").exists()
            && dir.path().join(".git/index").exists()
            && Repository::open(dir.path())?.head()?.target() == Some(new)
        {
            injected = true;
            anyhow::bail!("injected after ref commit");
        }
        Ok(())
    })
    .unwrap_err();
    assert!(injected, "fault must follow ref publication");
    assert!(
        format!("{error:#}").contains("conditional recovery completed"),
        "{error:#}"
    );
    assert!(!dir.path().join(".git/index").exists());
    assert_eq!(
        Repository::open(dir.path())
            .unwrap()
            .head()
            .unwrap()
            .target(),
        Some(old)
    );
    assert_eq!(fs::read(dir.path().join("b")).unwrap(), b"old\n");
}
