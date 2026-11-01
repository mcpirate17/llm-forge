//! Explicit, bounded command execution for the native task journal. This is
//! process supervision, not a filesystem/network sandbox or approval authority.

use crate::task_limits::{LimitScope, LimitsReport};
use crate::task_resources::{ResourceRequest, ResourceSnapshot};
use crate::task_store::{Completion, State, Store, Task};
use crate::tasks::{self, RunArgs};
use anyhow::{ensure, Context, Result};
use forge::process_capture::{Capture, LogSummary};
use forge::process_group::ProcessGroup;
use forge::process_io::ChildExit;
use forge::process_signals::Interrupts;
use serde::Serialize;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;
use tokio::time::Instant;

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
    group: ProcessGroup,
    scope_stopped: bool,
    scope: Option<LimitScope>,
}

struct Launch<'a> {
    host: &'a Path,
    task: &'a Task,
    args: &'a RunArgs,
    interrupts: &'a Interrupts,
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
        let group_result = self.group.stop().map(|_| ()).map_err(anyhow::Error::from);
        match (scope_result, group_result) {
            (Err(scope), Err(group)) => {
                anyhow::bail!("scope cleanup: {scope:#}; group cleanup: {group:#}")
            }
            (Err(error), _) | (_, Err(error)) => Err(error),
            _ => Ok(()),
        }
    }
}

impl Drop for ManagedChild {
    fn drop(&mut self) {
        if let Err(error) = self.stop() {
            eprintln!("forge task: child cleanup failed: {error:#}");
        }
    }
}

fn output_file(directory: &Path, name: &str) -> Result<File> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(directory.join(name))
        .with_context(|| format!("creating task output {name}"))
}

fn spawn(
    launch: &Launch<'_>,
    directory: &Path,
    available: &ResourceSnapshot,
    mut scope: Option<LimitScope>,
) -> Result<ManagedChild> {
    let Launch {
        host,
        task,
        args,
        interrupts,
    } = *launch;
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
    interrupts.configure_child(&mut command);
    ensure!(!interrupts.interrupted()?, "task interrupted before launch");
    Ok(ManagedChild {
        group: ProcessGroup::spawn(&mut command)
            .with_context(|| format!("spawning task executable {program:?}"))?,
        scope_stopped: false,
        scope,
    })
}

async fn monitor(
    launch: &Launch<'_>,
    store: &mut Store,
    child: &mut ManagedChild,
) -> Result<(Option<i32>, bool)> {
    let Launch {
        host,
        task,
        args,
        interrupts,
    } = *launch;
    ensure!(!interrupts.interrupted()?, "task interrupted by signal");
    let exit =
        ChildExit::new(child.group.child()).context("register Linux child exit notification")?;
    let signals = interrupts.register_reactor()?;
    let deadline = Instant::now() + Duration::from_secs(args.timeout_seconds);
    let heartbeat_interval =
        Duration::from_millis((args.lease.lease_seconds as u64 * 1000 / 3).max(100));
    let mut heartbeat =
        tokio::time::interval_at(Instant::now() + heartbeat_interval, heartbeat_interval);
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut signal = [0u8; std::mem::size_of::<libc::signalfd_siginfo>()];
    loop {
        tokio::select! {
            biased;
            _ = tokio::time::sleep_until(deadline) => return Ok((None, true)),
            received = signals.read(&mut signal) => {
                received.context("read supervisor interrupt")?;
                anyhow::bail!("task interrupted by signal");
            }
            status = exit.wait() => return Ok((status?.code(), false)),
            _ = heartbeat.tick() => {
                tasks::validate_claim(host, task, &args.lease.task.owner)?;
                let remaining = deadline.saturating_duration_since(Instant::now());
                store.set_busy_timeout(remaining.min(Duration::from_millis(200)))?;
                store.heartbeat(&task.id, &args.lease.task.owner, task.attempt,
                    args.lease.lease_seconds, tasks::now())?;
            }
        }
    }
}

async fn execute(
    launch: &Launch<'_>,
    store: &mut Store,
    directory: &Path,
    available: &ResourceSnapshot,
    scope: Option<LimitScope>,
) -> Result<Outcome> {
    let Launch { task, args, .. } = *launch;
    let mut outcome = initial_outcome(task, args, available, scope.as_ref());
    let out = output_file(directory, "stdout.log")?;
    let err = output_file(directory, "stderr.log")?;
    let mut child = spawn(launch, directory, available, scope)?;
    let stdout_pipe = child.group.take_stdout().context("task stdout pipe")?;
    let stderr_pipe = child.group.take_stderr().context("task stderr pipe")?;
    let stdout = Capture::start(stdout_pipe.into(), out, args.log_bytes)?;
    let stderr = Capture::start(stderr_pipe.into(), err, args.log_bytes)?;
    let result = monitor(launch, store, &mut child).await;
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
    let (stdout, stderr) = tokio::join!(stdout.finish(), stderr.finish());
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
    result: Result<LogSummary>,
    name: &str,
    outcome: &mut Outcome,
) -> Option<LogSummary> {
    match result {
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
    let signals = Interrupts::install()?;
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
        !signals.interrupted()?,
        "task interrupted during resource preflight"
    );
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
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
    let launch = Launch {
        host,
        task: &task,
        args: &args,
        interrupts: &signals,
    };
    let outcome = (|| {
        std::fs::create_dir_all(&directory)?;
        let fallback = initial_outcome(&task, &args, &available, scope.as_ref());
        let outcome = match runtime.block_on(execute(&launch, store, &directory, &available, scope))
        {
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
