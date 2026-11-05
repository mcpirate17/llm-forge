//! Delegated runner ownership checks; no mutation engine or campaign evidence.
use super::*;
use crate::{
    cgroup::Limits as GroupLimits,
    mutation_execution::CommandSession,
    sandbox::{Policy, Sandbox},
};
use std::{
    cell::Cell,
    collections::BTreeMap,
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    path::PathBuf,
    time::{Duration, Instant},
};

fn start_time(pid: u32) -> Option<String> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    stat.rsplit_once(')')?
        .1
        .split_whitespace()
        .nth(19)
        .map(str::to_owned)
}
struct OwnedChild(OwnedFd);
impl OwnedChild {
    fn open(pid: u32, worker: &Path, receipt: &Path) -> Self {
        let before = start_time(pid).expect("child start time");
        assert_eq!(fs::read_link(format!("/proc/{pid}/exe")).unwrap(), worker);
        let group = fs::read_to_string(format!("/proc/{pid}/cgroup")).unwrap();
        let ledger: Value =
            serde_json::from_slice(&fs::read(receipt).expect("durable pending receipt")).unwrap();
        let path = ledger["process_commands"][0]["pending_cgroup"]["path"]
            .as_str()
            .expect("pending cgroup path");
        let relative = Path::new(path)
            .strip_prefix("/sys/fs/cgroup")
            .expect("cgroup v2 path");
        let expected = format!("0::/{}", relative.display());
        assert!(
            group.lines().any(|line| line == expected),
            "child cgroup {group:?} differs from {expected:?}"
        );
        // SAFETY: pidfd_open creates an owned fd for this exact process instance.
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) } as i32;
        assert!(
            fd >= 0,
            "pidfd_open failed: {}",
            std::io::Error::last_os_error()
        );
        let held = Self(unsafe { OwnedFd::from_raw_fd(fd) });
        assert_eq!(start_time(pid).as_deref(), Some(before.as_str()));
        held
    }
    fn ended(&self) -> bool {
        let mut p = libc::pollfd {
            fd: self.0.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one initialized poll descriptor, held for this call.
        (unsafe { libc::poll(&mut p, 1, 5000) }) > 0 && p.revents & libc::POLLIN != 0
    }
}
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if !self.ended() {
            // SAFETY: pidfd refers to the exact worker instance admitted above.
            let sent = unsafe {
                libc::syscall(
                    libc::SYS_pidfd_send_signal,
                    self.0.as_raw_fd(),
                    libc::SIGKILL,
                    std::ptr::null::<libc::siginfo_t>(),
                    0,
                )
            };
            if sent != 0 || !self.ended() {
                eprintln!(
                    "test child cleanup remains unverified; retained test host requires inspection"
                );
            }
        }
    }
}
fn watcher(
    pidfile: PathBuf,
    ack: PathBuf,
    worker: PathBuf,
    receipt: PathBuf,
    signal_thread: Option<libc::pthread_t>,
) -> std::thread::JoinHandle<OwnedChild> {
    std::thread::spawn(move || {
        let until = Instant::now() + Duration::from_secs(5);
        let pid = loop {
            if let Ok(text) = fs::read_to_string(&pidfile) {
                if let Ok(pid) = text.parse::<u32>() {
                    break pid;
                }
            }
            assert!(Instant::now() < until, "worker never published a PID");
            std::thread::sleep(Duration::from_millis(10));
        };
        let held = OwnedChild::open(pid, &worker, &receipt);
        fs::write(ack, "held").unwrap();
        if let Some(thread) = signal_thread {
            // SAFETY: the thread is the live test owner with SIGTERM blocked.
            assert_eq!(unsafe { libc::pthread_kill(thread, libc::SIGTERM) }, 0);
        }
        held
    })
}
fn worker() -> PathBuf {
    PathBuf::from(
        std::env::var_os("FORGE_TEST_TASK_WORKER").expect("provide separately built task worker"),
    )
    .canonicalize()
    .unwrap()
}
fn delegated_parent() -> PathBuf {
    std::env::var_os("FORGE_TEST_CGROUP_PARENT")
        .expect("explicit delegated cgroup required")
        .into()
}
struct TestHost(Option<tempfile::TempDir>);
impl TestHost {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        fs::set_permissions(
            root.path(),
            std::os::unix::fs::PermissionsExt::from_mode(0o700),
        )
        .unwrap();
        Self(Some(root))
    }
    fn path(&self) -> &Path {
        self.0.as_ref().unwrap().path()
    }
    fn remove(mut self) {
        self.0.take().unwrap().close().unwrap();
    }
}
impl Drop for TestHost {
    fn drop(&mut self) {
        if let Some(root) = self.0.take() {
            eprintln!(
                "retained runner test host for inspection: {}",
                root.keep().display()
            );
        }
    }
}
fn setup() -> (TestHost, Snapshot, Sandbox, tokio::runtime::Runtime) {
    let host = TestHost::new();
    let snapshot = Snapshot::allocate(host.path()).unwrap();
    let sandbox = Sandbox::prepare(Policy {
        read_only: vec![
            worker(),
            PathBuf::from("/usr"),
            PathBuf::from("/etc/ld.so.cache"),
        ],
        scratch: snapshot.root.clone(),
        working_directory: snapshot.root.clone(),
        environment: BTreeMap::new(),
    })
    .unwrap();
    fs::create_dir(&snapshot.worktree).unwrap();
    let reactor = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    (host, snapshot, sandbox, reactor)
}
fn expected_outcome(result: Step<(CommandResult, String)>, mode: &str, cancel: bool) {
    if cancel {
        assert!(matches!(result, Err(Stop::Cancelled(_))));
    } else if mode == "setsid-wait" {
        assert!(result.unwrap().0.timed_out);
    } else {
        assert_eq!(result.unwrap().0.returncode, Some(0));
    }
}

fn assert_removed(path: &Path) {
    assert!(
        matches!(fs::symlink_metadata(path), Err(e) if e.kind() == std::io::ErrorKind::NotFound),
        "owned path was not removed: {}",
        path.display()
    );
}

fn exercise(mode: &str, cancel: bool) {
    let _owner = crate::process_signals::TEST_OWNER
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let interrupts = Interrupts::install().unwrap();
    let supervisor = Supervisor {
        interrupts: &interrupts,
        registry: PathBuf::from("unused"),
        memory_cap: None,
    };
    let (host, mut snapshot, sandbox, reactor) = setup();
    snapshot.preserve();
    let root = snapshot.root.clone();
    let pidfile = snapshot.worktree.join("child.pid");
    let ack = snapshot.worktree.join("owner.ack");
    let executable = worker();
    let thread = if cancel {
        Some(unsafe { libc::pthread_self() })
    } else {
        None
    };
    let output = host.path().join("receipt.json");
    let waiter = watcher(
        pidfile.clone(),
        ack.clone(),
        executable.clone(),
        output.clone(),
        thread,
    );
    let mut campaign = crate::mutation_run_test_support::campaign(host.path(), json!({}));
    let mut receipt = Map::from_iter([("snapshot".into(), json!({"removed":false}))]);
    let complete = Cell::new(true);
    let signal = Cell::new(None);
    let parent = delegated_parent();
    let session = CommandSession {
        sandbox: &sandbox,
        reactor: &reactor,
        cgroup_parent: &parent,
        limits: GroupLimits {
            cpu_millis: 1000,
            memory_mib: 128,
            processes: 16,
        },
        cleanup_complete: &complete,
        signal: &signal,
        programs: None,
        scratch: &root,
        max_wall_seconds: 30,
        cargo_value: false,
    };
    let mut run = EngineRun {
        campaign: &mut campaign,
        receipt: &mut receipt,
        output_path: &output,
        repo_root: host.path(),
        worktree: &snapshot.worktree,
        interpreter: None,
        supervisor: &supervisor,
        runtime: None,
        test_session: Some(&session),
        planned: None,
    };
    let argv = vec![
        executable.to_string_lossy().into_owned(),
        mode.into(),
        "20000".into(),
        pidfile.to_string_lossy().into_owned(),
        ack.to_string_lossy().into_owned(),
    ];
    let result = run.run(
        &argv,
        &snapshot.worktree,
        if mode == "setsid-wait" && !cancel {
            1
        } else {
            15
        },
        &BTreeMap::new(),
        &[],
    );
    let child = waiter.join().unwrap();
    assert!(child.ended(), "detached descendant survived actor return");
    expected_outcome(result, mode, cancel);
    let cgroup = PathBuf::from(
        receipt["process_commands"][0]["confinement"]["resources"]["path"]
            .as_str()
            .unwrap(),
    );
    assert_removed(&cgroup);
    assert!(
        root.exists(),
        "snapshot removed before observing owner cleanup"
    );
    dispose_legacy_snapshot(snapshot, &mut receipt, true).unwrap();
    assert_removed(&root);
    host.remove();
}
#[test]
#[ignore = "requires FORGE_TEST_CGROUP_PARENT and FORGE_TEST_TASK_WORKER"]
fn runner_cleans_setsid_descendant_on_normal_exit() {
    exercise("setsid-exit", false);
}
#[test]
#[ignore = "requires FORGE_TEST_CGROUP_PARENT and FORGE_TEST_TASK_WORKER"]
fn runner_cleans_setsid_descendant_on_timeout() {
    exercise("setsid-wait", false);
}
#[test]
#[ignore = "requires FORGE_TEST_CGROUP_PARENT and FORGE_TEST_TASK_WORKER"]
fn runner_cleans_setsid_descendant_on_cancellation() {
    exercise("setsid-wait", true);
}

#[test]
#[ignore = "requires FORGE_TEST_CGROUP_PARENT and FORGE_TEST_TASK_WORKER"]
fn pending_receipt_write_failure_refuses_launch_and_retains_snapshot() {
    let _owner = crate::process_signals::TEST_OWNER
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let interrupts = Interrupts::install().unwrap();
    let supervisor = Supervisor {
        interrupts: &interrupts,
        registry: PathBuf::from("unused"),
        memory_cap: None,
    };
    let (host, mut snapshot, sandbox, reactor) = setup();
    snapshot.preserve();
    let root = snapshot.root.clone();
    let marker = snapshot.worktree.join("must-not-exist");
    let mut campaign = crate::mutation_run_test_support::campaign(host.path(), json!({}));
    let mut receipt = Map::from_iter([("snapshot".into(), json!({"removed":false}))]);
    let complete = Cell::new(true);
    let signal = Cell::new(None);
    let parent = delegated_parent();
    let session = CommandSession {
        sandbox: &sandbox,
        reactor: &reactor,
        cgroup_parent: &parent,
        limits: GroupLimits {
            cpu_millis: 1000,
            memory_mib: 128,
            processes: 16,
        },
        cleanup_complete: &complete,
        signal: &signal,
        programs: None,
        scratch: &root,
        max_wall_seconds: 30,
        cargo_value: false,
    };
    fs::write(host.path().join("not-a-directory"), b"blocked").unwrap();
    let output = host.path().join("not-a-directory/receipt.json");
    let mut run = EngineRun {
        campaign: &mut campaign,
        receipt: &mut receipt,
        output_path: &output,
        repo_root: host.path(),
        worktree: &snapshot.worktree,
        interpreter: None,
        supervisor: &supervisor,
        runtime: None,
        test_session: Some(&session),
        planned: None,
    };
    let argv = vec![
        worker().to_string_lossy().into_owned(),
        "pid-sleep".into(),
        "500".into(),
        marker.to_string_lossy().into_owned(),
    ];
    assert!(run
        .run(&argv, &snapshot.worktree, 10, &BTreeMap::new(), &[])
        .is_err());
    assert!(!complete.get());
    assert!(run
        .run(&argv, &snapshot.worktree, 10, &BTreeMap::new(), &[])
        .is_err());
    assert!(
        !marker.exists(),
        "payload started despite failed receipt write"
    );
    let cgroup = receipt["process_commands"][0]["pending_cgroup"]["path"]
        .as_str()
        .unwrap();
    assert!(
        matches!(fs::symlink_metadata(cgroup), Err(e) if e.kind() == std::io::ErrorKind::NotFound),
        "prelaunch cgroup survived"
    );
    assert!(dispose_legacy_snapshot(snapshot, &mut receipt, false).is_err());
    assert!(root.exists(), "uncertain snapshot was deleted by Drop");
    fs::remove_dir_all(root).unwrap();
    host.remove();
}

#[test]
#[ignore = "requires FORGE_TEST_CGROUP_PARENT and FORGE_TEST_TASK_WORKER"]
fn unresolved_prior_owner_refuses_next_command_and_retains_snapshot() {
    let _owner = crate::process_signals::TEST_OWNER
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let interrupts = Interrupts::install().unwrap();
    let supervisor = Supervisor {
        interrupts: &interrupts,
        registry: PathBuf::from("unused"),
        memory_cap: None,
    };
    let (host, mut snapshot, sandbox, reactor) = setup();
    snapshot.preserve();
    let root = snapshot.root.clone();
    let marker = snapshot.worktree.join("must-not-exist");
    let mut campaign = crate::mutation_run_test_support::campaign(host.path(), json!({}));
    let mut receipt = Map::from_iter([
        ("snapshot".into(), json!({"removed":false})),
        (
            "process_cleanup".into(),
            json!({"schema_version":1,"complete":false,"commands_observed":0}),
        ),
        ("process_commands".into(), json!([{"state":"pending"}])),
    ]);
    let complete = Cell::new(false);
    let signal = Cell::new(None);
    let parent = delegated_parent();
    let session = CommandSession {
        sandbox: &sandbox,
        reactor: &reactor,
        cgroup_parent: &parent,
        limits: GroupLimits {
            cpu_millis: 1000,
            memory_mib: 128,
            processes: 16,
        },
        cleanup_complete: &complete,
        signal: &signal,
        programs: None,
        scratch: &root,
        max_wall_seconds: 30,
        cargo_value: false,
    };
    let output = host.path().join("receipt.json");
    let mut run = EngineRun {
        campaign: &mut campaign,
        receipt: &mut receipt,
        output_path: &output,
        repo_root: host.path(),
        worktree: &snapshot.worktree,
        interpreter: None,
        supervisor: &supervisor,
        runtime: None,
        test_session: Some(&session),
        planned: None,
    };
    let argv = vec![
        worker().to_string_lossy().into_owned(),
        "pid-sleep".into(),
        "500".into(),
        marker.to_string_lossy().into_owned(),
    ];
    assert!(run
        .run(&argv, &snapshot.worktree, 10, &BTreeMap::new(), &[])
        .is_err());
    assert!(!marker.exists());
    assert!(dispose_legacy_snapshot(snapshot, &mut receipt, true).is_err());
    assert!(root.exists(), "unknown cleanup must survive Drop");
    fs::remove_dir_all(root).unwrap();
    host.remove();
}
