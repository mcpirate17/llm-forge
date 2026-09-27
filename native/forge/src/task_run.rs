//! Explicit, bounded command execution for the native task journal. This is
//! process supervision, not a filesystem/network sandbox or approval authority.

use crate::task_limits::{LimitScope, LimitsReport};
use crate::task_resources::{child_exit_status, ResourceRequest, ResourceSnapshot};
use crate::task_store::{Completion, State, Store, Task};
use crate::tasks::{self, RunArgs};
use anyhow::{ensure, Context, Result};
use serde::Serialize;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

#[derive(Serialize, Default)]
struct LogSummary {
    retained_bytes: u64,
    discarded_bytes: u64,
}

#[derive(Serialize)]
struct ResourceEvidence {
    requested: ResourceRequest,
    available: ResourceSnapshot,
    enforcement: Option<LimitsReport>,
}

#[derive(Serialize)]
struct Outcome {
    schema_version: u32,
    task_id: String,
    attempt: u32,
    owner: String,
    session_id: Option<String>,
    claim_id: Option<String>,
    message_id: Option<String>,
    started_at: i64,
    finished_at: i64,
    exit_code: Option<i32>,
    timed_out: bool,
    succeeded: bool,
    detail: String,
    resources: ResourceEvidence,
    stdout: Option<LogSummary>,
    stderr: Option<LogSummary>,
}

struct ManagedChild {
    child: Child,
    group_stopped: bool,
    scope_stopped: bool,
    scope: Option<LimitScope>,
}

static INTERRUPTED: AtomicBool = AtomicBool::new(false);

#[cfg(unix)]
extern "C" fn interrupt(_signal: libc::c_int) {
    INTERRUPTED.store(true, Ordering::Release);
}

struct SignalGuard {
    #[cfg(unix)]
    previous: Vec<(libc::c_int, libc::sigaction)>,
}

impl SignalGuard {
    fn install() -> Result<Self> {
        INTERRUPTED.store(false, Ordering::Release);
        #[cfg(unix)]
        {
            let mut guard = Self {
                previous: Vec::new(),
            };
            for signal in [libc::SIGINT, libc::SIGTERM] {
                let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
                let mut previous: libc::sigaction = unsafe { std::mem::zeroed() };
                action.sa_sigaction = interrupt as *const () as usize;
                unsafe {
                    libc::sigemptyset(&mut action.sa_mask);
                }
                ensure!(
                    unsafe { libc::sigaction(signal, &action, &mut previous) } == 0,
                    "installing task signal handler: {}",
                    std::io::Error::last_os_error()
                );
                guard.previous.push((signal, previous));
            }
            Ok(guard)
        }
        #[cfg(not(unix))]
        {
            Ok(Self {})
        }
    }
}

impl Drop for SignalGuard {
    fn drop(&mut self) {
        #[cfg(unix)]
        for (signal, previous) in &self.previous {
            if unsafe { libc::sigaction(*signal, previous, std::ptr::null_mut()) } != 0 {
                eprintln!(
                    "forge task: failed restoring signal handler: {}",
                    std::io::Error::last_os_error()
                );
            }
        }
    }
}

impl ManagedChild {
    fn stop(&mut self) -> Result<()> {
        // A detached descendant may outlive both the command and its wrapper.
        let scope_result = if self.scope_stopped {
            Ok(None)
        } else {
            self.scope.as_ref().map(LimitScope::stop).transpose()
        };
        self.scope_stopped = scope_result.is_ok();
        let group_result = self.stop_group();
        match (scope_result, group_result) {
            (Err(scope), Err(group)) => {
                anyhow::bail!("scope cleanup: {scope:#}; group cleanup: {group:#}")
            }
            (Err(error), _) | (_, Err(error)) => Err(error),
            _ => Ok(()),
        }
    }

    fn stop_group(&mut self) -> Result<()> {
        if self.group_stopped {
            return Ok(());
        }
        #[cfg(unix)]
        {
            // The child was created in its own process group, so killing its
            // descendants cannot affect the caller's shell or another task.
            let result = unsafe { libc::kill(-(self.child.id() as i32), libc::SIGKILL) };
            if result != 0 {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::ESRCH) {
                    return Err(error.into());
                }
            }
        }
        #[cfg(not(unix))]
        if self.child.try_wait()?.is_none() {
            self.child.kill()?;
        }
        self.child.wait()?;
        self.group_stopped = true;
        Ok(())
    }
}

impl Drop for ManagedChild {
    fn drop(&mut self) {
        if let Err(error) = self.stop() {
            eprintln!("forge task: child cleanup failed: {error:#}");
        }
    }
}

#[cfg(unix)]
fn nonblocking<R: std::os::fd::AsRawFd>(input: &R) -> Result<()> {
    let fd = input.as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    ensure!(
        flags >= 0,
        "reading pipe flags: {}",
        std::io::Error::last_os_error()
    );
    ensure!(
        unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } >= 0,
        "setting nonblocking pipe: {}",
        std::io::Error::last_os_error()
    );
    Ok(())
}

fn output_thread<R: Read + Send + 'static>(
    mut input: R,
    mut output: File,
    limit: u64,
    done: Arc<AtomicBool>,
) -> JoinHandle<Result<LogSummary>> {
    std::thread::spawn(move || {
        let mut summary = LogSummary::default();
        let mut buffer = [0u8; 8192];
        let mut stopping = None;
        loop {
            if done.load(Ordering::Acquire) {
                let at = stopping.get_or_insert_with(Instant::now);
                if at.elapsed() > Duration::from_millis(200) {
                    break;
                }
            }
            let count = match input.read(&mut buffer) {
                Ok(count) => count,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if stopping.is_some() {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error.into()),
            };
            if count == 0 {
                break;
            }
            let keep = (limit.saturating_sub(summary.retained_bytes)).min(count as u64) as usize;
            output.write_all(&buffer[..keep])?;
            summary.retained_bytes += keep as u64;
            summary.discarded_bytes += (count - keep) as u64;
        }
        output.sync_all()?;
        Ok(summary)
    })
}

fn output_file(directory: &Path, name: &str) -> Result<File> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(directory.join(name))
        .with_context(|| format!("creating task output {name}"))
}

fn spawn(
    host: &Path,
    task: &Task,
    args: &RunArgs,
    directory: &Path,
    available: &ResourceSnapshot,
    mut scope: Option<LimitScope>,
) -> Result<ManagedChild> {
    let (program, arguments) = task
        .command
        .split_first()
        .context("task has no stored command")?;
    let mut command = match scope.as_mut() {
        Some(scope) => scope.command(program, arguments, directory),
        None => {
            let mut command = Command::new(program);
            command.args(arguments);
            command
        }
    };
    args.resources.configure_command(&mut command, available)?;
    command
        .current_dir(host)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("FORGE_TASK_ID", &task.id)
        .env("FORGE_TASK_ATTEMPT", task.attempt.to_string())
        .env("FORGE_TASK_OWNER", &args.lease.task.owner);
    if let Some(session) = &task.session_id {
        command.env("FORGE_SESSION_ID", session);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
        #[cfg(target_os = "linux")]
        {
            let parent = std::process::id() as libc::pid_t;
            // Only async-signal-safe libc calls in the child before exec.
            // Direct child death is guaranteed even if the supervisor is killed;
            // detached descendants still require an OS service/cgroup boundary.
            unsafe {
                command.pre_exec(move || {
                    if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    if libc::getppid() != parent {
                        libc::kill(libc::getpid(), libc::SIGKILL);
                    }
                    Ok(())
                });
            }
        }
    }
    ensure!(
        !INTERRUPTED.load(Ordering::Acquire),
        "task interrupted before launch"
    );
    Ok(ManagedChild {
        child: command
            .spawn()
            .with_context(|| format!("spawning task executable {program:?}"))?,
        group_stopped: false,
        scope_stopped: false,
        scope,
    })
}

fn monitor(
    host: &Path,
    store: &mut Store,
    task: &Task,
    child: &mut ManagedChild,
    args: &RunArgs,
) -> Result<(Option<i32>, bool)> {
    let began = Instant::now();
    let heartbeat_interval =
        Duration::from_millis((args.lease.lease_seconds as u64 * 1000 / 3).max(100));
    let mut heartbeat_at = Instant::now();
    loop {
        ensure!(
            !INTERRUPTED.load(Ordering::Acquire),
            "task interrupted by signal"
        );
        let remaining = Duration::from_secs(args.timeout_seconds).saturating_sub(began.elapsed());
        if remaining.is_zero() {
            return Ok((None, true));
        }
        if heartbeat_at.elapsed() >= heartbeat_interval {
            tasks::validate_claim(host, task, &args.lease.task.owner)?;
            // Contention must never hold a running child beyond its deadline.
            store.set_busy_timeout(remaining.min(Duration::from_millis(200)))?;
            store.heartbeat(
                &task.id,
                &args.lease.task.owner,
                task.attempt,
                args.lease.lease_seconds,
                tasks::now(),
            )?;
            heartbeat_at = Instant::now();
        }
        if began.elapsed() >= Duration::from_secs(args.timeout_seconds) {
            return Ok((None, true));
        }
        ensure!(
            !INTERRUPTED.load(Ordering::Acquire),
            "task interrupted by signal"
        );
        // Keep the exited leader unreaped until cleanup, pinning the process
        // group identity while scope management commands finish.
        if let Some(status) = child_exit_status(&child.child)? {
            return Ok((status.code(), false));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn execute(
    host: &Path,
    store: &mut Store,
    task: &Task,
    args: &RunArgs,
    directory: &Path,
    available: &ResourceSnapshot,
    scope: Option<LimitScope>,
) -> Result<Outcome> {
    let mut outcome = initial_outcome(task, args, available, scope.as_ref());
    let out = output_file(directory, "stdout.log")?;
    let err = output_file(directory, "stderr.log")?;
    let mut child = spawn(host, task, args, directory, available, scope)?;
    let stdout_pipe = child.child.stdout.take().context("task stdout pipe")?;
    let stderr_pipe = child.child.stderr.take().context("task stderr pipe")?;
    #[cfg(unix)]
    {
        nonblocking(&stdout_pipe)?;
        nonblocking(&stderr_pipe)?;
    }
    let done = Arc::new(AtomicBool::new(false));
    let stdout = output_thread(stdout_pipe, out, args.log_bytes, done.clone());
    let stderr = output_thread(stderr_pipe, err, args.log_bytes, done.clone());
    let result = monitor(host, store, task, &mut child, args);
    // Scope teardown uses several bounded management probes. Reserve enough
    // lease time to stop descendants and bind the receipt even with a 2s lease.
    let completion_lease = if child.scope.is_some() {
        store
            .set_busy_timeout(Duration::from_millis(200))
            .and_then(|()| {
                store.heartbeat(
                    &task.id,
                    &args.lease.task.owner,
                    task.attempt,
                    args.lease.lease_seconds.max(30),
                    tasks::now(),
                )
            })
            .map(|_| ())
    } else {
        Ok(())
    };
    // Also close inherited pipes held by descendants after the leader exits.
    let stopped = child.stop();
    done.store(true, Ordering::Release);
    match result {
        Ok((exit_code, timed_out)) => {
            outcome.exit_code = exit_code;
            outcome.timed_out = timed_out;
            outcome.succeeded = exit_code == Some(0) && !timed_out;
            outcome.detail = if timed_out {
                "wall-clock deadline exceeded".into()
            } else {
                format!(
                    "command exited {}",
                    exit_code.map_or("by signal".into(), |code| code.to_string())
                )
            };
        }
        Err(error) => outcome.detail = format!("execution failed: {error:#}"),
    }
    if let Err(error) = completion_lease {
        record_failure(&mut outcome, "completion lease", error);
    }
    outcome.stdout = join_output(stdout, "stdout", &mut outcome);
    outcome.stderr = join_output(stderr, "stderr", &mut outcome);
    if let Err(error) = stopped {
        record_failure(&mut outcome, "cleanup", error);
    }
    if let Some(scope) = &child.scope {
        match scope.report() {
            Ok(report) => outcome.resources.enforcement = Some(report),
            Err(error) => record_failure(&mut outcome, "resource verification", error),
        }
    }
    outcome.finished_at = tasks::now();
    Ok(outcome)
}

fn record_failure(outcome: &mut Outcome, operation: &str, error: anyhow::Error) {
    outcome.succeeded = false;
    outcome
        .detail
        .push_str(&format!("; {operation} failed: {error:#}"));
}

fn join_output(
    reader: JoinHandle<Result<LogSummary>>,
    name: &str,
    outcome: &mut Outcome,
) -> Option<LogSummary> {
    match reader
        .join()
        .unwrap_or_else(|_| Err(anyhow::anyhow!("reader panicked")))
    {
        Ok(summary) => Some(summary),
        Err(error) => {
            record_failure(outcome, name, error);
            None
        }
    }
}

fn initial_outcome(
    task: &Task,
    args: &RunArgs,
    available: &ResourceSnapshot,
    scope: Option<&LimitScope>,
) -> Outcome {
    Outcome {
        schema_version: 2,
        task_id: task.id.clone(),
        attempt: task.attempt,
        owner: args.lease.task.owner.clone(),
        session_id: task.session_id.clone(),
        claim_id: task.claim_id.clone(),
        message_id: task.message_id.clone(),
        started_at: task.updated_at,
        finished_at: tasks::now(),
        exit_code: None,
        timed_out: false,
        succeeded: false,
        detail: "execution did not start".into(),
        resources: ResourceEvidence {
            requested: args.resources.clone(),
            available: available.clone(),
            enforcement: scope.map(LimitScope::unverified_report),
        },
        stdout: None,
        stderr: None,
    }
}

fn persist(directory: &Path, outcome: &Outcome) -> Result<PathBuf> {
    let path = directory.join("receipt.json");
    let mut file = output_file(directory, "receipt.json")?;
    serde_json::to_writer_pretty(&mut file, outcome)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    File::open(directory)?.sync_all()?;
    Ok(path)
}

fn completion(outcome: Outcome, receipt: crate::task_store::Receipt) -> (Completion, u8) {
    let success = outcome.succeeded;
    (
        Completion {
            state: if success {
                State::Succeeded
            } else {
                State::Failed
            },
            exit_code: outcome.exit_code,
            detail: Some(outcome.detail),
            receipt: Some(receipt),
        },
        if success { 0 } else { 1 },
    )
}

pub fn run(host: &Path, store: &mut Store, args: RunArgs) -> Result<u8> {
    ensure!(
        cfg!(target_os = "linux"),
        "supervised task execution currently requires Linux resource admission and process groups"
    );
    let _signals = SignalGuard::install()?;
    ensure!(
        (1..=86400).contains(&args.timeout_seconds),
        "timeout-seconds must be 1..86400"
    );
    ensure!(
        args.log_bytes <= 64 * 1024 * 1024,
        "log-bytes must be at most 64 MiB per stream"
    );
    let before = store.get(&args.lease.task.id)?;
    ensure!(
        !before.command.is_empty(),
        "task has no command; use start/finish for external work"
    );
    tasks::validate_claim(host, &before, &args.lease.task.owner)?;
    // Admission and backend readiness precede retry/start: refusals do not spend
    // an attempt or alter a failed task's recovery state.
    let available = args.resources.admit()?;
    let scope = LimitScope::prepare(
        host,
        &before.id,
        &args.resources,
        &available,
        args.timeout_seconds,
    )?;
    ensure!(
        !INTERRUPTED.load(Ordering::Acquire),
        "task interrupted during resource preflight"
    );
    if args.resume {
        store.retry(&before.id, &args.lease.task.owner, tasks::now())?;
    }
    let task = store.start(
        &before.id,
        &args.lease.task.owner,
        args.lease.lease_seconds,
        tasks::now(),
    )?;
    let directory = host
        .join(".agents/forge/runs")
        .join(&task.id)
        .join(task.attempt.to_string());
    let outcome = (|| {
        std::fs::create_dir_all(&directory)?;
        let fallback = initial_outcome(&task, &args, &available, scope.as_ref());
        let outcome = match execute(host, store, &task, &args, &directory, &available, scope) {
            Ok(outcome) => outcome,
            Err(error) => Outcome {
                finished_at: tasks::now(),
                detail: format!("execution failed: {error:#}"),
                ..fallback
            },
        };
        let receipt = tasks::bind_receipt(host, &persist(&directory, &outcome)?)?;
        Ok::<_, anyhow::Error>((outcome, receipt))
    })();
    let (completion, code) = match outcome {
        Ok((outcome, receipt)) => completion(outcome, receipt),
        Err(error) => (
            Completion {
                state: State::Failed,
                exit_code: None,
                detail: Some(format!("{error:#}")),
                receipt: None,
            },
            1,
        ),
    };
    let task = store.finish(
        &task.id,
        &args.lease.task.owner,
        task.attempt,
        completion,
        tasks::now(),
    )?;
    println!("{}", serde_json::to_string(&task)?);
    Ok(code)
}
