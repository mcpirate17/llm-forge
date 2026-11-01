#![cfg(target_os = "linux")]
#[allow(dead_code)]
mod support;
use forge::cgroup::{Cgroup, Limits};
use forge::managed_service::{prepare_children, verify_self, Service};
use std::collections::BTreeMap;
use std::fs;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn start(host: &support::Host, lifetime: u64) -> Service {
    Service::launch(
        &std::env::current_exe().unwrap(),
        &[
            "--exact".into(),
            "service_fixture".into(),
            "--nocapture".into(),
        ],
        Limits {
            cpu_millis: 500,
            memory_mib: 128,
            processes: 64,
        },
        lifetime,
        &BTreeMap::from([(
            "FORGE_SERVICE_TEST_ROOT".into(),
            host.0.to_str().unwrap().into(),
        )]),
    )
    .unwrap()
}

fn ready(root: &Path) -> (OwnedFd, PathBuf) {
    let until = Instant::now() + Duration::from_secs(8);
    while !root.join("ready").exists() {
        assert!(
            !root.join("error").exists(),
            "{}",
            fs::read_to_string(root.join("error")).unwrap()
        );
        assert!(Instant::now() < until, "service did not become ready");
        std::thread::sleep(Duration::from_millis(20));
    }
    let pid: i32 = fs::read_to_string(root.join("ready"))
        .unwrap()
        .parse()
        .unwrap();
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) as i32 };
    assert!(fd >= 0, "{}", std::io::Error::last_os_error());
    (
        unsafe { OwnedFd::from_raw_fd(fd) },
        fs::read_to_string(root.join("group")).unwrap().into(),
    )
}

fn assert_gone(pid: &OwnedFd, group: &Path) {
    let mut descriptor = libc::pollfd {
        fd: pid.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // Runtime test uses 7 seconds plus the verified 5-second stop grace.
    assert_eq!(unsafe { libc::poll(&mut descriptor, 1, 15000) }, 1);
    let until = Instant::now() + Duration::from_secs(4);
    while group.exists() {
        assert!(
            Instant::now() < until,
            "owned cgroup still exists: {}",
            group.display()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn refuses_unmanaged_processes_and_unbounded_requests() {
    assert!(verify_self("unrelated.service").is_err());
    assert!(verify_self("forge-harness-nonexistent.service").is_err());
    for lifetime in [0, 86401] {
        assert!(Service::launch(
            Path::new("/bin/true"),
            &[],
            Limits {
                cpu_millis: 100,
                memory_mib: 64,
                processes: 32,
            },
            lifetime,
            &BTreeMap::new()
        )
        .is_err());
    }
}

#[test]
#[ignore = "starts an explicitly bounded transient systemd user service"]
fn manager_kills_detached_descendants_when_daemon_crashes() {
    let host = support::Host::new();
    let mut service = start(&host, 30);
    let (child, group) = ready(&host.0);
    service.kill_main().unwrap();
    assert_gone(&child, &group);
    service.stop().unwrap();
    service.stop().unwrap();
}

#[test]
#[ignore = "starts an explicitly bounded transient systemd user service"]
fn runtime_deadline_cleans_descendants_without_client_or_daemon_cleanup() {
    let host = support::Host::new();
    let mut service = start(&host, 7);
    let (child, group) = ready(&host.0);
    assert_gone(&child, &group);
    service.stop().unwrap();
}

#[test]
#[ignore = "starts an explicitly bounded transient systemd user service"]
fn dropping_launch_ownership_stops_the_service_tree() {
    let host = support::Host::new();
    let service = start(&host, 30);
    let (child, group) = ready(&host.0);
    drop(service);
    assert_gone(&child, &group);
}

#[test]
#[ignore = "starts a bounded service and waits through its configured SIGTERM grace"]
fn explicit_stop_waits_for_the_full_descendant_cleanup_grace() {
    let host = support::Host::new();
    let mut service = start(&host, 30);
    let (child, group) = ready(&host.0);
    service.stop().unwrap();
    assert_gone(&child, &group);
    service.stop().unwrap();
}

#[test]
fn service_fixture() {
    let Some(root) = std::env::var_os("FORGE_SERVICE_TEST_ROOT") else {
        return;
    };
    let root = PathBuf::from(root);
    if let Err(error) = run_fixture(&root) {
        fs::write(root.join("error"), format!("{error:#}")).unwrap();
        panic!("{error:#}");
    }
}

fn run_fixture(root: &Path) -> anyhow::Result<()> {
    let parent = prepare_children(&std::env::var("FORGE_SERVICE_UNIT")?)?;
    assert_eq!(
        fs::read_to_string(parent.join("memory.max"))?.trim(),
        "134217728"
    );
    assert_eq!(fs::read_to_string(parent.join("pids.max"))?.trim(), "64");
    assert_eq!(
        fs::read_to_string(parent.join("cpu.max"))?.trim(),
        "50000 100000"
    );
    let mut group = Cgroup::create(
        &parent,
        Limits {
            cpu_millis: 100,
            memory_mib: 32,
            processes: 8,
        },
    )?;
    let mut command = Command::new(std::env::current_exe()?);
    command
        .args(["--exact", "descendant_fixture", "--nocapture"])
        .env("FORGE_SERVICE_TEST_CHILD", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    group.configure_command(&mut command)?;
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            libc::signal(libc::SIGTERM, libc::SIG_IGN);
            Ok(())
        });
    }
    let mut child = command.spawn()?;
    group.verify_member(&child)?;
    fs::write(root.join("group"), parent.to_str().unwrap())?;
    fs::write(root.join("ready"), child.id().to_string())?;
    std::thread::sleep(Duration::from_secs(60));
    child.kill()?;
    child.wait()?;
    Ok(())
}

#[test]
fn descendant_fixture() {
    if std::env::var_os("FORGE_SERVICE_TEST_CHILD").is_some() {
        std::thread::sleep(Duration::from_secs(60));
    }
}
