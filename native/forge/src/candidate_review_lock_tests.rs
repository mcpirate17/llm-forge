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
fn released(path: &Path) {
    // Parallel tests can fork while this process holds a CLOEXEC descriptor.
    // That temporary copy closes at exec/exit, not when our owner is dropped.
    // Bound the wait so a leaked descriptor still fails instead of retrying forever.
    shared(path, Duration::from_secs(5)).unwrap();
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
    choose_exclusive(&path, None, false, Duration::from_secs(5)).unwrap();
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
    released(&path);
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

#[test]
fn writer_lease_blocks_readers_and_is_inherited_by_child() {
    let (_dir, path) = path();
    let writer = choose_exclusive(&path, None, false, Duration::ZERO).unwrap();
    assert!(shared(&path, Duration::ZERO).is_err());
    let mut command = std::process::Command::new("sh");
    command.args(["-c", "test -e /proc/self/fd/$LLM_GOVERNANCE_COMMIT_LOCK_FD && test -z \"$LLM_GOVERNANCE_COMMIT_LOCK_TOKEN\""]);
    assert!(writer.run_child(&mut command).unwrap().success());
    assert!(shared(&path, Duration::ZERO).is_err());
    drop(writer);
    released(&path);
}
#[test]
fn child_retains_writer_lease_after_parent_owner_drops() {
    use std::io::{BufRead, BufReader, Write};
    use std::process::Stdio;
    let (_dir, path) = path();
    let writer = choose_exclusive(&path, None, false, Duration::ZERO).unwrap();
    let mut command = std::process::Command::new("sh");
    command
        .args(["-c", "printf 'ready\\n'; read -r release"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped());
    writer.prepare_child(&mut command).unwrap();
    let mut child = command.spawn().unwrap();
    let mut ready = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut ready)
        .unwrap();
    assert_eq!(ready, "ready\n");
    drop(writer);
    assert!(shared(&path, Duration::ZERO).is_err());
    child.stdin.take().unwrap().write_all(b"release\n").unwrap();
    assert!(child.wait().unwrap().success());
    released(&path);
}
#[test]
fn writer_refuses_token_and_unheld_descriptors() {
    let (_dir, path) = path();
    assert!(choose_exclusive(&path, None, true, Duration::ZERO).is_err());
    let unheld = open(&path).unwrap();
    assert!(choose_exclusive(
        &path,
        Some(unheld.as_raw_fd().to_string().into()),
        false,
        Duration::ZERO
    )
    .is_err());
    assert!(choose_exclusive(&path, Some("-1".into()), false, Duration::ZERO).is_err());
}
#[test]
fn child_failure_and_signal_return_status_and_release_writer_on_drop() {
    use std::os::unix::process::ExitStatusExt;
    let (_dir, path) = path();
    let writer = choose_exclusive(&path, None, false, Duration::ZERO).unwrap();
    let mut fail = std::process::Command::new("sh");
    fail.args(["-c", "exit 37"]);
    assert_eq!(writer.run_child(&mut fail).unwrap().code(), Some(37));
    let mut signal = std::process::Command::new("sh");
    signal.args(["-c", "kill -TERM $$"]);
    assert_eq!(
        writer.run_child(&mut signal).unwrap().signal(),
        Some(libc::SIGTERM)
    );
    assert!(shared(&path, Duration::ZERO).is_err());
    drop(writer);
    released(&path);
}

#[test]
fn verified_incoming_fd_is_cloexec_but_explicit_child_handoff_works() {
    use std::os::fd::FromRawFd;
    let (_dir, path) = path();
    let owner = exclusive(&path);
    let raw = unsafe { libc::fcntl(owner.as_raw_fd(), libc::F_DUPFD, 100) };
    assert!(raw >= 100);
    let received = unsafe { File::from_raw_fd(raw) };
    assert_eq!(
        unsafe { libc::fcntl(raw, libc::F_GETFD) } & libc::FD_CLOEXEC,
        0
    );
    let lease = inherited(&path, raw).unwrap();
    assert_ne!(
        unsafe { libc::fcntl(raw, libc::F_GETFD) } & libc::FD_CLOEXEC,
        0
    );
    let mut unrelated = std::process::Command::new("sh");
    unrelated.args(["-c", &format!("test ! -e /proc/self/fd/{raw}")]);
    assert!(unrelated.status().unwrap().success());
    let mut intentional = std::process::Command::new("sh");
    intentional.args(["-c", "test -e /proc/self/fd/$LLM_GOVERNANCE_COMMIT_LOCK_FD"]);
    assert!(lease.run_child(&mut intentional).unwrap().success());
    drop(received);
}
#[test]
fn exclusive_wait_calls_cancellation_without_releasing_competing_owner() {
    let (_dir, path) = path();
    let _owner = exclusive(&path);
    let mut calls = 0;
    let error =
        match choose_exclusive_cancellable(&path, None, false, Duration::from_secs(60), &mut || {
            calls += 1;
            if calls >= 4 {
                anyhow::bail!("cancelled by test");
            }
            Ok(())
        }) {
            Ok(_) => panic!("cancellation failed to interrupt lock wait"),
            Err(error) => error,
        };
    assert!(error.to_string().contains("cancelled by test"));
    assert!(shared(&path, Duration::ZERO).is_err());
}
