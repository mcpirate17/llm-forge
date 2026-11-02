//! Bounded synchronous management probes using kernel readiness, not sleep loops
//! or waiter threads. Async services call this through a bounded blocking pool.
use crate::process_group::ProcessGroup;
use crate::process_io::open_pidfd;
use crate::process_signals::Interrupts;
use anyhow::{ensure, Context, Result};
use std::os::fd::AsRawFd;
use std::process::{Command, Output, Stdio};
use std::time::Duration;
#[path = "process_probe_io.rs"]
pub(crate) mod readiness;
use readiness::{CapturePolicy, Input, Monitor};

pub fn run(command: &mut Command, timeout: Duration, output_limit: usize) -> Result<Output> {
    run_inner(command, None, timeout, output_limit, None)
}

#[derive(Debug)]
pub struct Interrupted;
impl std::fmt::Display for Interrupted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("probe interrupted")
    }
}
impl std::error::Error for Interrupted {}

#[derive(Debug)]
pub struct TimedOut(pub Duration);
impl std::fmt::Display for TimedOut {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "probe timed out after {} ms", self.0.as_millis())
    }
}
impl std::error::Error for TimedOut {}

/// A foreground supervisor can cancel a probe through its owned signalfd.
/// Cancellation tears down only this probe's process group before returning.
pub fn run_interruptible(
    command: &mut Command,
    timeout: Duration,
    output_limit: usize,
    interrupts: &Interrupts,
) -> Result<Output> {
    if interrupts.interrupted()? {
        return Err(Interrupted.into());
    }
    interrupts.configure_child(command);
    run_inner(command, None, timeout, output_limit, Some(interrupts))
}

/// Deliver bounded stdin while draining both output streams on the same kernel
/// readiness loop. A Unix socketpair provides byte-stream stdin and MSG_NOSIGNAL
/// writes; callers do not need to change a process-wide signal disposition.
pub fn run_with_input(
    command: &mut Command,
    input: &[u8],
    timeout: Duration,
    output_limit: usize,
) -> Result<Output> {
    ensure!(input.len() <= 1024 * 1024, "probe stdin exceeds 1 MiB");
    run_inner(command, Some(input), timeout, output_limit, None)
}

/// Deliver bounded stdin while supervising interrupts and restoring the child's mask.
pub fn run_with_input_interruptible(
    command: &mut Command,
    input: &[u8],
    timeout: Duration,
    output_limit: usize,
    interrupts: &Interrupts,
) -> Result<Output> {
    ensure!(input.len() <= 1024 * 1024, "probe stdin exceeds 1 MiB");
    if interrupts.interrupted()? {
        return Err(Interrupted.into());
    }
    interrupts.configure_child(command);
    run_inner(
        command,
        Some(input),
        timeout,
        output_limit,
        Some(interrupts),
    )
}

fn run_inner(
    command: &mut Command,
    input: Option<&[u8]>,
    timeout: Duration,
    output_limit: usize,
    interrupts: Option<&Interrupts>,
) -> Result<Output> {
    let deadline = readiness::deadline(timeout)?;
    let policy = CapturePolicy::StrictCombined(output_limit);
    policy.validate()?;
    let input = Input::prepare(command, input)?;
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut group = ProcessGroup::spawn(command)
        .with_context(|| format!("starting probe {:?}", command.get_program()))?;
    command.stdin(Stdio::null());
    let pidfd = open_pidfd(group.child())?;
    let mut monitor = Monitor::new(
        group.take_stdout().context("probe stdout")?.into(),
        group.take_stderr().context("probe stderr")?.into(),
        input,
        policy,
    )?;
    let observation = monitor.observe(
        pidfd.as_raw_fd(),
        deadline,
        interrupts.map(AsRawFd::as_raw_fd),
        || Ok(interrupts.map_or(Ok(false), Interrupts::interrupted)?),
    )?;
    readiness::observation_error(observation, timeout)?;
    let status = group.stop()?;
    monitor.require_stdin_consumed()?;
    monitor.drain_available(timeout)?;
    let captured = monitor.finish();
    Ok(Output {
        status,
        stdout: captured.stdout,
        stderr: captured.stderr,
    })
}
