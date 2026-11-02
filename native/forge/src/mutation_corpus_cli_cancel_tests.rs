//! Opt-in CLI tests exercise the real supervisor and a native blocked Git fixture.
use super::*;
use std::{
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

fn fixture_git(parent: &Path) -> PathBuf {
    let binary = parent.join("blocked-git");
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/corpus_block_git.rs");
    let compiler = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let output = crate::process_probe::run(
        Command::new(compiler)
            .args(["--edition=2021", "-o"])
            .arg(&binary)
            .arg(source),
        Duration::from_secs(30),
        65536,
    )
    .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).unwrap();
    binary
}
fn secure_tree(path: &Path) {
    let meta = fs::symlink_metadata(path).unwrap();
    assert!(!meta.file_type().is_symlink());
    if meta.is_dir() {
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        for child in fs::read_dir(path).unwrap() {
            secure_tree(&child.unwrap().path());
        }
    } else if meta.is_file() {
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }
}
fn command(f: &Fixture, marker: &Path, record: bool) -> Command {
    let bin = std::env::var_os("FORGE_TEST_BIN")
        .expect("set FORGE_TEST_BIN to the matching Forge binary");
    let mut cmd = Command::new(bin);
    cmd.args(["candidate", "mutation-corpus", "--repo"])
        .arg(&f.root)
        .args([
            "--kind",
            "commit",
            "--policy",
            "policy.toml",
            "--scratch-parent",
        ])
        .arg(&f.scratch)
        .arg("--git")
        .arg(&f.git)
        .arg("--git-sha256")
        .arg(&f.git_hash)
        .arg("--git-build-profile")
        .arg(&f.profile)
        .arg("--git-build-profile-sha256")
        .arg(&f.profile_hash)
        .arg("--mutation-runner-bundle")
        .arg(&f.bundle)
        .arg("--mutation-runner-sha256")
        .arg(&f.bundle_hash)
        .args(["--registry", "campaigns/registry.json"])
        .env("FORGE_TEST_BLOCK_GIT_DIR", marker)
        .env_remove("LLM_GOVERNANCE_COMMIT_LOCK_FD")
        .env_remove("LLM_GOVERNANCE_COMMIT_LOCK_TOKEN")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if record {
        cmd.arg("--write-baseline");
    }
    cmd
}
struct Running(Child);
impl Drop for Running {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            unsafe {
                libc::kill(self.0.id() as i32, libc::SIGTERM);
            }
            let deadline = Instant::now() + Duration::from_secs(3);
            while Instant::now() < deadline {
                if self.0.try_wait().ok().flatten().is_some() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}
fn wait_until(mut ready: impl FnMut() -> bool, child: &mut Child, what: &str) {
    let end = Instant::now() + Duration::from_secs(12);
    while !ready() {
        assert!(
            child.try_wait().unwrap().is_none(),
            "CLI exited before {what}"
        );
        assert!(Instant::now() < end, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}
fn pidfd(pid: i32) -> OwnedFd {
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
    assert!(fd >= 0, "failed to pin fixture process {pid}");
    unsafe { OwnedFd::from_raw_fd(fd as i32) }
}
fn exited(fd: &OwnedFd) -> bool {
    let mut poll = libc::pollfd {
        fd: fd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    unsafe { libc::poll(&mut poll, 1, 0) == 1 && poll.revents & libc::POLLIN != 0 }
}
fn cancelled(child: &mut Child) {
    assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGTERM) }, 0);
    let end = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(!status.success());
            return;
        }
        assert!(Instant::now() < end, "CLI did not stop after SIGTERM");
        std::thread::sleep(Duration::from_millis(5));
    }
}
fn setup() -> (tempfile::TempDir, Fixture) {
    let temp = tempfile::tempdir().unwrap();
    fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let git = fixture_git(temp.path());
    let fixture = Fixture::new_with_git(Some(git));
    secure_tree(&fixture.root);
    (temp, fixture)
}
#[test]
#[ignore = "requires source-matching FORGE_TEST_BIN and rustc for the native fixture"]
fn standalone_cli_cancels_owned_git_probe_and_descendant() {
    let (temp, f) = setup();
    let mut child = Running(command(&f, temp.path(), false).spawn().unwrap());
    let marker = temp.path().join("pids");
    wait_until(|| marker.exists(), &mut child.0, "blocked Git probe");
    let text = fs::read_to_string(marker).unwrap();
    let pids: Vec<i32> = text
        .split_whitespace()
        .map(|s| s.parse().unwrap())
        .collect();
    assert_eq!(pids.len(), 2);
    let children: Vec<OwnedFd> = pids.into_iter().map(pidfd).collect();
    cancelled(&mut child.0);
    let end = Instant::now() + Duration::from_secs(5);
    while !children.iter().all(exited) {
        assert!(Instant::now() < end, "Git descendant survived cancellation");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        fs::read_dir(&f.scratch).unwrap().count(),
        0,
        "candidate snapshot leaked"
    );
}
fn signal_masked(pid: u32) -> bool {
    let text = fs::read_to_string(format!("/proc/{pid}/status")).unwrap_or_default();
    let mask = text
        .lines()
        .find_map(|line| line.strip_prefix("SigBlk:\t"))
        .and_then(|n| u64::from_str_radix(n.trim(), 16).ok())
        .unwrap_or(0);
    mask & (1 << (libc::SIGTERM - 1)) != 0
}
#[test]
#[ignore = "requires source-matching FORGE_TEST_BIN and rustc for the native fixture"]
fn standalone_cli_cancels_while_waiting_for_real_writer_lease() {
    let (temp, f) = setup();
    let lease = crate::candidate_review_lock::ReviewLock::acquire_exclusive_cancellable(
        &f.root,
        Duration::from_secs(2),
        &mut || Ok(()),
    )
    .unwrap();
    let mut child = Running(command(&f, temp.path(), true).spawn().unwrap());
    let pid = child.0.id();
    wait_until(|| signal_masked(pid), &mut child.0, "interrupt ownership");
    cancelled(&mut child.0);
    lease.verify().unwrap();
    assert!(!temp.path().join("pids").exists());
    assert!(!f
        .root
        .join("campaigns/reproducibility_baseline.json")
        .exists());
    assert_eq!(fs::read_dir(&f.scratch).unwrap().count(), 0);
}
