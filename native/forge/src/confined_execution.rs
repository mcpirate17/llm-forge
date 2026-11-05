//! Own a sandbox launch through observation, cgroup cleanup and final reaping.
//! A dropped client signals cancellation; it never aborts the resource owner.
//! Runtime shutdown and uninterruptible kernel I/O require an outer watchdog.
#[path = "confined_execution_cancel.rs"]
mod cancellation;
#[path = "confined_execution_duplex.rs"]
mod duplex;
use crate::process_probe::readiness::{self, CapturePolicy, Input, Monitor, Observation};
use crate::{
    cgroup::Cgroup,
    process_io::open_pidfd,
    sandbox::{CommandPlan, ConfinedProcess, OwnedLaunch},
};
use anyhow::{ensure, Context, Result};
pub use cancellation::Cancellation;
pub use duplex::{Duplex, DuplexEvent, DuplexLimits};
use serde::Serialize;
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{oneshot, watch};
static EXECUTIONS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(32);

/// Extra resources transfer to the independent owner before launch. Once armed,
/// Drop must retain recovery data rather than remove paths a child may still use.
pub trait Resources: Send + 'static {
    fn arm(&mut self);
    fn finish(self: Box<Self>, cleanup_safe: bool) -> Result<()>;
    /// The owner fixes the disposition before calling this method. Existing
    /// resource implementations retain their cleanup-only behavior by default.
    fn finish_with_state(self: Box<Self>, state: FinishState) -> Result<()> {
        self.finish(state.cleanup_safe)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FinishState {
    pub cleanup_safe: bool,
    pub outcome: Outcome,
    pub exit_code: Option<i32>,
}

impl FinishState {
    pub fn succeeded(self) -> bool {
        self.cleanup_safe && self.outcome == Outcome::Exited && self.exit_code == Some(0)
    }
}
struct NoResources;
impl Resources for NoResources {
    fn arm(&mut self) {}
    fn finish(self: Box<Self>, _cleanup_safe: bool) -> Result<()> {
        Ok(())
    }
}

#[derive(Clone, Copy)]
pub struct Limits {
    pub timeout: Duration,
    pub max_output_chars: usize,
    pub cpu_seconds: Option<u64>,
    pub address_space_bytes: Option<u64>,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Exited,
    TimedOut,
    Cancelled,
    Failed,
}
#[derive(Debug, Serialize)]
pub struct Report {
    pub outcome: Outcome,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub stdout_bytes: u64,
    pub stderr_bytes: u64,
    pub duration_ms: u64,
    pub errors: Vec<String>,
    pub cleanup_complete: bool,
    pub resources: crate::cgroup::Report,
    pub cpu_seconds: Option<u64>,
    pub address_space_bytes: Option<u64>,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Terminal {
    pub outcome: Outcome,
    pub cleanup_complete: bool,
}
#[derive(Clone)]
pub struct Completion {
    receiver: watch::Receiver<Option<Terminal>>,
}
impl Completion {
    #[cfg(test)]
    pub(crate) fn test_channel() -> (watch::Sender<Option<Terminal>>, Self) {
        let (sender, receiver) = watch::channel(None);
        (sender, Self { receiver })
    }

    pub fn finished(&self) -> Option<Terminal> {
        *self.receiver.borrow()
    }
    pub async fn wait(&mut self) -> Result<Terminal> {
        loop {
            if let Some(terminal) = self.finished() {
                return Ok(terminal);
            }
            self.receiver
                .changed()
                .await
                .context("execution owner stopped without terminal cleanup evidence")?;
        }
    }
}
pub struct Execution {
    cancellation: Arc<Cancellation>,
    receiver: Option<oneshot::Receiver<Report>>,
    completed: bool,
    completion: Completion,
}
impl Execution {
    pub fn completion(&self) -> Completion {
        self.completion.clone()
    }
    pub fn cancellation(&self) -> Arc<Cancellation> {
        self.cancellation.clone()
    }
    pub async fn wait(mut self) -> Result<Report> {
        let report = self
            .receiver
            .take()
            .context("execution already awaited")?
            .await
            .context("execution owner stopped without a cleanup report")?;
        self.completed = true;
        Ok(report)
    }
}
impl Drop for Execution {
    fn drop(&mut self) {
        if !self.completed {
            if let Err(error) = self.cancellation.cancel() {
                eprintln!("forge: execution cancellation failed: {error:#}");
            }
        }
    }
}
enum Process {
    Running(ConfinedProcess),
    LaunchFailed(OwnedLaunch),
}
impl Process {
    async fn stop(&mut self) -> Result<ExitStatus> {
        match self {
            Self::Running(p) => p.stop().await,
            Self::LaunchFailed(p) => Ok(p.stop()?),
        }
    }
}
struct Observed {
    process: Process,
    monitor: Option<Monitor<'static>>,
    duplex: Option<duplex::Driver>,
    pidfd: Option<OwnedFd>,
}

/// `group` must be a newly created, unconfigured owned delegation. Admission
/// runs before launch; all post-launch errors go through the independent owner.
pub fn start(plan: CommandPlan, group: Cgroup, limits: Limits) -> Result<Execution> {
    start_with_resources(plan, group, limits, Box::new(NoResources))
}
pub fn start_with_resources(
    plan: CommandPlan,
    group: Cgroup,
    limits: Limits,
    resources: Box<dyn Resources>,
) -> Result<Execution> {
    start_inner(
        plan,
        group,
        limits,
        resources,
        None,
        CapturePolicy::TailChars(limits.max_output_chars),
        None,
    )
}

/// Capture complete legacy mutation output with independent raw byte limits.
/// Stdout is bounded at 16 MiB and stderr at 1 MiB; overflows fail through the
/// owning cleanup actor. Invalid UTF-8 uses the legacy replacement behavior.
/// `max_output_chars` applies only to the default tail mode and is unused here.
pub fn start_capture_with_resources(
    plan: CommandPlan,
    group: Cgroup,
    limits: Limits,
    resources: Box<dyn Resources>,
) -> Result<Execution> {
    start_inner(
        plan,
        group,
        limits,
        resources,
        None,
        CapturePolicy::CompleteSeparate {
            stdout: 16 * 1024 * 1024,
            stderr: 1024 * 1024,
        },
        None,
    )
}

/// Run a bounded UTF-8 filter without changing line endings or trimming output.
/// Input and combined raw stdout/stderr are each limited to 1 MiB. Invalid UTF-8,
/// incomplete stdin delivery, and output overflow fail through the owning actor.
/// `max_output_chars` is unused for this opt-in byte capture mode.
pub fn start_filter_with_resources(
    plan: CommandPlan,
    group: Cgroup,
    limits: Limits,
    resources: Box<dyn Resources>,
    input: Vec<u8>,
    max_output_bytes: usize,
) -> Result<Execution> {
    ensure!(input.len() <= 1024 * 1024, "probe stdin exceeds 1 MiB");
    start_inner(
        plan,
        group,
        limits,
        resources,
        Some(input),
        CapturePolicy::StrictCombined(max_output_bytes),
        None,
    )
}

/// Launch persistent bounded byte streams under the same owner and cleanup path.
/// Dropping the client or an in-flight operation cancels the endpoint. Stream
/// EOF alone does not prove exit or cleanup; await Completion or wait().
pub fn start_duplex_with_resources(
    mut plan: CommandPlan,
    group: Cgroup,
    limits: Limits,
    resources: Box<dyn Resources>,
    stream_limits: DuplexLimits,
) -> Result<Duplex> {
    let (seed, client) = duplex::prepare(&mut plan, stream_limits)?;
    let execution = start_inner(
        plan,
        group,
        limits,
        resources,
        None,
        CapturePolicy::TailChars(1),
        Some(seed),
    )?;
    Ok(client.bind(execution))
}
struct PreparedInput {
    one_shot: Option<Input<'static>>,
    duplex: Option<duplex::Seed>,
}

fn start_inner(
    mut plan: CommandPlan,
    mut group: Cgroup,
    mut limits: Limits,
    mut resources: Box<dyn Resources>,
    bytes: Option<Vec<u8>>,
    capture: CapturePolicy,
    duplex: Option<duplex::Seed>,
) -> Result<Execution> {
    tokio::runtime::Handle::try_current().context("execution requires a Tokio reactor")?;
    let admission = EXECUTIONS
        .try_acquire()
        .context("execution capacity of 32 exhausted")?;
    ensure!(
        limits.timeout <= Duration::from_secs(86400),
        "execution wall timeout exceeds one day"
    );
    readiness::deadline(limits.timeout)?;
    capture.validate()?;
    limits.cpu_seconds = limit_value(libc::RLIMIT_CPU, limits.cpu_seconds)?;
    limits.address_space_bytes = limit_value(libc::RLIMIT_AS, limits.address_space_bytes)?;
    let one_shot = if duplex.is_none() {
        Some(match bytes {
            Some(bytes) => Input::prepare_owned(plan.command(), bytes)?,
            None => Input::prepare(plan.command(), None)?,
        })
    } else {
        None
    };
    let input = PreparedInput { one_shot, duplex };
    plan.command().stdout(Stdio::piped()).stderr(Stdio::piped());
    configure_rlimits(plan.command(), limits);
    group.configure_command(plan.command())?;
    let cancellation = Arc::new(Cancellation::new()?);
    let (sender, receiver) = oneshot::channel();
    let (terminal_sender, terminal_receiver) = watch::channel(None);
    let cancel = cancellation.clone();
    tokio::spawn(async move {
        let _admission = admission;
        let started = Instant::now();
        resources.arm();
        let mut report = run(plan, group, input, limits, capture, cancel.clone()).await;
        // A cancellation observed before this point wins over normal exit.
        // After this point finalization owns the disposition and is irreversible.
        if cancel.is_cancelled() && report.outcome == Outcome::Exited {
            report.outcome = Outcome::Cancelled;
        }
        let state = FinishState {
            cleanup_safe: report.cleanup_complete,
            outcome: report.outcome,
            exit_code: report.exit_code,
        };
        let completion = tokio::task::spawn_blocking(move || {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                resources.finish_with_state(state)
            }))
            .map_err(|_| anyhow::anyhow!("execution resource cleanup panicked"))?
        })
        .await
        .context("execution resource cleanup task failed")
        .and_then(|r| r);
        if let Err(error) = completion {
            report.cleanup_complete = false;
            report.outcome = Outcome::Failed;
            report.errors.push(format!("workspace cleanup: {error:#}"));
        }
        report.duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        terminal_sender.send_replace(Some(Terminal {
            outcome: report.outcome,
            cleanup_complete: report.cleanup_complete,
        }));
        // A departed client does not change cleanup obligations.
        if let Err(report) = sender.send(report) {
            if !report.errors.is_empty() {
                eprintln!(
                    "forge: departed execution client; {}",
                    report.errors.join("; ")
                );
            }
        }
    });
    Ok(Execution {
        cancellation,
        receiver: Some(receiver),
        completed: false,
        completion: Completion {
            receiver: terminal_receiver,
        },
    })
}
pub(crate) fn limit_value(
    resource: libc::__rlimit_resource_t,
    requested: Option<u64>,
) -> Result<Option<u64>> {
    let Some(requested) = requested else {
        return Ok(None);
    };
    ensure!(
        requested > 0 && requested != libc::RLIM_INFINITY,
        "invalid process resource limit"
    );
    let mut inherited = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: getrlimit writes one initialized owned value.
    ensure!(
        unsafe { libc::getrlimit(resource, &mut inherited) } == 0,
        std::io::Error::last_os_error()
    );
    Ok(Some(requested.min(inherited.rlim_max)))
}
pub(crate) fn configure_rlimits(command: &mut std::process::Command, limits: Limits) {
    // SAFETY: pre-exec uses only fixed scalar state and libc resource syscalls.
    unsafe {
        command.pre_exec(move || {
            for (resource, value) in [
                (libc::RLIMIT_CPU, limits.cpu_seconds),
                (libc::RLIMIT_AS, limits.address_space_bytes),
            ] {
                if let Some(value) = value {
                    let value = libc::rlimit {
                        rlim_cur: value,
                        rlim_max: value,
                    };
                    if libc::setrlimit(resource, &value) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                }
            }
            Ok(())
        });
    }
}
fn prepare_monitor(
    owned: &mut Observed,
    group: &Cgroup,
    input: PreparedInput,
    capture: CapturePolicy,
) -> Result<()> {
    let Process::Running(process) = &mut owned.process else {
        anyhow::bail!("launch did not attach a broker")
    };
    group.verify_member(process.child())?;
    owned.pidfd = Some(open_pidfd(process.child())?);
    let out = process
        .take_stdout()
        .context("execution stdout missing")?
        .into();
    let err = process
        .take_stderr()
        .context("execution stderr missing")?
        .into();
    if let Some(seed) = input.duplex {
        owned.duplex = Some(duplex::Driver::new(seed, out, err)?);
    } else {
        owned.monitor = Some(Monitor::new(
            out,
            err,
            input.one_shot.context("execution input missing")?,
            capture,
        )?);
    }
    Ok(())
}
async fn observe(
    slot: Arc<Mutex<Option<Observed>>>,
    cutoff: Instant,
    cancel: Arc<Cancellation>,
) -> Result<Observation> {
    tokio::task::spawn_blocking(move || {
        // The actor retains another Arc: queued-task cancellation never destroys
        // the only process owner. The guard itself lives outside the unwind scope.
        let mut guard = slot
            .lock()
            .map_err(|_| anyhow::anyhow!("execution owner lock poisoned"))?;
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let owned = guard.as_mut().context("execution owner missing")?;
            let fd = owned
                .pidfd
                .as_ref()
                .context("execution pidfd missing")?
                .as_raw_fd();
            let observation = if let Some(driver) = owned.duplex.as_mut() {
                driver.observe(fd, cutoff, &cancel)?
            } else {
                let monitor = owned
                    .monitor
                    .as_mut()
                    .context("execution monitor missing")?;
                monitor.observe(fd, cutoff, Some(cancel.as_raw_fd()), || {
                    Ok(cancel.is_cancelled())
                })?
            };
            Ok(observation)
        }))
        .map_err(|_| anyhow::anyhow!("execution monitor panicked"))?
    })
    .await
    .context("execution observer task failed")?
}
async fn run(
    plan: CommandPlan,
    mut group: Cgroup,
    input: PreparedInput,
    limits: Limits,
    capture: CapturePolicy,
    cancel: Arc<Cancellation>,
) -> Report {
    let started = Instant::now();
    let mut report = empty_report(&group, limits);
    let mut owned = None;
    if cancel.is_cancelled() {
        report.outcome = Outcome::Cancelled;
    } else {
        match plan.spawn_owned() {
            Ok(process) => {
                owned = Some(Observed {
                    process: Process::Running(process),
                    monitor: None,
                    duplex: None,
                    pidfd: None,
                })
            }
            Err(failure) => {
                report.errors.push(format!("launch: {:#}", failure.error));
                owned = failure.process.map(|process| Observed {
                    process: Process::LaunchFailed(process),
                    monitor: None,
                    duplex: None,
                    pidfd: None,
                });
            }
        }
    }
    if report.errors.is_empty() {
        if let Some(state) = owned.as_mut() {
            match prepare_monitor(state, &group, input, capture) {
                Err(error) => report
                    .errors
                    .push(format!("monitor preparation: {error:#}")),
                Ok(()) => {
                    let slot = Arc::new(Mutex::new(owned.take()));
                    match observe(slot.clone(), started + limits.timeout, cancel.clone()).await {
                        Ok(Observation::Exited) => report.outcome = Outcome::Exited,
                        Ok(Observation::TimedOut) => report.outcome = Outcome::TimedOut,
                        Ok(Observation::Cancelled) => report.outcome = Outcome::Cancelled,
                        Err(error) => report.errors.push(format!("observation: {error:#}")),
                    }
                    owned = match slot.lock() {
                        Ok(mut state) => state.take(),
                        Err(error) => {
                            report
                                .errors
                                .push("execution owner lock poisoned after observation".into());
                            error.into_inner().take()
                        }
                    };
                }
            }
        }
    }
    cleanup(&mut group, owned, &mut report, cancel).await;
    if !report.errors.is_empty() {
        report.outcome = Outcome::Failed;
    }
    report.duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    report
}
async fn cleanup(
    group: &mut Cgroup,
    mut owned: Option<Observed>,
    report: &mut Report,
    cancel: Arc<Cancellation>,
) {
    let mut complete = true;
    if let Err(error) = group.finish(Duration::from_secs(2)).await {
        complete = false;
        report.errors.push(format!("cgroup cleanup: {error:#}"));
    }
    if let Some(state) = &mut owned {
        match state.process.stop().await {
            Ok(status) => {
                report.exit_code = status.code();
                report.signal = status.signal();
            }
            Err(error) => {
                complete = false;
                report.errors.push(format!("process cleanup: {error:#}"));
            }
        }
        if let Some(mut driver) = state.duplex.take() {
            if report.outcome == Outcome::Exited && report.exit_code == Some(0) {
                if let Err(error) = driver.require_input_complete() {
                    report
                        .errors
                        .push(format!("duplex stdin delivery: {error:#}"));
                }
            }
            let cleanup_safe = complete;
            let drained = tokio::task::spawn_blocking(move || {
                let result = driver.drain(Duration::from_secs(2), &cancel, cleanup_safe);
                (result, driver.bytes())
            })
            .await;
            match drained {
                Ok((result, (out, err))) => {
                    report.stdout_bytes = out;
                    report.stderr_bytes = err;
                    if let Err(error) = result {
                        complete = false;
                        report
                            .errors
                            .push(format!("duplex residual output: {error:#}"));
                    }
                }
                Err(error) => {
                    complete = false;
                    report.errors.push(format!("duplex drain task: {error:#}"));
                }
            }
        }
        if let Some(monitor) = state.monitor.take() {
            // A filter may intentionally reject stdin with a natural nonzero
            // exit or signal. Only successful exits promise full delivery.
            if report.outcome == Outcome::Exited && report.exit_code == Some(0) {
                if let Err(error) = monitor.require_stdin_consumed() {
                    report.errors.push(format!("stdin delivery: {error:#}"));
                }
            }
            let captured = match drain(monitor).await {
                Ok((captured, error)) => {
                    if let Some(error) = error {
                        complete = false;
                        report.errors.push(error);
                    }
                    Some(captured)
                }
                Err(error) => {
                    complete = false;
                    report
                        .errors
                        .push(format!("residual output task: {error:#}"));
                    None
                }
            };
            if let Some(captured) = captured {
                report.stdout_bytes = captured.stdout_bytes;
                report.stderr_bytes = captured.stderr_bytes;
                // Tail and strict filter behavior stay unchanged. Only complete
                // legacy capture decodes invalid bytes with replacement.
                match decode_capture(captured) {
                    Ok((out, err)) => {
                        report.stdout = out;
                        report.stderr = err;
                    }
                    Err(_) => {
                        complete = false;
                        report
                            .errors
                            .push("native output contained invalid UTF-8".into());
                    }
                }
            }
        }
    }
    report.cleanup_complete = complete;
}
fn decode_lossy(bytes: Vec<u8>) -> String {
    match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(error) => String::from_utf8_lossy(error.as_bytes()).into_owned(),
    }
}
fn decode_capture(captured: readiness::Captured) -> Result<(String, String)> {
    if captured.lossy_utf8 {
        return Ok((decode_lossy(captured.stdout), decode_lossy(captured.stderr)));
    }
    Ok((
        String::from_utf8(captured.stdout)?,
        String::from_utf8(captured.stderr)?,
    ))
}
async fn drain(mut monitor: Monitor<'static>) -> Result<(readiness::Captured, Option<String>)> {
    tokio::task::spawn_blocking(move || {
        let error = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            monitor.drain_to_eof(Duration::from_secs(2))
        })) {
            Ok(Ok(())) => None,
            Ok(Err(error)) => Some(format!("residual output: {error:#}")),
            Err(_) => Some("residual output drain panicked".into()),
        };
        (monitor.finish(), error)
    })
    .await
    .context("residual output worker failed")
}
fn empty_report(group: &Cgroup, limits: Limits) -> Report {
    Report {
        outcome: Outcome::Failed,
        exit_code: None,
        signal: None,
        stdout: String::new(),
        stderr: String::new(),
        stdout_bytes: 0,
        stderr_bytes: 0,
        duration_ms: 0,
        errors: Vec::new(),
        cleanup_complete: false,
        resources: group.report().clone(),
        cpu_seconds: limits.cpu_seconds,
        address_space_bytes: limits.address_space_bytes,
    }
}

#[cfg(test)]
mod capture_decoding_tests {
    use super::*;
    fn captured(lossy_utf8: bool) -> readiness::Captured {
        readiness::Captured {
            stdout: b"left\xffright".to_vec(),
            stderr: b"bad\xfe".to_vec(),
            stdout_bytes: 10,
            stderr_bytes: 4,
            lossy_utf8,
        }
    }
    #[test]
    fn complete_capture_reuses_valid_utf8_buffer() {
        let bytes = b"valid json".to_vec();
        let pointer = bytes.as_ptr();
        let text = decode_lossy(bytes);
        assert_eq!(text, "valid json");
        assert_eq!(text.as_ptr(), pointer);
    }
    #[test]
    fn complete_capture_decodes_lossily_but_strict_capture_rejects_invalid_utf8() {
        let (out, err) = decode_capture(captured(true)).unwrap();
        assert_eq!(out, "left\u{fffd}right");
        assert_eq!(err, "bad\u{fffd}");
        assert!(decode_capture(captured(false)).is_err());
    }
}
