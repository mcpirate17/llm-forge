use super::*;
use std::{fs, os::unix::fs::symlink};

#[test]
fn real_paths_pass_and_symlinks_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let actual = dir.path().join("actual");
    fs::create_dir(&actual).unwrap();
    let file = actual.join("input");
    fs::write(&file, b"ok").unwrap();
    check(Path::new("/")).unwrap();
    check(&actual).unwrap();
    check(&file).unwrap();
    symlink(&actual, dir.path().join("directory-link")).unwrap();
    symlink(&file, dir.path().join("file-link")).unwrap();
    symlink(dir.path().join("absent"), dir.path().join("dangling")).unwrap();
    for path in [
        dir.path().join("directory-link/input"),
        dir.path().join("file-link"),
        dir.path().join("dangling"),
        dir.path().join("absent"),
        file.join("child"),
    ] {
        assert!(
            check(&path).is_err(),
            "unexpectedly accepted {}",
            path.display()
        );
    }
    assert!(check(Path::new("relative")).is_err());
    assert!(check(&dir.path().join("actual/../actual/input")).is_err());
    if Path::new("/proc/self/fd").exists() {
        assert!(check(Path::new("/proc/self/fd/0")).is_err());
    }
}

#[test]
fn repeated_checks_do_not_retain_descriptors() {
    let dir = tempfile::tempdir().unwrap();
    if !Path::new("/proc/self/fd").exists() {
        return;
    }
    // Count only this test's unique directory so other parallel tests cannot
    // change the result by opening their own unrelated descriptors.
    let count = || {
        fs::read_dir("/proc/self/fd")
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| fs::read_link(entry.path()).is_ok_and(|path| path == dir.path()))
            .count()
    };
    let before = count();
    for _ in 0..256 {
        check(dir.path()).unwrap();
    }
    let after = count();
    assert_eq!(
        after, before,
        "checks retained descriptors for the directory"
    );
}
