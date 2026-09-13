//! `forge hook <Event>`: reach a verdict natively where possible, otherwise
//! delegate whole to the Python dispatcher.
//!
//! Default behaviour (`FORGE_NATIVE_HOOKS` unset) is byte-for-byte unchanged
//! from before this PR: stdin is piped straight through to the Python child
//! process with `Stdio::inherit()`, and its stdout/stderr go straight back out.
//! Only when the caller opts in via `FORGE_NATIVE_HOOKS` does forge read stdin
//! itself first, to check whether `handlers::precheck_pretooluse_bash` can
//! answer without spawning Python at all -- see `handlers` module docs for
//! exactly which case that is and why it stays opt-in.

use crate::{handlers, interpreter, telemetry};
use anyhow::{Context, Result};
use std::io::{Read, Write};
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

    let native_hooks = handlers::native_hook_names_from_env();
    if event == "PreToolUse" && !native_hooks.is_empty() {
        let mut input = String::new();
        std::io::stdin()
            .read_to_string(&mut input)
            .context("failed to read hook payload from stdin")?;

        let start = Instant::now();
        let parsed: Option<serde_json::Value> = serde_json::from_str(&input).ok();
        let native_answer = parsed
            .as_ref()
            .and_then(|payload| handlers::precheck_pretooluse_bash(payload, &native_hooks));

        if let Some(answer) = native_answer {
            let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
            println!("{answer}");
            telemetry::record_native(event, elapsed_ms);
            return Ok(0);
        }

        // Not answerable natively (allow verdict, non-Bash tool, or unparseable
        // payload): fall through to full Python delegation, forwarding the
        // stdin bytes we already consumed instead of `Stdio::inherit()`.
        return delegate(event, Some(input.as_bytes()));
    }

    delegate(event, None)
}

/// Spawns the Python dispatcher for `event`. `buffered_stdin` is `Some(bytes)`
/// when the caller already consumed stdin to inspect it (the opt-in native
/// precheck path) and must forward those exact bytes; `None` uses
/// `Stdio::inherit()` unchanged, the default path's original behaviour.
fn delegate(event: &str, buffered_stdin: Option<&[u8]>) -> Result<u8> {
    let root = interpreter::project_root();
    let python = interpreter::resolve_python(&root);

    let start = Instant::now();
    let status = match buffered_stdin {
        None => Command::new(&python)
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
            })?,
        Some(bytes) => {
            let mut child = Command::new(&python)
                .arg("-m")
                .arg("tooling.hooks.dispatch")
                .arg(event)
                .env("CLAUDE_PROJECT_DIR", &root)
                .stdin(Stdio::piped())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .spawn()
                .with_context(|| {
                    format!("failed to launch {python:?} -m tooling.hooks.dispatch {event}")
                })?;
            child
                .stdin
                .take()
                .expect("piped stdin")
                .write_all(bytes)
                .context("failed to forward buffered stdin to the Python dispatcher")?;
            child
                .wait()
                .context("failed to wait for the Python dispatcher")?
        }
    };
    let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;

    telemetry::record_delegation(event, elapsed_ms);

    let code = status.code().unwrap_or(1);
    Ok(code.clamp(0, 255) as u8)
}
