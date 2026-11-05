#![cfg(target_os = "linux")]
#[allow(dead_code)]
mod support;
use forge::{
    cgroup::{Cgroup, Limits as GroupLimits},
    confined_execution::{self, Limits, Outcome, Report},
    sandbox::{CommandPlan, Policy, Sandbox},
};
use std::{collections::BTreeMap, fs, os::unix::fs::PermissionsExt, path::PathBuf, time::Duration};

fn parent() -> PathBuf {
    std::env::var_os("FORGE_TEST_CGROUP_PARENT")
        .expect("explicit delegation required")
        .into()
}
fn limits() -> Limits {
    Limits {
        timeout: Duration::from_secs(3),
        max_output_chars: 32,
        cpu_seconds: None,
        address_space_bytes: None,
    }
}
fn prepared(
    host: &support::Host,
    program: &std::path::Path,
    args: &[&str],
) -> (CommandPlan, Cgroup, PathBuf) {
    let scratch = host.0.join("scratch");
    fs::create_dir(&scratch).unwrap();
    fs::set_permissions(&scratch, fs::Permissions::from_mode(0o700)).unwrap();
    let mut reads = vec![
        PathBuf::from("/usr"),
        "/lib".into(),
        "/lib64".into(),
        "/etc/ld.so.cache".into(),
    ];
    if program.exists() {
        reads.push(program.into());
    }
    let sandbox = Sandbox::prepare(Policy {
        read_only: reads,
        scratch: scratch.clone(),
        working_directory: scratch,
        environment: BTreeMap::new(),
    })
    .unwrap();
    let mut plan = sandbox.command(program).unwrap();
    plan.command().args(args);
    let group = Cgroup::create(
        &parent(),
        GroupLimits {
            cpu_millis: 1000,
            memory_mib: 128,
            processes: 16,
        },
    )
    .unwrap();
    let group_path = group.report().path.clone();
    (plan, group, group_path)
}
async fn completed(plan: CommandPlan, group: Cgroup, limits: Limits) -> Report {
    tokio::time::timeout(
        Duration::from_secs(8),
        confined_execution::start(plan, group, limits)
            .unwrap()
            .wait(),
    )
    .await
    .unwrap()
    .unwrap()
}
fn cleaned(report: &Report, path: &std::path::Path) {
    assert!(report.cleanup_complete, "{report:?}");
    assert!(report.errors.is_empty(), "{report:?}");
    assert!(report.resources.verified);
    assert!(!path.exists(), "owned cgroup survived completion");
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires explicitly delegated FORGE_TEST_CGROUP_PARENT"]
async fn separate_output_tails_preserve_status_and_parent_controls() {
    let controls = fs::read(parent().join("cgroup.subtree_control")).unwrap();
    let host = support::Host::new();
    let (plan, group, path) = prepared(&host, support::task_worker(), &["output"]);
    let report = completed(plan, group, limits()).await;
    cleaned(&report, &path);
    assert_eq!(report.outcome, Outcome::Exited);
    assert_eq!(report.exit_code, Some(0));
    assert_eq!(report.stdout, "x".repeat(32));
    assert_eq!(report.stdout_bytes, 8192);
    assert_eq!(report.stderr, "error detail");
    assert_eq!(report.stderr_bytes, 12);
    assert_eq!(
        fs::read(parent().join("cgroup.subtree_control")).unwrap(),
        controls
    );
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires explicitly delegated FORGE_TEST_CGROUP_PARENT"]
async fn deadline_cleans_cpu_work_and_cpu_time_limit_is_distinct() {
    let host = support::Host::new();
    let (plan, group, path) = prepared(&host, support::task_worker(), &["burn", "10000"]);
    let report = completed(
        plan,
        group,
        Limits {
            timeout: Duration::from_millis(100),
            ..limits()
        },
    )
    .await;
    cleaned(&report, &path);
    assert_eq!(report.outcome, Outcome::TimedOut);
    assert_eq!(report.signal, Some(libc::SIGKILL));
    let host = support::Host::new();
    let (plan, group, path) = prepared(&host, support::task_worker(), &["burn", "10000"]);
    let report = completed(
        plan,
        group,
        Limits {
            timeout: Duration::from_secs(5),
            cpu_seconds: Some(1),
            ..limits()
        },
    )
    .await;
    cleaned(&report, &path);
    assert_eq!(report.outcome, Outcome::Exited);
    assert_eq!(report.signal, Some(libc::SIGKILL));
    assert_eq!(report.cpu_seconds, Some(1));
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires explicitly delegated FORGE_TEST_CGROUP_PARENT"]
async fn explicit_cancellation_and_dropped_waiter_keep_cleanup_owner_alive() {
    let host = support::Host::new();
    let (plan, group, path) = prepared(&host, support::task_worker(), &["sleep", "10000"]);
    let execution = confined_execution::start(plan, group, limits()).unwrap();
    execution.cancellation().cancel().unwrap();
    let report = tokio::time::timeout(Duration::from_secs(4), execution.wait())
        .await
        .unwrap()
        .unwrap();
    cleaned(&report, &path);
    assert_eq!(report.outcome, Outcome::Cancelled);
    let host = support::Host::new();
    let (plan, group, path) = prepared(
        &host,
        support::task_worker(),
        &["pid-sleep", "10000", "leader.pid"],
    );
    let execution = confined_execution::start(plan, group, limits()).unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(100), execution.wait())
            .await
            .is_err()
    );
    tokio::time::timeout(Duration::from_secs(4), async {
        while path.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let pid: i32 = fs::read_to_string(host.0.join("scratch/leader.pid"))
        .unwrap()
        .parse()
        .unwrap();
    assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires explicitly delegated FORGE_TEST_CGROUP_PARENT"]
async fn successful_leader_does_not_leave_detached_children_running() {
    let host = support::Host::new();
    let (plan, group, path) = prepared(
        &host,
        support::task_worker(),
        &["detach-exit", "20000", "child.pid"],
    );
    let report = completed(plan, group, limits()).await;
    cleaned(&report, &path);
    assert_eq!(report.outcome, Outcome::Exited);
    assert_eq!(report.exit_code, Some(0));
    let pid: i32 = fs::read_to_string(host.0.join("scratch/child.pid"))
        .unwrap()
        .parse()
        .unwrap();
    match fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => assert!(
            stat.rsplit_once(')').unwrap().1.starts_with(" Z "),
            "{stat}"
        ),
        Err(error) => assert_eq!(error.kind(), std::io::ErrorKind::NotFound),
    }
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires explicitly delegated FORGE_TEST_CGROUP_PARENT"]
async fn metadata_broker_remains_live_while_synchronous_observer_waits() {
    let host = support::Host::new();
    let (plan, group, path) = prepared(
        &host,
        std::path::Path::new("/usr/bin/chmod"),
        &["700", "metadata"],
    );
    fs::write(host.0.join("scratch/metadata"), b"owned").unwrap();
    let report = completed(plan, group, limits()).await;
    cleaned(&report, &path);
    assert_eq!(report.exit_code, Some(0), "{report:?}");
    assert_eq!(
        fs::metadata(host.0.join("scratch/metadata"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires explicitly delegated FORGE_TEST_CGROUP_PARENT"]
async fn missing_executable_returns_failure_and_removes_owned_cgroup() {
    let host = support::Host::new();
    let (plan, group, path) = prepared(
        &host,
        std::path::Path::new("/usr/bin/forge-definitely-missing"),
        &[],
    );
    let report = completed(plan, group, limits()).await;
    assert_eq!(report.outcome, Outcome::Failed);
    assert!(report.cleanup_complete);
    assert!(!path.exists());
    assert!(report.errors.iter().any(|e| e.starts_with("launch:")));
    assert_eq!(report.exit_code, None);
}

#[derive(Debug)]
struct Finalized {
    armed: bool,
    cleanup_safe: bool,
    group_gone: bool,
    child_dead: bool,
}

struct OutcomeProbe(std::sync::mpsc::Sender<confined_execution::FinishState>);
impl confined_execution::Resources for OutcomeProbe {
    fn arm(&mut self) {}
    fn finish(self: Box<Self>, _: bool) -> anyhow::Result<()> {
        anyhow::bail!("outcome-aware finalizer was bypassed")
    }
    fn finish_with_state(
        self: Box<Self>,
        state: confined_execution::FinishState,
    ) -> anyhow::Result<()> {
        self.0.send(state)?;
        Ok(())
    }
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires explicitly delegated FORGE_TEST_CGROUP_PARENT"]
async fn finalizer_receives_exit_status_after_cleanup() {
    for (code, success) in [("0", true), ("7", false)] {
        let host = support::Host::new();
        let (plan, group, path) = prepared(&host, support::task_worker(), &["exit", code]);
        let (send, receive) = std::sync::mpsc::channel();
        let report = tokio::time::timeout(
            Duration::from_secs(8),
            confined_execution::start_with_resources(
                plan,
                group,
                limits(),
                Box::new(OutcomeProbe(send)),
            )
            .unwrap()
            .wait(),
        )
        .await
        .unwrap()
        .unwrap();
        let state = receive.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(state.cleanup_safe);
        assert!(!path.exists());
        assert_eq!(state.outcome, report.outcome);
        assert_eq!(state.exit_code, report.exit_code);
        assert_eq!(state.succeeded(), success);
    }
}

struct FinalizerBarrier {
    entered: tokio::sync::oneshot::Sender<confined_execution::FinishState>,
    release: std::sync::mpsc::Receiver<()>,
}
impl confined_execution::Resources for FinalizerBarrier {
    fn arm(&mut self) {}
    fn finish(self: Box<Self>, _: bool) -> anyhow::Result<()> {
        anyhow::bail!("outcome-aware finalizer was bypassed")
    }
    fn finish_with_state(
        self: Box<Self>,
        state: confined_execution::FinishState,
    ) -> anyhow::Result<()> {
        self.entered
            .send(state)
            .map_err(|_| anyhow::anyhow!("finalizer observer dropped"))?;
        self.release
            .recv_timeout(Duration::from_secs(5))
            .map_err(|error| anyhow::anyhow!("finalizer barrier: {error}"))?;
        Ok(())
    }
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires explicitly delegated FORGE_TEST_CGROUP_PARENT"]
async fn cancellation_after_finalizer_entry_does_not_relabel_fixed_exit() {
    let host = support::Host::new();
    let (plan, group, path) = prepared(&host, support::task_worker(), &["exit", "0"]);
    let (entered, receive) = tokio::sync::oneshot::channel();
    let (release, barrier) = std::sync::mpsc::channel();
    let execution = confined_execution::start_with_resources(
        plan,
        group,
        limits(),
        Box::new(FinalizerBarrier {
            entered,
            release: barrier,
        }),
    )
    .unwrap();
    let state = tokio::time::timeout(Duration::from_secs(5), receive)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(state.outcome, Outcome::Exited);
    assert_eq!(state.exit_code, Some(0));
    assert!(state.cleanup_safe && !path.exists());
    execution.cancellation().cancel().unwrap();
    release.send(()).unwrap();
    let report = tokio::time::timeout(Duration::from_secs(5), execution.wait())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(report.outcome, Outcome::Exited);
    assert_eq!(report.exit_code, Some(0));
    assert!(report.cleanup_complete);
    assert!(report.errors.is_empty());
}
struct LifecycleProbe {
    armed: bool,
    group: PathBuf,
    child_pid_file: Option<PathBuf>,
    notify: std::sync::mpsc::Sender<Finalized>,
    fail: bool,
}
impl confined_execution::Resources for LifecycleProbe {
    fn arm(&mut self) {
        self.armed = true;
    }
    fn finish(self: Box<Self>, cleanup_safe: bool) -> anyhow::Result<()> {
        let child_dead = match &self.child_pid_file {
            Some(path) => {
                let raw = fs::read_to_string(path)?;
                let pid: i32 = raw.parse()?;
                match fs::read_to_string(format!("/proc/{pid}/stat")) {
                    Ok(stat) => stat
                        .rsplit_once(')')
                        .is_some_and(|(_, suffix)| suffix.starts_with(" Z ")),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
                    Err(error) => return Err(error.into()),
                }
            }
            None => true,
        };
        let result = Finalized {
            armed: self.armed,
            cleanup_safe,
            group_gone: !self.group.exists(),
            child_dead,
        };
        self.notify.send(result)?;
        anyhow::ensure!(!self.fail, "injected resource finalizer failure");
        Ok(())
    }
}
fn lifecycle(
    group: &std::path::Path,
    pid_file: Option<PathBuf>,
    fail: bool,
) -> (
    Box<dyn confined_execution::Resources>,
    std::sync::mpsc::Receiver<Finalized>,
) {
    let (notify, receive) = std::sync::mpsc::channel();
    (
        Box::new(LifecycleProbe {
            armed: false,
            group: group.to_owned(),
            child_pid_file: pid_file,
            notify,
            fail,
        }),
        receive,
    )
}
async fn finalization(receiver: std::sync::mpsc::Receiver<Finalized>) -> Finalized {
    tokio::time::timeout(Duration::from_secs(5), async move {
        loop {
            match receiver.try_recv() {
                Ok(value) => break value,
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    tokio::time::sleep(Duration::from_millis(10)).await
                }
                Err(error) => panic!("resource finalizer disappeared: {error}"),
            }
        }
    })
    .await
    .expect("resource finalizer timed out")
}
fn assert_finalized(value: Finalized) {
    assert!(
        value.armed && value.cleanup_safe && value.group_gone && value.child_dead,
        "{value:?}"
    );
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires explicitly delegated FORGE_TEST_CGROUP_PARENT"]
async fn resource_finalizer_runs_after_detached_descendant_and_group_cleanup() {
    let host = support::Host::new();
    let (plan, group, path) = prepared(
        &host,
        support::task_worker(),
        &["detach-exit", "20000", "child.pid"],
    );
    let (resources, receive) = lifecycle(&path, Some(host.0.join("scratch/child.pid")), false);
    let report = tokio::time::timeout(
        Duration::from_secs(8),
        confined_execution::start_with_resources(plan, group, limits(), resources)
            .unwrap()
            .wait(),
    )
    .await
    .unwrap()
    .unwrap();
    cleaned(&report, &path);
    assert_eq!(report.outcome, Outcome::Exited);
    assert_finalized(finalization(receive).await);
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires explicitly delegated FORGE_TEST_CGROUP_PARENT"]
async fn dropped_waiter_still_finalizes_after_child_cleanup() {
    let host = support::Host::new();
    let (plan, group, path) = prepared(
        &host,
        support::task_worker(),
        &["pid-sleep", "20000", "child.pid"],
    );
    let pid = host.0.join("scratch/child.pid");
    let (resources, receive) = lifecycle(&path, Some(pid.clone()), false);
    let execution =
        confined_execution::start_with_resources(plan, group, limits(), resources).unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while !pid.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    drop(execution);
    assert_finalized(finalization(receive).await);
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires explicitly delegated FORGE_TEST_CGROUP_PARENT"]
async fn finalizer_error_fails_report_after_successful_process() {
    let host = support::Host::new();
    let (plan, group, path) = prepared(&host, support::task_worker(), &["exit", "0"]);
    let (resources, receive) = lifecycle(&path, None, true);
    let report = tokio::time::timeout(
        Duration::from_secs(8),
        confined_execution::start_with_resources(plan, group, limits(), resources)
            .unwrap()
            .wait(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_finalized(finalization(receive).await);
    assert_eq!(report.outcome, Outcome::Failed);
    assert!(!report.cleanup_complete);
    assert!(
        report
            .errors
            .iter()
            .any(|error| error.contains("injected resource finalizer failure")),
        "{report:?}"
    );
    assert!(!path.exists());
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires explicitly delegated FORGE_TEST_CGROUP_PARENT"]
async fn deadline_is_serviced_while_both_streams_flood() {
    let host = support::Host::new();
    let (plan, group, path) = prepared(&host, support::task_worker(), &["flood-both", "10000"]);
    let report = completed(
        plan,
        group,
        Limits {
            timeout: Duration::from_millis(300),
            ..limits()
        },
    )
    .await;
    cleaned(&report, &path);
    assert_eq!(report.outcome, Outcome::TimedOut);
    assert!(
        report.stdout_bytes > 0 && report.stderr_bytes > 0,
        "{report:?}"
    );
    assert_eq!(report.stdout, "o".repeat(32));
    assert_eq!(report.stderr, "e".repeat(32));
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires explicitly delegated FORGE_TEST_CGROUP_PARENT"]
async fn virtual_address_space_limit_is_distinct_from_cgroup_memory() {
    for (address_space, expected) in [
        (None, "mapping granted\n"),
        (Some(64 * 1024 * 1024), "mapping denied\n"),
    ] {
        let host = support::Host::new();
        let (plan, group, path) = prepared(&host, support::task_worker(), &["map-probe", "96"]);
        let report = completed(
            plan,
            group,
            Limits {
                address_space_bytes: address_space,
                ..limits()
            },
        )
        .await;
        cleaned(&report, &path);
        assert_eq!(report.outcome, Outcome::Exited);
        assert_eq!(report.exit_code, Some(0));
        assert_eq!(report.stdout, expected);
        assert_eq!(report.address_space_bytes, address_space);
        assert_eq!(report.resources.memory_max_bytes, 128 * 1024 * 1024);
    }
}

struct CaptureFinalizer(std::sync::Arc<std::sync::atomic::AtomicBool>);
impl confined_execution::Resources for CaptureFinalizer {
    fn arm(&mut self) {}
    fn finish(self: Box<Self>, _cleanup_safe: bool) -> anyhow::Result<()> {
        self.0.store(true, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires explicitly delegated FORGE_TEST_CGROUP_PARENT"]
async fn complete_output_overflow_fails_and_runs_owned_cleanup() {
    let host = support::Host::new();
    let (plan, group, path) = prepared(&host, std::path::Path::new("/usr/bin/yes"), &[]);
    let finished = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let execution = confined_execution::start_capture_with_resources(
        plan,
        group,
        Limits {
            timeout: Duration::from_secs(10),
            ..limits()
        },
        Box::new(CaptureFinalizer(finished.clone())),
    )
    .unwrap();
    let report = tokio::time::timeout(Duration::from_secs(20), execution.wait())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(report.outcome, Outcome::Failed, "{report:?}");
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.contains("stdout output exceeds")),
        "{report:?}"
    );
    assert!(report.stdout.len() <= 16 * 1024 * 1024);
    assert!(!path.exists(), "owned cgroup survived overflow");
    assert!(finished.load(std::sync::atomic::Ordering::SeqCst));
}
