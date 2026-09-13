//! `forge hook <Event>`: answer what can be answered natively, then always
//! still invoke the Python dispatcher once so the hooks that are not (yet)
//! ported still run -- see `handlers` module docs for exactly which names are
//! native today.
//!
//! For `PreToolUse` this binary always reads stdin itself (rather than
//! `Stdio::inherit()`) so it can inspect the payload and compute
//! `handlers::precheck_pretooluse_bash`'s verdict before Python starts. The
//! Python child then always still runs, but is told via two env vars --
//! `FORGE_NATIVE_HOOKS` (which hook names forge already answered) and
//! `FORGE_NATIVE_ANSWERS` (their precomputed JSON verdicts) -- to splice those
//! answers back in at their normal registry position instead of re-running
//! their adapters (`registry.native_answers`, `runner.dispatch`). Both env
//! vars are always set explicitly on the child, every call, overriding
//! whatever the parent shell's environment happened to hold: a stale
//! inherited `FORGE_NATIVE_HOOKS` naming a hook forge did *not* answer this
//! call would otherwise make Python silently skip it with no replacement,
//! which must never happen.
//!
//! `FORGE_NATIVE_HOOKS=""` (explicitly set to the empty string, by whoever
//! invokes `forge`) is the documented escape hatch back to the pre-port,
//! all-Python behaviour: `handlers::native_hook_names_from_env` returns an
//! empty set, `precheck_pretooluse_bash` then always returns `None`, and this
//! module forwards that same empty override to the Python child.

use crate::{handlers, interpreter, telemetry};
use anyhow::{Context, Result};
use serde_json::Value;
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

    if event != "PreToolUse" {
        // No native handlers exist for any other event yet: read nothing,
        // change nothing, delegate exactly as before this PR.
        return delegate(event, None, &no_native_env());
    }

    let mut input = String::new();
    std::io::stdin()
        .read_to_string(&mut input)
        .context("failed to read hook payload from stdin")?;

    let native_hooks = handlers::native_hook_names_from_env();
    let parsed: Option<Value> = serde_json::from_str(&input).ok();
    let native_answer = parsed
        .as_ref()
        .and_then(|payload| handlers::precheck_pretooluse_bash(payload, &native_hooks));

    let extra_env = match &native_answer {
        Some(answer) => {
            let answers = serde_json::json!({ "pre_bash": answer });
            vec![
                ("FORGE_NATIVE_HOOKS".to_string(), "pre_bash".to_string()),
                ("FORGE_NATIVE_ANSWERS".to_string(), answers.to_string()),
            ]
        }
        None => no_native_env(),
    };

    delegate(event, Some(input.as_bytes()), &extra_env)
}

/// The explicit "forge answered nothing natively this call" env override --
/// always set, never left to whatever the parent process's environment
/// happens to contain, so a stale `FORGE_NATIVE_HOOKS` never causes a silent
/// skip with no native replacement.
fn no_native_env() -> Vec<(String, String)> {
    vec![
        ("FORGE_NATIVE_HOOKS".to_string(), String::new()),
        ("FORGE_NATIVE_ANSWERS".to_string(), String::new()),
    ]
}

/// Spawns the Python dispatcher for `event`. `buffered_stdin` is `Some(bytes)`
/// when the caller already consumed stdin to inspect it and must forward
/// those exact bytes; `None` uses `Stdio::inherit()` unchanged. `extra_env`
/// is applied to the child on top of `CLAUDE_PROJECT_DIR`, always overriding
/// any same-named variable already in forge's own environment.
fn delegate(
    event: &str,
    buffered_stdin: Option<&[u8]>,
    extra_env: &[(String, String)],
) -> Result<u8> {
    let root = interpreter::project_root();
    let python = interpreter::resolve_python(&root);

    let start = Instant::now();
    let mut cmd = Command::new(&python);
    cmd.arg("-m")
        .arg("tooling.hooks.dispatch")
        .arg(event)
        .env("CLAUDE_PROJECT_DIR", &root);
    for (key, value) in extra_env {
        cmd.env(key, value);
    }

    let status = match buffered_stdin {
        None => cmd
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .with_context(|| {
                format!("failed to launch {python:?} -m tooling.hooks.dispatch {event}")
            })?,
        Some(bytes) => {
            let mut child = cmd
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
