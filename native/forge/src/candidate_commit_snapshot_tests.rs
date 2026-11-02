use super::*;
use git2::Repository;
use std::os::unix::fs::{symlink, PermissionsExt};

struct Fixture {
    _dir: tempfile::TempDir,
    repo: Repository,
    root: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        let repo = Repository::init(&root).unwrap();
        fs::write(root.join("both"), "base").unwrap();
        fs::write(root.join("removed"), "base").unwrap();
        fs::write(root.join(".gitignore"), "ignored\n").unwrap();
        let mut index = repo.index().unwrap();
        index
            .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let signature = Signature::now("Test", "test@example.com").unwrap();
        repo.commit(Some("HEAD"), &signature, &signature, "base", &tree, &[])
            .unwrap();
        drop(tree);
        Self {
            _dir: dir,
            repo,
            root,
        }
    }
    fn source(&self) -> Source {
        Source::open(&self.root).unwrap()
    }
    fn stage(&self, path: &str) {
        let mut index = self.repo.index().unwrap();
        index.add_path(Path::new(path)).unwrap();
        index.write().unwrap();
    }
    fn content(&self, commit: &git2::Commit<'_>, path: &str) -> Option<Vec<u8>> {
        let tree = commit.tree().unwrap();
        let entry = tree.get_path(Path::new(path)).ok()?;
        Some(self.repo.find_blob(entry.id()).unwrap().content().to_vec())
    }
}
#[test]
fn preserves_distinct_index_and_worktree_without_touching_index() {
    let f = Fixture::new();
    fs::write(f.root.join("both"), "staged").unwrap();
    f.stage("both");
    fs::write(f.root.join("both"), "unstaged").unwrap();
    fs::remove_file(f.root.join("removed")).unwrap();
    fs::write(f.root.join("new"), "untracked").unwrap();
    fs::write(f.root.join("ignored"), "ignore me").unwrap();
    let index_before = fs::read(f.repo.path().join("index")).unwrap();
    let recovery = create(&f.source(), Limits::default()).unwrap();
    assert_eq!(fs::read(f.repo.path().join("index")).unwrap(), index_before);
    let snapshot = f
        .repo
        .find_reference(&recovery.reference)
        .unwrap()
        .peel_to_commit()
        .unwrap();
    assert_eq!(snapshot.parent_count(), 2);
    let staged = snapshot.parent(0).unwrap();
    let work = snapshot.parent(1).unwrap();
    assert_eq!(staged.tree_id(), recovery.staged_tree);
    assert_eq!(work.tree_id(), recovery.worktree_tree);
    assert_eq!(f.content(&staged, "both").unwrap(), b"staged");
    assert_eq!(f.content(&work, "both").unwrap(), b"unstaged");
    assert_eq!(f.content(&staged, "removed").unwrap(), b"base");
    assert!(f.content(&work, "removed").is_none());
    assert_eq!(f.content(&work, "new").unwrap(), b"untracked");
    assert!(f.content(&work, "ignored").is_none());
    assert_eq!(
        staged.parent_id(0).unwrap(),
        f.repo.head().unwrap().target().unwrap()
    );
}
#[test]
fn staged_deletion_and_recreated_worktree_are_both_recoverable() {
    let f = Fixture::new();
    fs::remove_file(f.root.join("removed")).unwrap();
    let mut index = f.repo.index().unwrap();
    index.remove_path(Path::new("removed")).unwrap();
    index.write().unwrap();
    fs::write(f.root.join("removed"), "recreated").unwrap();
    let recovery = create(&f.source(), Limits::default()).unwrap();
    let snapshot = f
        .repo
        .find_reference(&recovery.reference)
        .unwrap()
        .peel_to_commit()
        .unwrap();
    assert!(f.content(&snapshot.parent(0).unwrap(), "removed").is_none());
    assert_eq!(
        f.content(&snapshot.parent(1).unwrap(), "removed").unwrap(),
        b"recreated"
    );
}
#[test]
fn executable_and_symlink_are_recorded_without_following_the_link() {
    let f = Fixture::new();
    fs::write(f.root.join("exec"), "run").unwrap();
    fs::set_permissions(f.root.join("exec"), fs::Permissions::from_mode(0o755)).unwrap();
    symlink("both", f.root.join("link")).unwrap();
    let recovery = create(&f.source(), Limits::default()).unwrap();
    let tree = f.repo.find_tree(recovery.worktree_tree).unwrap();
    assert_eq!(
        tree.get_path(Path::new("exec")).unwrap().filemode(),
        0o100755
    );
    let link = tree.get_path(Path::new("link")).unwrap();
    assert_eq!(link.filemode(), 0o120000);
    assert_eq!(f.repo.find_blob(link.id()).unwrap().content(), b"both");
}
#[test]
fn admission_limits_fail_without_publishing_a_ref() {
    let f = Fixture::new();
    fs::write(f.root.join("large"), "12345").unwrap();
    let tiny = Limits {
        max_entries: 100,
        max_file_bytes: 4,
        max_total_bytes: 100,
    };
    assert!(create(&f.source(), tiny).is_err());
    assert_eq!(
        f.repo.references_glob("refs/snapshots/*").unwrap().count(),
        0
    );
    let count = Limits {
        max_entries: 1,
        ..Limits::default()
    };
    assert!(create(&f.source(), count).is_err());
    let total = Limits {
        max_entries: 100,
        max_file_bytes: 100,
        max_total_bytes: 3,
    };
    assert!(create(&f.source(), total).is_err());
}
#[test]
fn detects_drift_and_refuses_special_files() {
    let f = Fixture::new();
    let source = f.source();
    let mut c = Capture {
        source: &source,
        limits: Limits::default(),
        tracked: BTreeSet::new(),
        visited: 0,
        total: 0,
        stamps: BTreeMap::new(),
        work: Index::new().unwrap(),
    };
    c.walk(source.root(), "").unwrap();
    fs::write(f.root.join("both"), "changed after capture").unwrap();
    assert!(c.verify().is_err());
    use std::ffi::CString;
    let fifo = CString::new(f.root.join("fifo").as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    assert!(create(&source, Limits::default()).is_err());
}
#[test]
fn each_snapshot_publishes_a_distinct_ref() {
    let f = Fixture::new();
    let a = create(&f.source(), Limits::default()).unwrap();
    let b = create(&f.source(), Limits::default()).unwrap();
    assert_ne!(a.reference, b.reference);
    assert!(f.repo.find_reference(&a.reference).is_ok());
    assert!(f.repo.find_reference(&b.reference).is_ok());
}

#[test]
fn nested_git_and_index_flags_fail_closed() {
    let f = Fixture::new();
    fs::create_dir(f.root.join("nested")).unwrap();
    fs::write(f.root.join("nested/.git"), "gitdir: elsewhere").unwrap();
    assert!(create(&f.source(), Limits::default()).is_err());
    fs::remove_dir_all(f.root.join("nested")).unwrap();
    let mut index = f.repo.index().unwrap();
    let mut entry = index.get_path(Path::new("both"), 0).unwrap();
    entry.flags |= 0x8000; // assume-unchanged
    index.add(&entry).unwrap();
    index.write().unwrap();
    assert!(create(&f.source(), Limits::default()).is_err());
}

#[test]
fn escaping_symlink_refuses_publication() {
    let f = Fixture::new();
    symlink("../outside", f.root.join("link")).unwrap();
    assert!(create(&f.source(), Limits::default()).is_err());
    assert_eq!(
        f.repo.references_glob("refs/snapshots/*").unwrap().count(),
        0
    );
}
#[test]
fn ignored_directory_is_pruned_before_visiting_its_children() {
    let f = Fixture::new();
    fs::write(f.root.join(".gitignore"), "ignored\ncache/\n").unwrap();
    fs::create_dir(f.root.join("cache")).unwrap();
    for number in 0..128 {
        fs::write(f.root.join("cache").join(format!("entry-{number}")), "x").unwrap();
    }
    let limits = Limits {
        max_entries: 8,
        ..Limits::default()
    };
    let recovery = create(&f.source(), limits).unwrap();
    let tree = f.repo.find_tree(recovery.worktree_tree).unwrap();
    assert!(tree.get_path(Path::new("cache/entry-0")).is_err());
    let source = f.source();
    let mut capture = Capture {
        source: &source,
        limits,
        tracked: BTreeSet::from([".gitignore".into(), "both".into(), "removed".into()]),
        visited: 0,
        total: 0,
        stamps: BTreeMap::new(),
        work: Index::new().unwrap(),
    };
    capture.walk(source.root(), "").unwrap();
    fs::write(f.root.join("cache/entry-0"), "volatile ignored content").unwrap();
    assert!(capture.verify().is_ok());
}
#[test]
fn tracked_files_inside_ignored_parent_remain_recoverable() {
    let f = Fixture::new();
    fs::create_dir(f.root.join("cache")).unwrap();
    fs::write(f.root.join("cache/from-head"), "head").unwrap();
    f.stage("cache/from-head");
    let mut index = f.repo.index().unwrap();
    let tree = f.repo.find_tree(index.write_tree().unwrap()).unwrap();
    let parent = f.repo.head().unwrap().peel_to_commit().unwrap();
    let sig = Signature::now("Test", "test@example.com").unwrap();
    f.repo
        .commit(
            Some("HEAD"),
            &sig,
            &sig,
            "tracked baseline",
            &tree,
            &[&parent],
        )
        .unwrap();
    drop(tree);
    drop(parent);
    fs::write(f.root.join(".gitignore"), "ignored\ncache/\n").unwrap();
    index.remove_path(Path::new("cache/from-head")).unwrap();
    index.write().unwrap();
    fs::write(f.root.join("cache/from-head"), "worktree head path").unwrap();
    fs::write(f.root.join("cache/from-index"), "worktree index path").unwrap();
    index.add_path(Path::new("cache/from-index")).unwrap();
    index.write().unwrap();
    fs::write(f.root.join("cache/excluded"), "ignored").unwrap();
    let recovery = create(&f.source(), Limits::default()).unwrap();
    let staged = f.repo.find_tree(recovery.staged_tree).unwrap();
    let work = f.repo.find_tree(recovery.worktree_tree).unwrap();
    assert!(staged.get_path(Path::new("cache/from-head")).is_err());
    assert!(staged.get_path(Path::new("cache/from-index")).is_ok());
    assert!(work.get_path(Path::new("cache/from-head")).is_ok());
    assert!(work.get_path(Path::new("cache/from-index")).is_ok());
    assert!(work.get_path(Path::new("cache/excluded")).is_err());
}
fn unborn_repo() -> (tempfile::TempDir, Repository, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("repo");
    let repo = Repository::init(&root).unwrap();
    (dir, repo, root)
}
#[test]
fn unborn_head_preserves_distinct_staged_and_worktree_files() {
    let (_dir, repo, root) = unborn_repo();
    fs::write(root.join("new"), "staged").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("new")).unwrap();
    index.write().unwrap();
    fs::write(root.join("new"), "worktree").unwrap();
    let recovery = create(&Source::open(&root).unwrap(), Limits::default()).unwrap();
    let snapshot = repo
        .find_reference(&recovery.reference)
        .unwrap()
        .peel_to_commit()
        .unwrap();
    let staged = snapshot.parent(0).unwrap();
    let work = snapshot.parent(1).unwrap();
    assert_eq!(staged.parent_count(), 0);
    assert_eq!(work.parent_count(), 0);
    let staged_entry = staged.tree().unwrap().get_path(Path::new("new")).unwrap();
    let work_entry = work.tree().unwrap().get_path(Path::new("new")).unwrap();
    assert_eq!(
        repo.find_blob(staged_entry.id()).unwrap().content(),
        b"staged"
    );
    assert_eq!(
        repo.find_blob(work_entry.id()).unwrap().content(),
        b"worktree"
    );
}
#[test]
fn unborn_head_and_absent_index_capture_untracked_bytes() {
    let (_dir, repo, root) = unborn_repo();
    assert!(!repo.path().join("index").exists());
    fs::write(root.join("untracked"), "recover me").unwrap();
    let recovery = create(&Source::open(&root).unwrap(), Limits::default()).unwrap();
    assert!(!repo.path().join("index").exists());
    assert_eq!(repo.find_tree(recovery.staged_tree).unwrap().len(), 0);
    let tree = repo.find_tree(recovery.worktree_tree).unwrap();
    let entry = tree.get_path(Path::new("untracked")).unwrap();
    assert_eq!(repo.find_blob(entry.id()).unwrap().content(), b"recover me");
}
#[test]
fn appearance_of_initial_index_during_capture_refuses_publication() {
    let (_dir, repo, root) = unborn_repo();
    fs::write(root.join("untracked"), "work").unwrap();
    let source = Source::open(&root).unwrap();
    let result = create_inner(&source, Limits::default(), || {
        repo.index()?.write()?;
        Ok(())
    });
    assert!(result.is_err());
    assert_eq!(
        repo.references_glob("refs/snapshots/forge/*")
            .unwrap()
            .count(),
        0
    );
}
#[test]
fn unborn_head_becoming_born_during_capture_refuses_publication() {
    let (_dir, repo, root) = unborn_repo();
    fs::write(root.join("untracked"), "work").unwrap();
    let source = Source::open(&root).unwrap();
    let result = create_inner(&source, Limits::default(), || {
        let oid = repo.treebuilder(None)?.write()?;
        let tree = repo.find_tree(oid)?;
        let sig = Signature::now("Test", "test@example.com")?;
        repo.commit(
            Some("HEAD"),
            &sig,
            &sig,
            "concurrent first commit",
            &tree,
            &[],
        )?;
        Ok(())
    });
    assert!(result.is_err());
    assert_eq!(
        repo.references_glob("refs/snapshots/forge/*")
            .unwrap()
            .count(),
        0
    );
}
#[test]
fn changing_unborn_head_reference_during_capture_refuses_publication() {
    let (_dir, repo, root) = unborn_repo();
    let source = Source::open(&root).unwrap();
    let result = create_inner(&source, Limits::default(), || {
        repo.set_head("refs/heads/a-different-unborn-head")?;
        Ok(())
    });
    assert!(result.is_err());
    assert_eq!(
        repo.references_glob("refs/snapshots/forge/*")
            .unwrap()
            .count(),
        0
    );
}
