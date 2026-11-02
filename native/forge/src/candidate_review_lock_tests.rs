use super::*;
use std::os::unix::fs::PermissionsExt;
fn path() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("lock");
    (dir, path)
}
fn exclusive(path: &Path) -> File {
    let file = open(path).unwrap();
    assert_eq!(
        unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
    file
}
#[test]
fn independent_readers_share_and_writer_waits() {
    let (_dir, path) = path();
    let first = shared(&path, Duration::ZERO).unwrap();
    let second = shared(&path, Duration::ZERO).unwrap();
    let test = open(&path).unwrap();
    assert_ne!(
        unsafe { libc::flock(test.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
    drop(first);
    assert_ne!(
        unsafe { libc::flock(test.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
    drop(second);
    assert_eq!(
        unsafe { libc::flock(test.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
}
#[test]
fn inherited_exclusive_is_retained_without_unlocking_parent() {
    let (_dir, path) = path();
    let parent = exclusive(&path);
    let child = inherited(&path, parent.as_raw_fd()).unwrap();
    assert!(shared(&path, Duration::ZERO).is_err());
    drop(child);
    assert!(shared(&path, Duration::ZERO).is_err());
    drop(parent);
    assert!(shared(&path, Duration::ZERO).is_ok());
}
#[test]
fn inherited_explicit_parent_unlock_is_detected() {
    let (_dir, path) = path();
    let parent = exclusive(&path);
    let child = inherited(&path, parent.as_raw_fd()).unwrap();
    assert_eq!(unsafe { libc::flock(parent.as_raw_fd(), libc::LOCK_UN) }, 0);
    assert!(child.verify().is_err());
}
#[test]
fn invalid_descriptor_replaced_inode_and_unsafe_file_refuse() {
    let (_dir, path) = path();
    let unlocked = open(&path).unwrap();
    assert!(inherited(&path, unlocked.as_raw_fd()).is_err());
    let owner = exclusive(&path);
    let other = path.with_extension("other");
    let wrong = open(&other).unwrap();
    assert!(inherited(&path, wrong.as_raw_fd()).is_err());
    let lease = inherited(&path, owner.as_raw_fd()).unwrap();
    fs::rename(&path, path.with_extension("old")).unwrap();
    let replacement = open(&path).unwrap();
    assert!(lease.verify().is_err());
    drop(replacement);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o666)).unwrap();
    assert!(open(&path).is_err());
}
#[test]
fn token_only_and_invalid_fd_fail_without_environment_mutation() {
    let (_dir, path) = path();
    let _parent = exclusive(&path);
    assert!(choose(&path, None, true, Duration::ZERO).is_err());
    assert!(choose(&path, Some("invalid".into()), true, Duration::ZERO).is_err());
}
#[test]
fn symlink_governance_directory_refuses() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("real");
    fs::create_dir(&target).unwrap();
    std::os::unix::fs::symlink(&target, temp.path().join("link")).unwrap();
    assert!(shared(&temp.path().join("link/lock"), Duration::ZERO).is_err());
    assert!(!target.join("lock").exists());
}
