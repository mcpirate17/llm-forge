use super::*;
use std::{fs, path::Path};

fn put(root: &Path, path: &str, value: &str) {
    let full = root.join(path);
    fs::create_dir_all(full.parent().unwrap()).unwrap();
    fs::write(full, value).unwrap();
}
fn commit(repo: &git2::Repository, message: &str) -> git2::Oid {
    let mut index = repo.index().unwrap();
    index
        .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
        .unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let sig = git2::Signature::now("fixture", "fixture@example.test").unwrap();
    let parent = repo.head().ok().and_then(|head| head.peel_to_commit().ok());
    let parents: Vec<_> = parent.iter().collect();
    repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &parents)
        .unwrap()
}
fn selected(source: &Source, selection: &Selection, untracked: bool) -> Vec<String> {
    let mut capture = LiveInputs::open(source.root()).unwrap();
    let result = select(source, &mut capture, selection, untracked, &[]).unwrap();
    capture.verify().unwrap();
    result
}
#[test]
fn inventory_tracks_index_worktree_base_and_ignores() {
    let dir = tempfile::tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    put(dir.path(), ".gitignore", "ignored/\n");
    put(dir.path(), "tests/test_old.py", "old\n");
    put(dir.path(), "src/unit.rs", "pub fn value() {}\n");
    let base = commit(&repo, "base");
    put(dir.path(), "tests/test_branch.py", "branch\n");
    commit(&repo, "branch");
    let source = Source::open(dir.path()).unwrap();
    let branch = selected(&source, &Selection::Changed(Some(base.to_string())), false);
    assert_eq!(branch, vec!["tests/test_branch.py"]);
    fs::remove_file(dir.path().join("tests/test_old.py")).unwrap();
    put(dir.path(), "tests/test_staged.py", "staged\n");
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("tests/test_staged.py")).unwrap();
    index.write().unwrap();
    put(dir.path(), "src/unit.rs", "#[test]\nfn current() {}\n");
    put(dir.path(), "tests/test_untracked.py", "untracked\n");
    put(dir.path(), "ignored/test_hidden.py", "ignored\n");
    assert_eq!(
        selected(&source, &Selection::Changed(Some(base.to_string())), false),
        vec!["tests/test_branch.py"]
    );
    assert_eq!(
        selected(&source, &Selection::Changed(Some(base.to_string())), true),
        vec!["tests/test_branch.py", "tests/test_untracked.py"]
    );
    let changed = selected(&source, &Selection::Changed(None), false);
    assert_eq!(
        changed,
        vec![
            "src/unit.rs",
            "tests/test_old.py",
            "tests/test_staged.py",
            "tests/test_untracked.py"
        ]
    );
    let all = selected(&source, &Selection::Coverage, false);
    assert_eq!(
        all,
        vec![
            "src/unit.rs",
            "tests/test_branch.py",
            "tests/test_old.py",
            "tests/test_staged.py"
        ]
    );
    let all = selected(&source, &Selection::Coverage, true);
    assert!(all.contains(&"tests/test_untracked.py".into()));
    assert!(!all.contains(&"ignored/test_hidden.py".into()));
}
#[test]
fn rust_files_used_to_classify_non_tests_are_held() {
    let dir = tempfile::tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    put(dir.path(), "src/lib.rs", "pub fn value() {}\n");
    commit(&repo, "base");
    let source = Source::open(dir.path()).unwrap();
    let mut capture = LiveInputs::open(dir.path()).unwrap();
    assert!(
        select(&source, &mut capture, &Selection::Coverage, false, &[])
            .unwrap()
            .is_empty()
    );
    put(dir.path(), "src/lib.rs", "#[test]\nfn check() {}\n");
    assert!(capture.verify().is_err());
}

#[test]
fn malformed_rust_source_is_a_visible_error() {
    let dir = tempfile::tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    put(dir.path(), "src/lib.rs", "pub fn value() {}\n");
    commit(&repo, "base");
    fs::write(dir.path().join("src/lib.rs"), [0xff]).unwrap();
    let source = Source::open(dir.path()).unwrap();
    let mut capture = LiveInputs::open(dir.path()).unwrap();
    let error = select(&source, &mut capture, &Selection::Coverage, false, &[]).unwrap_err();
    assert!(error.to_string().contains("not UTF-8: src/lib.rs"));
}
#[test]
fn changed_rename_reports_destination_and_base_excludes_deletion() {
    let dir = tempfile::tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    put(dir.path(), "tests/test_before.py", "same content\n");
    put(dir.path(), "tests/test_removed.py", "remove me\n");
    let base = commit(&repo, "base");
    fs::rename(
        dir.path().join("tests/test_before.py"),
        dir.path().join("tests/test_after.py"),
    )
    .unwrap();
    fs::remove_file(dir.path().join("tests/test_removed.py")).unwrap();
    let mut index = repo.index().unwrap();
    index
        .remove_path(Path::new("tests/test_before.py"))
        .unwrap();
    index
        .remove_path(Path::new("tests/test_removed.py"))
        .unwrap();
    index.add_path(Path::new("tests/test_after.py")).unwrap();
    index.write().unwrap();
    let source = Source::open(dir.path()).unwrap();
    let working = selected(&source, &Selection::Changed(None), true);
    assert_eq!(
        working,
        vec!["tests/test_after.py", "tests/test_removed.py"]
    );
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let sig = git2::Signature::now("fixture", "fixture@example.test").unwrap();
    let parent = repo.head().unwrap().peel_to_commit().unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "rename", &tree, &[&parent])
        .unwrap();
    let branch = selected(&source, &Selection::Changed(Some(base.to_string())), true);
    assert_eq!(branch, vec!["tests/test_after.py"]);
}
