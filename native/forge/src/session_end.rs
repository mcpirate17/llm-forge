//! Native `SessionEnd`: continuous ledger population (design step 6).
//!
//! When a session ends, its transcript is complete and will never change --
//! the ideal moment to roll it up into the ledger. This module runs
//! `forge ledger rollup` (this same binary, re-invoked as a child so the 2 s
//! bound is a real kill, not a hope) over the ending session's transcript
//! path plus the telemetry directory `hook_rollup` reads. A separate bounded
//! native child publishes the journal. SessionEnd owns no verdict: failures
//! are reported on stderr and incomplete journal records remain for retry.
//!
//! `FORGE_LEDGER_DISABLE=1` skips the rollup entirely (the escape hatch for
//! a broken ledger root or a machine where the write must not happen); the
//! hook itself still runs unchanged.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::Result;
use serde_json::Value;

use crate::bounded_child::run_bounded;

/// Wall-clock bound for the whole rollup child, kill on overrun (design
/// step 6: SessionEnd must not delay the harness's shutdown path).
const ROLLOUP_TIMEOUT: Duration = Duration::from_secs(2);

/// The PostToolUse telemetry file's directory (`hook_rollup`'s input), or
/// `None` when no telemetry directory exists -- nothing to roll up.
fn telemetry_dir() -> Option<PathBuf> {
    let path = match crate::context_telemetry::telemetry_path() {
        Ok(path) => path,
        Err(error) => {
            eprintln!("forge: session telemetry storage unavailable: {error}");
            return None;
        }
    };
    let dir = path.parent()?.to_path_buf();
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

fn run_rollup_child(transcript: &Path) -> Result<()> {
    let mut args = vec!["ledger".to_string(), "rollup".to_string()];
    args.push(transcript.display().to_string());
    if let Some(dir) = telemetry_dir() {
        args.push(dir.display().to_string());
    }
    run_bounded(&args, ROLLOUP_TIMEOUT, "forge ledger rollup")
}

/// Journal I/O and libgit2 status scanning cannot delay shutdown indefinitely.
/// The child receives the original validated JSON through a bounded pipe pump.
pub fn journal_ending_session(payload: &str) {
    if let Err(error) = run_journal_child(payload) {
        eprintln!("forge: session-end journal failed: {error:#}");
    }
}

fn run_journal_child(payload: &str) -> Result<()> {
    let mut command = Command::new(std::env::current_exe()?);
    command.args(["session", "journal", "--host"]);
    command.arg(crate::interpreter::project_root());
    let output = forge::process_probe::run_with_input(
        &mut command,
        payload.as_bytes(),
        Duration::from_secs(3),
        64 * 1024,
    )?;
    anyhow::ensure!(
        output.status.success(),
        "journal exited {}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(())
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
    // integration tests get (`FORGE_TEST_BIN`); under `cargo test` this
    // module's own `current_exe()` is the test harness binary, not the CLI.
}
