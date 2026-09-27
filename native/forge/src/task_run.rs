//! Explicit, bounded command execution for the native task journal. This is
//! process supervision, not a filesystem/network sandbox or approval authority.

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
    detail: String,
    stdout: LogSummary,
    stderr: LogSummary,
}

struct ManagedChild(Child, bool);

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
        if self.1 {
            return Ok(());
        }
        #[cfg(unix)]
        {
            // The child was created in its own process group, so killing its
            // descendants cannot affect the caller's shell or another task.
            let result = unsafe { libc::kill(-(self.0.id() as i32), libc::SIGKILL) };
            if result != 0 {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::ESRCH) {
                    return Err(error.into());
                }
            }
        }
        #[cfg(not(unix))]
        if self.0.try_wait()?.is_none() {
            self.0.kill()?;
        }
        self.0.wait()?;
        self.1 = true;
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

fn spawn(host: &Path, task: &Task, owner: &str) -> Result<ManagedChild> {
    let (program, arguments) = task
        .command
        .split_first()
        .context("task has no stored command")?;
    let mut command = Command::new(program);
    command
        .args(arguments)
        .current_dir(host)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("FORGE_TASK_ID", &task.id)
        .env("FORGE_TASK_ATTEMPT", task.attempt.to_string())
        .env("FORGE_TASK_OWNER", owner);
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
    Ok(ManagedChild(
        command
            .spawn()
            .with_context(|| format!("spawning task executable {program:?}"))?,
        false,
    ))
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
        if let Some(status) = child.0.try_wait()? {
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
) -> Result<Outcome> {
    let out = output_file(directory, "stdout.log")?;
    let err = output_file(directory, "stderr.log")?;
    let mut child = spawn(host, task, &args.lease.task.owner)?;
    let stdout_pipe = child.0.stdout.take().context("task stdout pipe")?;
    let stderr_pipe = child.0.stderr.take().context("task stderr pipe")?;
    #[cfg(unix)]
    {
        nonblocking(&stdout_pipe)?;
        nonblocking(&stderr_pipe)?;
    }
    let done = Arc::new(AtomicBool::new(false));
    let stdout = output_thread(stdout_pipe, out, args.log_bytes, done.clone());
    let stderr = output_thread(stderr_pipe, err, args.log_bytes, done.clone());
    let result = monitor(host, store, task, &mut child, args);
    // Also close inherited pipes held by descendants after the leader exits.
    let stopped = child.stop();
    done.store(true, Ordering::Release);
    let stdout = stdout
        .join()
        .map_err(|_| anyhow::anyhow!("stdout reader panicked"))?;
    let stderr = stderr
        .join()
        .map_err(|_| anyhow::anyhow!("stderr reader panicked"))?;
    stopped?;
    let (exit_code, timed_out) = result?;
    Ok(Outcome {
        schema_version: 1,
        task_id: task.id.clone(),
        attempt: task.attempt,
        owner: args.lease.task.owner.clone(),
        session_id: task.session_id.clone(),
        claim_id: task.claim_id.clone(),
        message_id: task.message_id.clone(),
        started_at: task.updated_at,
        finished_at: tasks::now(),
        exit_code,
        timed_out,
        detail: if timed_out {
            "wall-clock deadline exceeded".into()
        } else {
            format!(
                "command exited {}",
                exit_code.map_or("by signal".into(), |code| code.to_string())
            )
        },
        stdout: stdout?,
        stderr: stderr?,
    })
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

pub fn run(host: &Path, store: &mut Store, args: RunArgs) -> Result<u8> {
    ensure!(
        cfg!(unix),
        "supervised task execution currently requires Unix process groups and pipes"
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
        let outcome = execute(host, store, &task, &args, &directory)?;
        let receipt = tasks::bind_receipt(host, &persist(&directory, &outcome)?)?;
        Ok::<_, anyhow::Error>((outcome, receipt))
    })();
    let (completion, code) = match outcome {
        Ok((outcome, receipt)) => {
            let success = outcome.exit_code == Some(0) && !outcome.timed_out;
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
