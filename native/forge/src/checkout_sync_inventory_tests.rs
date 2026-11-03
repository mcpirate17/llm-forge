use super::*;
use std::fs;
#[test]
fn records_ignored_leaves_without_following_symlink_targets() {
    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    git2::Repository::init(dir.path()).unwrap();
    fs::write(dir.path().join(".gitignore"), b"ignored/\nlink\n").unwrap();
    fs::create_dir(dir.path().join("ignored")).unwrap();
    fs::write(dir.path().join("ignored/secret"), b"data").unwrap();
    fs::write(outside.path().join("external"), b"safe").unwrap();
    std::os::unix::fs::symlink(outside.path(), dir.path().join("link")).unwrap();
    let source = Source::open(dir.path()).unwrap();
    let lease = SyncLease::acquire(dir.path(), &mut || Ok(())).unwrap();
    let hidden = inventory(&source, &lease, &BTreeSet::new())
        .unwrap()
        .ignored;
    assert!(hidden.contains("ignored/secret"));
    assert!(hidden.contains("link"));
    assert!(!hidden.contains("link/external"));
    assert_eq!(fs::read(outside.path().join("external")).unwrap(), b"safe");
}
