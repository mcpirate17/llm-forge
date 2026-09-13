//! `forge hook <Event>`: delegate whole to the Python dispatcher.
//!
//! Reads nothing itself -- stdin is piped straight through to the child process,
//! and the child's stdout/stderr go straight back out, so the merged hook JSON and
//! any trace output are byte-for-byte what `python -m tooling.hooks.dispatch
//! <Event>` would have produced on its own. The only work forge does here is
//! resolve the interpreter, time the call, and (if `handlers::fully_native` ever
//! covers an event) refuse to silently keep shelling out once native coverage is
//! claimed without a routing path -- see `handlers::fully_native`.

use crate::{handlers, interpreter, telemetry};
use anyhow::{Context, Result};
use std::process::{Command, Stdio};
use std::time::Instant;

/// Runs one hook event and returns the exit code to propagate to the caller.
pub fn run_hook(event: &str) -> Result<u8> {
    if handlers::fully_native(event) {
        anyhow::bail!(
            "handlers::fully_native(\"{event}\") is true but dispatch::run_hook has \
             no native routing path yet -- this is a wiring bug, not a fallback"
        );
    }

    let root = interpreter::project_root();
    let python = interpreter::resolve_python(&root);

    let start = Instant::now();
    let status = Command::new(&python)
        .arg("-m")
        .arg("tooling.hooks.dispatch")
        .arg(event)
        .env("CLAUDE_PROJECT_DIR", &root)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .with_context(|| {
            format!("failed to launch {python:?} -m tooling.hooks.dispatch {event}")
        })?;
    let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;

    telemetry::record_delegation(event, elapsed_ms);

    let code = status.code().unwrap_or(1);
    Ok(code.clamp(0, 255) as u8)
}
