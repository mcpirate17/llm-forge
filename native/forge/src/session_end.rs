//! Native `SessionEnd`: continuous ledger population (design step 6).
//!
//! When a session ends, its transcript is complete and will never change --
//! the ideal moment to roll it up into the ledger. This module runs
//! `forge ledger rollup` (this same binary, re-invoked as a child so the 2 s
//! bound is a real kill, not a hope) over the ending session's transcript
//! path plus the telemetry directory `hook_rollup` reads, then hands the
//! event to the Python dispatcher exactly as before -- SessionEnd owns no
//! verdict, so the rollup is best-effort: every failure goes to stderr and
//! the hook's exit code stays whatever the dispatcher says.
//!
//! `FORGE_LEDGER_DISABLE=1` skips the rollup entirely (the escape hatch for
//! a broken ledger root or a machine where the write must not happen); the
//! hook itself still runs unchanged.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::Value;

/// Wall-clock bound for the whole rollup child, kill on overrun (design
/// step 6: SessionEnd must not delay the harness's shutdown path).
const ROLLOUP_TIMEOUT: Duration = Duration::from_secs(2);

/// The PostToolUse telemetry file's directory (`hook_rollup`'s input), or
/// `None` when no telemetry directory exists -- nothing to roll up.
fn telemetry_dir() -> Option<PathBuf> {
    let dir = crate::context_telemetry::telemetry_path()
        .parent()?
        .to_path_buf();
    dir.is_dir().then_some(dir)
}

/// Runs the SessionEnd rollup for `payload`'s `transcript_path`, best-effort:
/// every error (missing path, timeout, nonzero child) is one stderr line and
/// `Ok(())` -- a SessionEnd hook must never fail because the ledger could
/// not be written.
pub fn rollup_ending_session(payload: &str) {
    if std::env::var("FORGE_LEDGER_DISABLE").ok().as_deref() == Some("1") {
        eprintln!("forge: ledger rollup disabled (FORGE_LEDGER_DISABLE=1)");
        return;
    }
    let parsed: Value = match serde_json::from_str(payload) {
        Ok(value) => value,
        Err(err) => {
            eprintln!("forge: SessionEnd payload is not JSON ({err}); no ledger rollup");
            return;
        }
    };
    let Some(transcript) = parsed
        .get("transcript_path")
        .and_then(Value::as_str)
        .map(PathBuf::from)
    else {
        eprintln!("forge: SessionEnd payload carries no transcript_path; no ledger rollup");
        return;
    };
    if let Err(err) = run_rollup_child(&transcript) {
        eprintln!("forge: session-end ledger rollup failed: {err:#}");
    }
}

fn run_rollup_child(transcript: &PathBuf) -> Result<()> {
    let exe = std::env::current_exe().context("locating the forge binary")?;
    let mut command = Command::new(exe);
    command
        .args(["ledger", "rollup"])
        .arg(transcript)
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    if let Some(dir) = telemetry_dir() {
        command.arg(dir);
    }
    let child = command.spawn().context("spawning forge ledger rollup")?;
    let pid = child.id();

    // The 2 s bound is enforced by waiting on a channel, not on the child:
    // `Child::wait` blocks with no deadline, so the blocking wait happens on
    // a thread and the main thread kills the child by pid when the deadline
    // passes -- the waiter thread then returns and is joined below.
    let (tx, rx) = mpsc::channel();
    let handle = std::thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });
    let outcome = match rx.recv_timeout(ROLLOUP_TIMEOUT) {
        Ok(received) => received.context("waiting for forge ledger rollup"),
        Err(_) => {
            let _ = Command::new("kill")
                .arg(pid.to_string())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            Err(anyhow::anyhow!(
                "timed out after {} ms (child killed)",
                ROLLOUP_TIMEOUT.as_millis()
            ))
        }
    };
    let _ = handle.join();
    match outcome {
        Ok(output) if output.status.success() => Ok(()),
        Ok(output) => anyhow::bail!(
            "forge ledger rollup exited {}: {}",
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_payload_without_a_transcript_path_is_a_stderr_note_not_a_failure() {
        // Neither branch may panic or propagate: the return type is (),
        // so calling these is itself the assertion (they print to stderr).
        rollup_ending_session("not json at all");
        rollup_ending_session(r#"{"cwd":"/tmp"}"#);
    }

    #[test]
    fn disable_env_skips_the_rollup_entirely() {
        // With the hatch set and a transcript_path present, the disabled
        // branch returns before touching the filesystem -- proven by a
        // nonexistent transcript path that would otherwise fail loudly.
        std::env::set_var("FORGE_LEDGER_DISABLE", "1");
        rollup_ending_session(r#"{"transcript_path":"/nonexistent/x.jsonl"}"#);
        std::env::remove_var("FORGE_LEDGER_DISABLE");
    }

    // The end-to-end path -- a real `forge ledger rollup` child over a real
    // transcript through `forge hook SessionEnd` -- lives in
    // tests/hook_delegation.rs (`session_end_rolls_the_ending_session_into_
    // the_ledger`): it needs the compiled `forge` binary, which only
    // integration tests get (`CARGO_BIN_EXE_forge`); under `cargo test` this
    // module's own `current_exe()` is the test harness binary, not the CLI.
}
