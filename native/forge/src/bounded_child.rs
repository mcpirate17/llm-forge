//! Shared "re-invoke this same binary as a child, killed on a wall-clock
//! deadline" pattern. Lifted out of `session_end.rs` (design step 6) so
//! `subagent_stop.rs` (Phase 3 step 3, item 1) does not duplicate the same
//! ~40 lines of process/timeout plumbing -- both call sites need exactly
//! this: spawn `forge <args>`, wait on a background thread so the deadline
//! is a real kill rather than a hope, and turn every failure mode (spawn
//! error, nonzero exit, timeout) into one `Result`.

use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use anyhow::{Context, Result};

/// Re-invokes the current executable with `args`, waits up to `timeout`,
/// and kills it by pid on overrun. `action` names the child for error
/// messages only (e.g. `"forge ledger rollup"`).
pub fn run_bounded(args: &[String], timeout: Duration, action: &str) -> Result<()> {
    let exe = std::env::current_exe().context("locating the forge binary")?;
    let mut command = Command::new(exe);
    command
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let child = command
        .spawn()
        .with_context(|| format!("spawning {action}"))?;
    let pid = child.id();

    // The bound is enforced by waiting on a channel, not on the child:
    // `Child::wait` blocks with no deadline, so the blocking wait happens on
    // a thread and the main thread kills the child by pid when the deadline
    // passes -- the waiter thread then returns and is joined below.
    let (tx, rx) = mpsc::channel();
    let handle = std::thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });
    let outcome = match rx.recv_timeout(timeout) {
        Ok(received) => received.with_context(|| format!("waiting for {action}")),
        Err(_) => {
            let _ = Command::new("kill")
                .arg(pid.to_string())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            Err(anyhow::anyhow!(
                "{action} timed out after {} ms (child killed)",
                timeout.as_millis()
            ))
        }
    };
    let _ = handle.join();
    match outcome {
        Ok(output) if output.status.success() => Ok(()),
        Ok(output) => anyhow::bail!(
            "{action} exited {}: {}",
            output
                .status
                .code()
                .map(|code| code.to_string())
                .unwrap_or_else(|| "signal".to_string()),
            String::from_utf8_lossy(&output.stderr).trim()
        ),
        Err(err) => Err(err),
    }
}
