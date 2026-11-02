#![cfg(target_os = "linux")]
#[path = "support/forge_binary.rs"]
mod forge_binary;
use git2::{Repository, Signature};
use std::{
    fs,
    os::{
        fd::AsRawFd,
        unix::{fs::PermissionsExt, process::ExitStatusExt},
    },
    path::{Path, PathBuf},
    process::{Command, Output},
    time::{Duration, Instant},
};

struct Fixture {
    temp: tempfile::TempDir,
    root: PathBuf,
    repo: Repository,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("repo");
        let repo = Repository::init(&root).unwrap();
        let mut config = repo.config().unwrap();
        config.set_str("user.name", "Fixture").unwrap();
        config
            .set_str("user.email", "fixture@example.invalid")
            .unwrap();
        drop(config);
        fs::write(root.join("tracked"), "baseline").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("tracked")).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let sig = Signature::now("Fixture", "fixture@example.invalid").unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "base", &tree, &[])
            .unwrap();
        drop(tree);
        fs::write(root.join("tracked"), "staged").unwrap();
        index.add_path(Path::new("tracked")).unwrap();
        index.write().unwrap();
        fs::write(root.join("tracked"), "unstaged").unwrap();
        fs::write(root.join("untracked"), "untracked").unwrap();
        Self { temp, root, repo }
    }
    fn command(&self) -> Command {
        let mut cmd = Command::new(forge_binary::path());
        cmd.current_dir(&self.root)
            .args(["candidate", "commit", "--repo", "."])
            .env("HOME", self.temp.path())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("FORGE_HOOK_TEST", forge_binary::path());
        for key in [
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_INDEX_FILE",
            "GIT_COMMON_DIR",
            "GIT_OBJECT_DIRECTORY",
            "GIT_ALTERNATE_OBJECT_DIRECTORIES",
            "LLM_GOVERNANCE_COMMIT_LOCK_FD",
            "LLM_GOVERNANCE_COMMIT_LOCK_TOKEN",
        ] {
            cmd.env_remove(key);
        }
        cmd
    }
    fn run(&self, args: &[&str]) -> Output {
        self.command().args(args).output().unwrap()
    }
    fn hook(&self, body: &str) {
        let path = self.repo.path().join("hooks/pre-commit");
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    fn refs(&self) -> Vec<String> {
        self.repo
            .references_glob("refs/snapshots/forge/*")
            .unwrap()
            .map(|v| v.unwrap().name().unwrap().to_owned())
            .collect()
    }
    fn lock(&self) -> fs::File {
        fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(self.repo.path().join("governance/commit-review.lock"))
            .unwrap()
    }
}
#[test]
fn commit_preserves_both_snapshots_and_runs_the_real_hook() {
    let f = Fixture::new();
    f.hook("printf hook-ran > ../hook-marker");
    let out = f.run(&["--", "-m", "candidate"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        fs::read(f.temp.path().join("hook-marker")).unwrap(),
        b"hook-ran"
    );
    assert_eq!(
        f.repo
            .head()
            .unwrap()
            .peel_to_commit()
            .unwrap()
            .tree()
            .unwrap()
            .get_path(Path::new("tracked"))
            .unwrap()
            .to_object(&f.repo)
            .unwrap()
            .as_blob()
            .unwrap()
            .content(),
        b"staged"
    );
    assert_eq!(fs::read(f.root.join("tracked")).unwrap(), b"unstaged");
    let refs = f.refs();
    assert_eq!(refs.len(), 1);
    let snap = f
        .repo
        .find_reference(&refs[0])
        .unwrap()
        .peel_to_commit()
        .unwrap();
    for (parent, expected) in [(0, b"staged".as_slice()), (1, b"unstaged".as_slice())] {
        let tree = snap.parent(parent).unwrap().tree().unwrap();
        let blob = f
            .repo
            .find_blob(tree.get_path(Path::new("tracked")).unwrap().id())
            .unwrap();
        assert_eq!(blob.content(), expected);
    }
    let tree = snap.parent(1).unwrap().tree().unwrap();
    assert_eq!(
        f.repo
            .find_blob(tree.get_path(Path::new("untracked")).unwrap().id())
            .unwrap()
            .content(),
        b"untracked"
    );
}
#[test]
fn hook_failure_preserves_exit_status_and_releases_lock() {
    let f = Fixture::new();
    let before = f.repo.index().unwrap().write_tree().unwrap();
    let head = f.repo.head().unwrap().target().unwrap();
    let control = Fixture::new();
    control.hook("exit 23");
    let direct = Command::new("/usr/bin/git")
        .current_dir(&control.root)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", control.temp.path())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(["commit", "-m", "blocked"])
        .output()
        .unwrap();
    assert_eq!(direct.status.code(), Some(1));
    f.hook("exit 23");
    let out = f.run(&["--", "-m", "blocked"]);
    // Git may rewrite cache-tree metadata on a rejected hook. Snapshot creation
    // itself is separately tested byte-for-byte; the real Git child owns its
    // normal index updates. Preserve its status and the actual staged tree.
    assert_eq!(out.status.code(), direct.status.code());
    for repo in [&f.repo, &control.repo] {
        let mut index = repo.index().unwrap();
        index.read(true).unwrap();
        assert_eq!(index.write_tree().unwrap(), before);
    }
    assert_eq!(f.repo.head().unwrap().target().unwrap(), head);
    assert_eq!(fs::read(f.root.join("tracked")).unwrap(), b"unstaged");
    assert_eq!(fs::read(f.root.join("untracked")).unwrap(), b"untracked");
    assert_eq!(f.refs().len(), 1);
    let lock = f.lock();
    assert_eq!(
        unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
}
#[test]
fn hook_can_reenter_native_commit_with_inherited_open_lease() {
    let f = Fixture::new();
    f.hook("\"$FORGE_HOOK_TEST\" candidate commit --repo \"$PWD\" -- --dry-run --no-verify >../nested-out 2>../nested-err");
    let out = f.run(&["--", "-m", "candidate"]);
    assert!(
        out.status.success(),
        "outer: {} nested: {}",
        String::from_utf8_lossy(&out.stderr),
        fs::read_to_string(f.temp.path().join("nested-err")).unwrap_or_default()
    );
    assert_eq!(f.refs().len(), 2);
}
#[test]
fn forged_or_token_only_inheritance_and_invalid_limits_fail_before_snapshot() {
    let f = Fixture::new();
    let mut forged = f.command();
    forged
        .env("LLM_GOVERNANCE_COMMIT_LOCK_FD", "99999")
        .args(["--", "-m", "x"]);
    assert!(!forged.output().unwrap().status.success());
    let mut token = f.command();
    token
        .env("LLM_GOVERNANCE_COMMIT_LOCK_TOKEN", "forged")
        .args(["--", "-m", "x"]);
    assert!(!token.output().unwrap().status.success());
    assert!(!f
        .run(&["--max-entries", "0", "--", "-m", "x"])
        .status
        .success());
    assert!(f.refs().is_empty());
}
#[test]
fn snapshot_admission_failure_does_not_run_hooks_or_publish() {
    let f = Fixture::new();
    f.hook("printf ran > ../hook-marker");
    fs::write(f.root.join("large"), vec![b'x'; 32]).unwrap();
    let out = f.run(&["--max-file-bytes", "16", "--", "-m", "x"]);
    assert!(!out.status.success());
    assert!(!f.temp.path().join("hook-marker").exists());
    assert!(f.refs().is_empty());
}
#[test]
fn child_signal_is_propagated_and_lease_is_released() {
    let f = Fixture::new();
    let fake = f.temp.path().join("bin");
    fs::create_dir(&fake).unwrap();
    let git = fake.join("git");
    fs::write(&git, "#!/bin/sh\nkill -TERM $$\n").unwrap();
    fs::set_permissions(&git, fs::Permissions::from_mode(0o755)).unwrap();
    let mut cmd = f.command();
    let out = cmd
        .env("PATH", &fake)
        .args(["--", "-m", "x"])
        .output()
        .unwrap();
    assert_eq!(out.status.signal(), Some(libc::SIGTERM));
    assert_eq!(f.refs().len(), 1);
    let lock = f.lock();
    assert_eq!(
        unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
}
struct ReapWrapper(std::process::Child);
impl Drop for ReapWrapper {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_some() {
            return;
        }
        unsafe {
            libc::kill(self.0.id() as i32, libc::SIGTERM);
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if self.0.try_wait().ok().flatten().is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        if let Err(error) = self.0.kill() {
            eprintln!("test cleanup wrapper: {error}");
        }
        if let Err(error) = self.0.wait() {
            eprintln!("test reap wrapper: {error}");
        }
    }
}

fn wait_path(path: &Path, deadline: Instant) {
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}
fn terminated(pid: i32) -> bool {
    match fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => stat
            .rsplit_once(") ")
            .is_some_and(|(_, rest)| rest.starts_with("Z ")),
        Err(error) => error.kind() == std::io::ErrorKind::NotFound,
    }
}
fn cancellation_stops_real_git_hook_and_descendant(signal: i32, ignored: bool) {
    use std::os::unix::process::CommandExt;
    use std::process::Stdio;
    let f = Fixture::new();
    let head = f.repo.head().unwrap().target().unwrap();
    // All three pids are in the owned noninteractive Git process group. The
    // short fallback timeout bounds leaked hook lifetime if the assertion fails.
    f.hook("echo $PPID > ../git-pid; echo $$ > ../hook-pid; trap '' TERM INT; /bin/sleep 5 & echo $! > ../sleep-pid; wait");
    let mut cmd = f.command();
    if ignored {
        unsafe {
            cmd.pre_exec(move || {
                if libc::signal(signal, libc::SIG_IGN) == libc::SIG_ERR {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    let mut wrapper = ReapWrapper(
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .args(["--", "-m", "cancelled"])
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(8);
    let marker = f.temp.path().join("sleep-pid");
    wait_path(&marker, deadline);
    let read_pid = |name: &str| -> i32 {
        fs::read_to_string(f.temp.path().join(name))
            .unwrap()
            .trim()
            .parse()
            .unwrap()
    };
    let pids = [
        read_pid("git-pid"),
        read_pid("hook-pid"),
        read_pid("sleep-pid"),
    ];
    let lock = f.lock();
    assert_ne!(
        unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
    assert_eq!(unsafe { libc::kill(wrapper.0.id() as i32, signal) }, 0);
    let status = loop {
        if let Some(status) = wrapper.0.try_wait().unwrap() {
            break status;
        }
        assert!(Instant::now() < deadline, "wrapper did not reap and exit");
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(status.signal(), Some(signal));
    while pids.iter().any(|pid| !terminated(*pid)) {
        assert!(
            Instant::now() < deadline,
            "Git or hook descendant survived cancellation"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
    assert_eq!(f.repo.head().unwrap().target().unwrap(), head);
    assert_eq!(f.refs().len(), 1);
}
#[test]
fn parent_signals_stop_real_git_and_descendants() {
    cancellation_stops_real_git_hook_and_descendant(libc::SIGTERM, false);
    cancellation_stops_real_git_hook_and_descendant(libc::SIGINT, true);
}
#[test]
fn child_receives_only_git_commit_and_its_exact_nonzero_exit_is_returned() {
    let f = Fixture::new();
    let fake = f.temp.path().join("bin");
    fs::create_dir(&fake).unwrap();
    let git = fake.join("git");
    fs::write(
        &git,
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > ../git-args\nexit 37\n",
    )
    .unwrap();
    fs::set_permissions(&git, fs::Permissions::from_mode(0o755)).unwrap();
    let mut cmd = f.command();
    let out = cmd
        .env("PATH", &fake)
        .args(["--", "status"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(37));
    assert_eq!(
        fs::read_to_string(f.temp.path().join("git-args")).unwrap(),
        "commit\nstatus\n"
    );
}
#[test]
fn unrelated_open_descriptor_is_rejected_before_snapshot() {
    use std::os::unix::process::CommandExt;
    let f = Fixture::new();
    let file = fs::File::create(f.temp.path().join("fake-lock")).unwrap();
    let fd = file.as_raw_fd();
    let mut cmd = f.command();
    cmd.env("LLM_GOVERNANCE_COMMIT_LOCK_FD", fd.to_string())
        .args(["--", "-m", "x"]);
    unsafe {
        cmd.pre_exec(move || {
            let flags = libc::fcntl(fd, libc::F_GETFD);
            if flags < 0 || libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    assert!(!cmd.output().unwrap().status.success());
    assert!(f.refs().is_empty());
}

#[test]
fn invalid_repository_is_rejected_before_git_runs() {
    let f = Fixture::new();
    let mut cmd = Command::new(forge_binary::path());
    cmd.current_dir(&f.root)
        .args(["candidate", "commit", "--repo", "missing", "--", "-m", "x"]);
    assert!(!cmd.output().unwrap().status.success());
    assert!(f.refs().is_empty());
}
#[test]
fn parent_cancellation_interrupts_competing_lease_wait_before_snapshot() {
    use std::os::unix::fs::OpenOptionsExt;
    use std::process::Stdio;
    let f = Fixture::new();
    let lock_dir = f.repo.path().join("governance");
    fs::create_dir_all(&lock_dir).unwrap();
    let lock_path = lock_dir.join("commit-review.lock");
    let owner = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&lock_path)
        .unwrap();
    assert_eq!(
        unsafe { libc::flock(owner.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
    f.hook("printf ran > ../hook-marker");
    let before = fs::read(f.repo.path().join("index")).unwrap();
    let mut command = f.command();
    let mut wrapper = ReapWrapper(
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .args(["--", "-m", "cancelled"])
            .spawn()
            .unwrap(),
    );
    let fd_dir = PathBuf::from(format!("/proc/{}/fd", wrapper.0.id()));
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let opened = fs::read_dir(&fd_dir)
            .ok()
            .into_iter()
            .flatten()
            .flatten()
            .any(|entry| fs::read_link(entry.path()).is_ok_and(|target| target == lock_path));
        if opened {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "wrapper never began waiting on competing lease"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        unsafe { libc::kill(wrapper.0.id() as i32, libc::SIGTERM) },
        0
    );
    let deadline = Instant::now() + Duration::from_secs(3);
    let status = loop {
        if let Some(status) = wrapper.0.try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "lock-wait cancellation exceeded deadline"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(status.signal(), Some(libc::SIGTERM));
    assert!(f.refs().is_empty());
    assert!(!f.temp.path().join("hook-marker").exists());
    assert_eq!(fs::read(f.repo.path().join("index")).unwrap(), before);
    let probe = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&lock_path)
        .unwrap();
    assert_ne!(
        unsafe { libc::flock(probe.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
}
fn initial_commit_case(staged: bool, alternate: bool) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("repo");
    let repo = Repository::init(&root).unwrap();
    let mut config = repo.config().unwrap();
    config.set_str("user.name", "Fixture").unwrap();
    config
        .set_str("user.email", "fixture@example.invalid")
        .unwrap();
    drop(config);
    fs::write(
        root.join("new"),
        if staged { "staged" } else { "untracked" },
    )
    .unwrap();
    if staged {
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("new")).unwrap();
        index.write().unwrap();
        fs::write(root.join("new"), "worktree").unwrap();
    } else {
        assert!(!repo.path().join("index").exists());
    }
    let mut command = Command::new(forge_binary::path());
    command
        .current_dir(&root)
        .args([
            "candidate",
            "commit",
            "--repo",
            ".",
            "--",
            "--allow-empty",
            "-m",
            "initial",
        ])
        .env("HOME", temp.path())
        .env("GIT_CONFIG_NOSYSTEM", "1");
    for key in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "LLM_GOVERNANCE_COMMIT_LOCK_FD",
        "LLM_GOVERNANCE_COMMIT_LOCK_TOKEN",
    ] {
        command.env_remove(key);
    }
    if alternate {
        command.env("GIT_INDEX_FILE", repo.path().join("initial-index"));
    } else {
        command.env_remove("GIT_INDEX_FILE");
    }
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut refs = repo.references_glob("refs/snapshots/forge/*").unwrap();
    let reference = refs.next().unwrap().unwrap();
    assert!(refs.next().is_none());
    let snapshot = reference.peel_to_commit().unwrap();
    let index_commit = snapshot.parent(0).unwrap();
    let work_commit = snapshot.parent(1).unwrap();
    assert_eq!(index_commit.parent_count(), 0);
    assert_eq!(work_commit.parent_count(), 0);
    let index_tree = index_commit.tree().unwrap();
    let work_tree = work_commit.tree().unwrap();
    if staged {
        let entry = index_tree.get_path(Path::new("new")).unwrap();
        assert_eq!(repo.find_blob(entry.id()).unwrap().content(), b"staged");
    } else {
        assert!(index_tree.get_path(Path::new("new")).is_err());
    }
    let entry = work_tree.get_path(Path::new("new")).unwrap();
    assert_eq!(
        repo.find_blob(entry.id()).unwrap().content(),
        if staged {
            b"worktree".as_slice()
        } else {
            b"untracked".as_slice()
        }
    );
    assert_eq!(
        repo.head()
            .unwrap()
            .peel_to_commit()
            .unwrap()
            .parent_count(),
        0
    );
}
#[test]
fn initial_commits_recover_staged_and_untracked_files() {
    initial_commit_case(true, false);
    initial_commit_case(false, false);
    initial_commit_case(false, true);
}
