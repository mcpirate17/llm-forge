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
struct DuplexHost {
    host: Option<support::Host>,
    safe: bool,
}
impl DuplexHost {
    fn new() -> Self {
        Self {
            host: Some(support::Host::new()),
            safe: false,
        }
    }
    fn get(&self) -> &support::Host {
        self.host.as_ref().unwrap()
    }
}
impl Drop for DuplexHost {
    fn drop(&mut self) {
        if !self.safe {
            if let Some(host) = self.host.take() {
                eprintln!("retaining unverified duplex fixture {}", host.0.display());
                std::mem::forget(host);
            }
        }
    }
}
struct DuplexResources(std::sync::Arc<std::sync::Mutex<Vec<bool>>>);
impl confined_execution::Resources for DuplexResources {
    fn arm(&mut self) {}
    fn finish(self: Box<Self>, cleanup_safe: bool) -> anyhow::Result<()> {
        self.0.lock().unwrap().push(cleanup_safe);
        Ok(())
    }
}
fn duplex_limits() -> confined_execution::DuplexLimits {
    confined_execution::DuplexLimits {
        message_bytes: 1024 * 1024,
        input_bytes: 2 * 1024 * 1024,
        stdout_bytes: 1024 * 1024,
        stderr_bytes: 1024 * 1024,
        output_chunks: 32,
    }
}
fn duplex_start(
    plan: CommandPlan,
    group: Cgroup,
    limits: Limits,
    trace: std::sync::Arc<std::sync::Mutex<Vec<bool>>>,
) -> confined_execution::Duplex {
    confined_execution::start_duplex_with_resources(
        plan,
        group,
        limits,
        Box::new(DuplexResources(trace)),
        duplex_limits(),
    )
    .unwrap()
}
async fn next_stdout(duplex: &mut confined_execution::Duplex) -> Vec<u8> {
    let mut output = Vec::new();
    loop {
        match tokio::time::timeout(Duration::from_secs(3), duplex.recv())
            .await
            .unwrap()
            .unwrap()
        {
            Some(confined_execution::DuplexEvent::Stdout(bytes)) => {
                output.extend(bytes);
                if output.last() == Some(&b'\n') {
                    return output;
                }
            }
            Some(_) => (),
            None => panic!("stdout ended before response"),
        }
    }
}
#[tokio::test(flavor = "current_thread")]
#[ignore = "requires explicitly delegated FORGE_TEST_CGROUP_PARENT"]
async fn duplex_keeps_stdin_open_for_multiple_exact_binary_responses() {
    let mut host = DuplexHost::new();
    let (plan, group, path) = prepared(host.get(), support::task_worker(), &["duplex-lines"]);
    let trace = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut duplex = duplex_start(plan, group, limits(), trace.clone());
    for value in [
        b"first\n".as_slice(),
        b"second\n".as_slice(),
        b"\xff\x00\n".as_slice(),
    ] {
        duplex.send(value.to_vec()).await.unwrap();
        assert_eq!(next_stdout(&mut duplex).await, value);
    }
    duplex.close_stdin().await.unwrap();
    while duplex.recv().await.unwrap().is_some() {}
    let report = duplex.wait().await.unwrap();
    cleaned(&report, &path);
    assert!(matches!(fs::symlink_metadata(path),Err(e) if e.kind()==std::io::ErrorKind::NotFound));
    assert_eq!(report.outcome, Outcome::Exited);
    assert_eq!(*trace.lock().unwrap(), vec![true]);
    host.safe = true;
}
#[tokio::test(flavor = "current_thread")]
#[ignore = "requires explicitly delegated FORGE_TEST_CGROUP_PARENT"]
async fn duplex_dropped_send_and_client_trigger_owned_cancellation() {
    let mut host = DuplexHost::new();
    let (plan, group, path) = prepared(host.get(), support::task_worker(), &["sleep", "2000"]);
    let trace = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut duplex = duplex_start(plan, group, limits(), trace.clone());
    let mut completion = duplex.completion();
    assert!(tokio::time::timeout(
        Duration::from_millis(100),
        duplex.send(vec![b'x'; 1024 * 1024])
    )
    .await
    .is_err());
    drop(duplex);
    let terminal = tokio::time::timeout(Duration::from_secs(6), completion.wait())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(terminal.outcome, Outcome::Cancelled);
    assert!(terminal.cleanup_complete);
    assert!(matches!(fs::symlink_metadata(path),Err(e) if e.kind()==std::io::ErrorKind::NotFound));
    assert_eq!(*trace.lock().unwrap(), vec![true]);
    host.safe = true;
}
#[tokio::test(flavor = "current_thread")]
#[ignore = "requires explicitly delegated FORGE_TEST_CGROUP_PARENT"]
async fn duplex_output_limit_fails_through_owned_cleanup() {
    let mut host = DuplexHost::new();
    let (plan, group, path) = prepared(host.get(), support::task_worker(), &["output"]);
    let trace = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut cfg = duplex_limits();
    cfg.stdout_bytes = 128;
    let mut duplex = confined_execution::start_duplex_with_resources(
        plan,
        group,
        limits(),
        Box::new(DuplexResources(trace.clone())),
        cfg,
    )
    .unwrap();
    while duplex.recv().await.unwrap().is_some() {}
    let report = duplex.wait().await.unwrap();
    assert_eq!(report.outcome, Outcome::Failed);
    assert!(report
        .errors
        .iter()
        .any(|e| e.contains("duplex output exceeds bound")));
    assert!(report.cleanup_complete);
    assert!(matches!(fs::symlink_metadata(path),Err(e) if e.kind()==std::io::ErrorKind::NotFound));
    assert_eq!(*trace.lock().unwrap(), vec![true]);
    host.safe = true;
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires explicitly delegated FORGE_TEST_CGROUP_PARENT"]
async fn duplex_dropped_recv_and_oversize_send_cancel_owner() {
    for oversize in [false, true] {
        let mut host = DuplexHost::new();
        let (plan, group, path) = prepared(host.get(), support::task_worker(), &["sleep", "2000"]);
        let trace = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut duplex = duplex_start(plan, group, limits(), trace.clone());
        let mut completion = duplex.completion();
        if oversize {
            assert!(duplex.send(vec![0; 1024 * 1024 + 1]).await.is_err());
        } else {
            assert!(
                tokio::time::timeout(Duration::from_millis(100), duplex.recv())
                    .await
                    .is_err()
            );
        }
        drop(duplex);
        let terminal = tokio::time::timeout(Duration::from_secs(6), completion.wait())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(terminal.outcome, Outcome::Cancelled);
        assert!(terminal.cleanup_complete);
        assert!(
            matches!(fs::symlink_metadata(path),Err(e) if e.kind()==std::io::ErrorKind::NotFound)
        );
        assert_eq!(*trace.lock().unwrap(), vec![true]);
        host.safe = true;
    }
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires explicitly delegated FORGE_TEST_CGROUP_PARENT"]
async fn duplex_early_wait_cancels_without_discarding_buffered_output() {
    let mut host = DuplexHost::new();
    let (plan, group, path) = prepared(host.get(), support::task_worker(), &["output"]);
    let trace = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let duplex = duplex_start(plan, group, limits(), trace.clone());
    let mut completion = duplex.completion();
    let error = duplex.wait().await.unwrap_err();
    assert!(error.to_string().contains("drained"));
    let terminal = tokio::time::timeout(Duration::from_secs(6), completion.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(fs::symlink_metadata(path),Err(e) if e.kind()==std::io::ErrorKind::NotFound));
    if terminal.cleanup_complete {
        host.safe = true;
    }
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires explicitly delegated FORGE_TEST_CGROUP_PARENT and pidfd"]
async fn duplex_cancellation_reaps_owned_setsid_descendant_with_closed_pipes() {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    let mut host = DuplexHost::new();
    let (plan, group, path) = prepared(
        host.get(),
        support::task_worker(),
        &["setsid-wait", "5000", "child.pid", "owner.ack"],
    );
    let trace = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let duplex = duplex_start(
        plan,
        group,
        Limits {
            timeout: Duration::from_secs(8),
            ..limits()
        },
        trace.clone(),
    );
    let pid_path = host.get().0.join("scratch/child.pid");
    let pid: i32 = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Ok(text) = fs::read_to_string(&pid_path) {
                if let Ok(pid) = text.parse() {
                    break pid;
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
    assert!(
        raw >= 0,
        "pidfd_open failed: {}",
        std::io::Error::last_os_error()
    );
    let fd = unsafe { OwnedFd::from_raw_fd(raw as i32) };
    let mut observed = libc::pollfd {
        fd: fd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    assert_eq!(
        unsafe { libc::poll(&mut observed, 1, 0) },
        0,
        "fixture descendant already exited"
    );
    let expected_group = format!(
        "0::/{}",
        path.strip_prefix("/sys/fs/cgroup").unwrap().display()
    );
    let membership = fs::read_to_string(format!("/proc/{pid}/cgroup")).unwrap();
    assert!(
        membership.lines().any(|line| line == expected_group),
        "fixture PID is outside owned cgroup: {membership}"
    );
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
    let start = stat
        .rsplit_once(')')
        .unwrap()
        .1
        .split_whitespace()
        .nth(19)
        .unwrap()
        .to_owned();
    let current = fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
    assert_eq!(
        current
            .rsplit_once(')')
            .unwrap()
            .1
            .split_whitespace()
            .nth(19),
        Some(start.as_str())
    );
    fs::write(host.get().0.join("scratch/owner.ack"), b"ack").unwrap();
    let mut completion = duplex.completion();
    duplex.cancellation().cancel().unwrap();
    drop(duplex);
    let terminal = tokio::time::timeout(Duration::from_secs(6), completion.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(terminal.cleanup_complete);
    assert_eq!(
        unsafe { libc::poll(&mut observed, 1, 0) },
        1,
        "owned descendant did not exit"
    );
    assert!(matches!(fs::symlink_metadata(path),Err(e) if e.kind()==std::io::ErrorKind::NotFound));
    assert_eq!(*trace.lock().unwrap(), vec![true]);
    host.safe = true;
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires explicitly delegated FORGE_TEST_CGROUP_PARENT"]
async fn duplex_saturated_output_queue_fails_and_retains_uncertain_resources() {
    let host = DuplexHost::new();
    let (plan, group, path) = prepared(host.get(), support::task_worker(), &["output"]);
    let trace = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut cfg = duplex_limits();
    cfg.output_chunks = 1;
    let mut duplex = confined_execution::start_duplex_with_resources(
        plan,
        group,
        limits(),
        Box::new(DuplexResources(trace.clone())),
        cfg,
    )
    .unwrap();
    let mut completion = duplex.completion();
    let terminal = tokio::time::timeout(Duration::from_secs(6), completion.wait())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(terminal.outcome, Outcome::Failed);
    assert!(!terminal.cleanup_complete);
    assert!(matches!(fs::symlink_metadata(path),Err(e) if e.kind()==std::io::ErrorKind::NotFound));
    assert_eq!(*trace.lock().unwrap(), vec![false]);
    while duplex.recv().await.unwrap().is_some() {}
    let report = duplex.wait().await.unwrap();
    assert!(report
        .errors
        .iter()
        .any(|e| e.contains("duplex output queue full")));
    // The fixture guard retains the root because finalization was uncertain.
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires explicitly delegated FORGE_TEST_CGROUP_PARENT"]
async fn duplex_abandoned_client_drains_after_containment_and_cleans() {
    let mut host = DuplexHost::new();
    let (plan, group, path) = prepared(host.get(), support::task_worker(), &["duplex-lines"]);
    let trace = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut duplex = duplex_start(plan, group, limits(), trace.clone());
    duplex.send(b"ready\n".to_vec()).await.unwrap();
    assert_eq!(next_stdout(&mut duplex).await, b"ready\n");
    let mut completion = duplex.completion();
    drop(duplex);
    let terminal = tokio::time::timeout(Duration::from_secs(6), completion.wait())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(terminal.outcome, Outcome::Cancelled);
    assert!(terminal.cleanup_complete);
    assert!(matches!(fs::symlink_metadata(path),Err(e) if e.kind()==std::io::ErrorKind::NotFound));
    assert_eq!(*trace.lock().unwrap(), vec![true]);
    host.safe = true;
}
