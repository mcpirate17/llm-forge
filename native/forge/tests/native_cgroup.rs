#![cfg(target_os = "linux")]
#[allow(dead_code)]
mod support;
use forge::cgroup::{Cgroup, Limits};
use forge::process_io::ChildExit;
use forge::sandbox::{CommandPlan, Policy, Sandbox};
use std::collections::BTreeMap;
use std::fs;
use std::os::fd::{FromRawFd, OwnedFd};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{unix::AsyncFd, Interest};

fn parent() -> PathBuf {
    std::env::var_os("FORGE_TEST_CGROUP_PARENT")
        .expect("explicit delegated cgroup parent required")
        .into()
}

fn budget(processes: u32) -> Limits {
    Limits {
        cpu_millis: 500,
        memory_mib: 64,
        processes,
    }
}

fn command(host: &support::Host, args: &[&str]) -> CommandPlan {
    let scratch = host.0.join("scratch");
    fs::create_dir(&scratch).unwrap();
    fs::set_permissions(&scratch, fs::Permissions::from_mode(0o700)).unwrap();
    let worker = support::task_worker();
    let policy = Policy {
        read_only: vec![
            "/usr".into(),
            "/lib".into(),
            "/lib64".into(),
            "/etc/ld.so.cache".into(),
            worker.to_path_buf(),
        ],
        scratch: scratch.clone(),
        working_directory: scratch,
        environment: BTreeMap::new(),
    };
    let sandbox = Sandbox::prepare(policy).unwrap();
    let mut plan = sandbox.command(worker).unwrap();
    plan.command()
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    plan
}

#[test]
fn rejects_fake_delegations_and_invalid_budgets_without_mutating_directories() {
    let host = support::Host::new();
    assert!(Cgroup::create(&host.0, budget(32)).is_err());
    assert_eq!(fs::read_dir(&host.0).unwrap().count(), 1);
    assert!(Cgroup::create(
        &host.0,
        Limits {
            cpu_millis: 0,
            ..budget(32)
        }
    )
    .is_err());
    assert!(Cgroup::create(
        &host.0,
        Limits {
            memory_mib: 0,
            ..budget(32)
        }
    )
    .is_err());
    assert!(Cgroup::create(&host.0, budget(0)).is_err());
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires explicitly delegated FORGE_TEST_CGROUP_PARENT and permission for a bounded 64 MiB OOM test"]
async fn memory_limit_is_observed_and_enforced_without_changing_parent_controls() {
    let before = fs::read(parent().join("cgroup.subtree_control")).unwrap();
    let host = support::Host::new();
    let mut group = Cgroup::create(&parent(), budget(32)).unwrap();
    let path = group.report().path.clone();
    assert!(group.report().verified);
    assert_eq!(group.report().cpu_quota_us, 50000);
    assert_eq!(group.report().memory_max_bytes, 64 * 1024 * 1024);
    let mut plan = command(&host, &["allocate", "128"]);
    group.configure_command(plan.command()).unwrap();
    let mut process = plan.spawn().unwrap();
    group.verify_member(process.child()).unwrap();
    let exited = ChildExit::new(process.child()).unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), exited.wait())
            .await
            .unwrap()
            .unwrap()
            .signal(),
        Some(libc::SIGKILL)
    );
    let events = fs::read_to_string(path.join("memory.events")).unwrap();
    assert!(events.lines().any(|line| line
        .strip_prefix("oom_kill ")
        .is_some_and(|v| v.parse::<u64>().unwrap() > 0)));
    group.finish(Duration::from_secs(2)).await.unwrap();
    process.stop().await.unwrap();
    assert!(!path.exists());
    assert_eq!(
        fs::read(parent().join("cgroup.subtree_control")).unwrap(),
        before
    );
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires explicitly delegated FORGE_TEST_CGROUP_PARENT"]
async fn cleanup_kills_detached_descendants_after_the_leader_exits() {
    let parent_membership = fs::read_to_string("/proc/self/cgroup").unwrap();
    let host = support::Host::new();
    let mut group = Cgroup::create(&parent(), budget(32)).unwrap();
    let path = group.report().path.clone();
    let mut plan = command(&host, &["detach-exit", "20000", "child.pid"]);
    group.configure_command(plan.command()).unwrap();
    let mut process = plan.spawn().unwrap();
    let exited = ChildExit::new(process.child()).unwrap();
    assert!(tokio::time::timeout(Duration::from_secs(3), exited.wait())
        .await
        .unwrap()
        .unwrap()
        .success());
    let descendant: u32 = fs::read_to_string(host.0.join("scratch/child.pid"))
        .unwrap()
        .parse()
        .unwrap();
    let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, descendant, 0) };
    assert!(raw >= 0);
    // SAFETY: pidfd_open returned a new descriptor. AsyncFd takes exclusive
    // ownership and the test neither closes nor replaces it during registration.
    let descendant = unsafe {
        AsyncFd::register_with_interest(OwnedFd::from_raw_fd(raw as i32), Interest::READABLE)
    }
    .unwrap();
    group.finish(Duration::from_secs(2)).await.unwrap();
    let _ready = tokio::time::timeout(Duration::from_secs(1), descendant.readable())
        .await
        .unwrap()
        .unwrap();
    process.stop().await.unwrap();
    group.finish(Duration::from_secs(2)).await.unwrap();
    assert!(!path.exists());
    assert_eq!(
        fs::read_to_string("/proc/self/cgroup").unwrap(),
        parent_membership
    );
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires explicitly delegated FORGE_TEST_CGROUP_PARENT"]
async fn process_limit_prevents_forks_inside_the_confined_task() {
    let host = support::Host::new();
    let mut group = Cgroup::create(&parent(), budget(1)).unwrap();
    let mut plan = command(&host, &["process-limit"]);
    group.configure_command(plan.command()).unwrap();
    assert!(group.configure_command(plan.command()).is_err());
    let mut process = plan.spawn().unwrap();
    let exited = ChildExit::new(process.child()).unwrap();
    assert!(tokio::time::timeout(Duration::from_secs(2), exited.wait())
        .await
        .unwrap()
        .unwrap()
        .success());
    group.finish(Duration::from_secs(2)).await.unwrap();
    process.stop().await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires explicitly delegated FORGE_TEST_CGROUP_PARENT"]
async fn cpu_quota_throttles_a_bounded_native_workload() {
    let host = support::Host::new();
    let mut group = Cgroup::create(
        &parent(),
        Limits {
            cpu_millis: 10,
            ..budget(32)
        },
    )
    .unwrap();
    let mut plan = command(&host, &["burn", "300"]);
    group.configure_command(plan.command()).unwrap();
    let mut process = plan.spawn().unwrap();
    let exited = ChildExit::new(process.child()).unwrap();
    assert!(tokio::time::timeout(Duration::from_secs(5), exited.wait())
        .await
        .unwrap()
        .unwrap()
        .success());
    let statistics = fs::read_to_string(group.report().path.join("cpu.stat")).unwrap();
    assert!(statistics.lines().any(|line| line
        .strip_prefix("nr_throttled ")
        .is_some_and(|v| v.parse::<u64>().unwrap() > 0)));
    group.finish(Duration::from_secs(2)).await.unwrap();
    process.stop().await.unwrap();
}
