//! A fresh native CLI process per measurement; existing pipe monitor and group owner.
use crate::{
    process_group::ProcessGroup, process_io::open_pidfd, process_probe::readiness,
    process_signals::Interrupts,
};
use anyhow::{ensure, Context, Result};
use std::{
    os::fd::AsRawFd,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

pub(crate) struct Measured {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub exit: i32,
    pub wall_seconds: f64,
    /// Largest individual process peak from wait4, with completed-wait propagation only.
    /// Unpropagated descendants and simultaneous aggregate tree RSS are not measured.
    pub max_rss_kib: libc::c_long,
}
/// Native review handles SIGINT and tears down its owned Stage/cgroups first.
/// Forced teardown is always infrastructure failure, never a complete timing row.
pub(crate) fn run(
    command: &mut Command,
    timeout: Duration,
    interrupts: &Interrupts,
) -> Result<Measured> {
    ensure!(
        !interrupts.interrupted()?,
        "benchmark interrupted before measurement"
    );
    let deadline = readiness::deadline(timeout)?;
    let capture = readiness::CapturePolicy::TailChars(262_144);
    capture.validate()?;
    let input = readiness::Input::prepare(command, None)?;
    interrupts.configure_child(command);
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let started = Instant::now();
    let mut group = ProcessGroup::spawn(command)?;
    command.stdin(Stdio::null());
    let pidfd = open_pidfd(group.child())?;
    let mut monitor = readiness::Monitor::new(
        group.take_stdout().context("benchmark stdout")?.into(),
        group.take_stderr().context("benchmark stderr")?.into(),
        input,
        capture,
    )?;
    let failure = monitor
        .observe(
            pidfd.as_raw_fd(),
            deadline,
            Some(interrupts.as_raw_fd()),
            || Ok(interrupts.interrupted()?),
        )
        .and_then(|observed| readiness::observation_error(observed, timeout))
        .err();
    if failure.is_some() {
        group.signal(libc::SIGINT)?;
        let cleanup = Duration::from_secs(120);
        let observed = monitor.observe(
            pidfd.as_raw_fd(),
            readiness::deadline(cleanup)?,
            None,
            || Ok(false),
        )?;
        readiness::observation_error(observed, cleanup).context(
            "review did not finish owned cleanup; fixture and grant scratch retained for recovery",
        )?;
    }
    let (status, usage) = group.stop_with_usage()?;
    monitor.drain_to_eof(Duration::from_secs(5))?;
    let output = monitor.finish();
    ensure!(
        output.stdout_bytes == output.stdout.len() as u64 && output.stdout.len() <= 4096,
        "measurement completion stdout was truncated or exceeds 4096 bytes"
    );
    if let Some(error) = failure {
        return Err(error).context("benchmark cancelled/timed out; measurement is incomplete");
    }
    Ok(Measured {
        stdout: output.stdout,
        stderr: output.stderr,
        exit: status
            .code()
            .context("review worker terminated by signal")?,
        wall_seconds: started.elapsed().as_secs_f64(),
        max_rss_kib: usage.ru_maxrss,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn actual_native_child_is_reaped_with_wall_and_peak_memory_observations() {
        let _owner = crate::process_signals::TEST_OWNER.lock().unwrap();
        let interrupts = Interrupts::install().unwrap();
        let mut command = Command::new("/bin/echo");
        command.arg("completed");
        let observed = run(&mut command, Duration::from_secs(5), &interrupts).unwrap();
        assert_eq!(observed.exit, 0);
        assert_eq!(observed.stdout, b"completed\n");
        assert!(observed.wall_seconds.is_finite() && observed.wall_seconds > 0.0);
        assert!(observed.max_rss_kib >= 0);
    }
    #[test]
    fn oversized_completion_is_refused_instead_of_accepting_a_truncated_marker() {
        let _owner = crate::process_signals::TEST_OWNER.lock().unwrap();
        let interrupts = Interrupts::install().unwrap();
        let mut command = Command::new("/bin/echo");
        command.arg("x".repeat(5000));
        let error = run(&mut command, Duration::from_secs(5), &interrupts)
            .err()
            .unwrap();
        assert!(error.to_string().contains("4096"));
    }
    #[test]
    fn timeout_cannot_become_a_completed_timing_row() {
        let _owner = crate::process_signals::TEST_OWNER.lock().unwrap();
        let interrupts = Interrupts::install().unwrap();
        let mut command = Command::new("/bin/sleep");
        command.arg("10");
        let error = run(&mut command, Duration::from_millis(20), &interrupts)
            .err()
            .unwrap();
        assert!(format!("{error:#}").contains("timed out"));
    }
}
