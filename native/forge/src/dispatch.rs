//! `forge hook <Event>`: answer what can be answered natively, then either
//! skip Python entirely (every Bash `PreToolUse` hook the Python registry
//! lists for this project is natively served) or still invoke the Python
//! dispatcher once for whatever is not (yet) ported -- see `handlers` module
//! docs for exactly which names are native today.
//!
//! For `PreToolUse` this binary always reads stdin itself (rather than
//! `Stdio::inherit()`) so it can inspect the payload and compute
//! `handlers::native_answers_for_bash`'s per-hook verdicts before deciding
//! whether Python needs to start at all.
//!
//! Two cases:
//!
//! * **Fully native** (`tool_name == "Bash"` and
//!   `handlers::bash_pretooluse_fully_native` is true for the opted-in set):
//!   `handlers::run_bash_pretooluse_fully_native` computes the same merged
//!   verdict Python's own `merge()` would produce across all four
//!   Bash-matching `HookSpec`s, forge prints it to stdout itself, and the
//!   Python dispatcher never starts for this call.
//! * **Partially native or non-Bash**: the Python child still always runs,
//!   but is told via two env vars -- `FORGE_NATIVE_HOOKS` (which hook names
//!   forge already answered) and `FORGE_NATIVE_ANSWERS` (their precomputed
//!   JSON verdicts) -- to splice those answers back in at their normal
//!   registry position instead of re-running their adapters
//!   (`registry.native_answers`, `runner.dispatch`). Both env vars are always
//!   set explicitly on the child, every call, overriding whatever the parent
//!   shell's environment happened to hold: a stale inherited
//!   `FORGE_NATIVE_HOOKS` naming a hook forge did *not* answer this call
//!   would otherwise make Python silently skip it with no replacement, which
//!   must never happen.
//!
//! `FORGE_NATIVE_HOOKS=""` (explicitly set to the empty string, by whoever
//! invokes `forge`) is the documented escape hatch back to the pre-port,
//! all-Python behaviour: `handlers::native_hook_names_from_env` returns an
//! empty set, `native_answers_for_bash` then always returns an empty map,
//! `bash_pretooluse_fully_native` is false, and this module forwards that
//! same empty override to the Python child.
//!
//! `PostToolUse` has both cases, on `tool_name`:
//!
//! * **Fully native** (not Edit/Write/NotebookEdit, and
//!   `handlers::post_tool_use_fully_native` true for the opted-in set): the
//!   nine ported names are every hook the Python registry matches for any
//!   call (the edit family included since `crg_graph_refresh`, `post_edit`
//!   and `obsidian_post_edit` were ported), `handlers::run_post_tooluse_
//!   fully_native` produces the merged answer, and no interpreter starts --
//!   the exit criterion this whole splice exists for (a Read, a plain
//!   `Bash ls` or an Edit answered in the forge process alone).
//! * **Partial opt-in** (some matching name not opted in, or the
//!   `FORGE_NATIVE_HOOKS=""` escape hatch): the Python child runs and gets
//!   the opted-in, matcher-eligible names' precomputed answers spliced back
//!   in via `FORGE_NATIVE_HOOKS`/`FORGE_NATIVE_ANSWERS`, exactly like a
//!   partially-native Bash `PreToolUse` call.
//!
//! `SessionStart` likewise: `handlers::native_answers_for_session_start`
//! answers `workspace_exposure_session` (the EXPOSED summary line) and
//! nothing else -- the legacy `session_start`/`session_handoff` bodies,
//! `crg_refresh_report_session` and `native_freshness` still run in Python
//! for every session start, so the event always delegates. The stdin bytes
//! are still read here (and forwarded unchanged) because the native answer
//! is computed before the child starts, exactly like the other partial
//! paths; the payload itself is irrelevant to this answer, which depends
//! only on the session's checkout.

use crate::{handlers, interpreter, telemetry};
use anyhow::{Context, Result};
use serde_json::Value;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::Instant;

/// Runs one hook event and returns the exit code to propagate to the caller.
pub fn run_hook(event: &str) -> Result<u8> {
    match event {
        "PreToolUse" => run_pre_tool_use(event),
        "PostToolUse" => run_post_tool_use(event),
        "SessionStart" => run_session_start(event),
        _ => {
            // No native handlers exist for any other event yet: read nothing,
            // change nothing, delegate exactly as before this PR.
            delegate(event, None, &no_native_env())
        }
    }
}

fn run_pre_tool_use(event: &str) -> Result<u8> {
    let mut input = String::new();
    std::io::stdin()
        .read_to_string(&mut input)
        .context("failed to read hook payload from stdin")?;

    let native_hooks = handlers::native_hook_names_from_env();
    let parsed: Option<Value> = serde_json::from_str(&input).ok();
    let is_bash = parsed
        .as_ref()
        .and_then(|payload| payload.get("tool_name"))
        .and_then(Value::as_str)
        == Some("Bash");

    if is_bash && handlers::bash_pretooluse_fully_native(&native_hooks) {
        let payload = parsed.as_ref().expect("is_bash implies parsed payload");
        let start = Instant::now();
        let answer = handlers::run_bash_pretooluse_fully_native(payload, &native_hooks);
        let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
        telemetry::record_native(event, elapsed_ms);

        let mut stdout = std::io::stdout();
        stdout
            .write_all(answer.to_string().as_bytes())
            .context("failed to write the native hook verdict to stdout")?;
        stdout
            .write_all(b"\n")
            .context("failed to write the native hook verdict to stdout")?;
        return Ok(0);
    }

    let native_answers = parsed
        .as_ref()
        .filter(|_| is_bash)
        .map(|payload| handlers::native_answers_for_bash(payload, &native_hooks))
        .unwrap_or_default();

    let extra_env = env_for_answers(&native_answers)?;
    delegate(event, Some(input.as_bytes()), &extra_env)
}

/// `PostToolUse`: fully native whenever the call's `tool_name` keeps every
/// registry-matched hook inside `POST_TOOL_USE_HOOK_NAMES` (see module docs),
/// else the Python child runs with the opted-in names' precomputed answers
/// spliced back in, exactly like a partially-native Bash `PreToolUse` call.
fn run_post_tool_use(event: &str) -> Result<u8> {
    let mut input = String::new();
    std::io::stdin()
        .read_to_string(&mut input)
        .context("failed to read hook payload from stdin")?;

    let native_hooks = handlers::native_hook_names_from_env();
    let parsed: Option<Value> = serde_json::from_str(&input).ok();
    let tool_name = parsed
        .as_ref()
        .and_then(|payload| payload.get("tool_name"))
        .and_then(Value::as_str)
        .unwrap_or("");

    if handlers::post_tool_use_fully_native(tool_name, &native_hooks) {
        let payload = parsed
            .as_ref()
            .expect("a tool_name implies a parsed payload");
        let start = Instant::now();
        let answer = handlers::run_post_tooluse_fully_native(payload);
        let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
        telemetry::record_native(event, elapsed_ms);

        let mut stdout = std::io::stdout();
        stdout
            .write_all(answer.to_string().as_bytes())
            .context("failed to write the native hook answer to stdout")?;
        stdout
            .write_all(b"\n")
            .context("failed to write the native hook answer to stdout")?;
        return Ok(0);
    }

    let native_answers = parsed
        .as_ref()
        .map(|payload| handlers::native_answers_for_post_tool_use(payload, &native_hooks))
        .unwrap_or_default();

    let extra_env = env_for_answers(&native_answers)?;
    delegate(event, Some(input.as_bytes()), &extra_env)
}

/// `SessionStart`: always starts Python (see module docs for why no fully
/// native fast path exists here), but splices in
/// `workspace_exposure_session`'s precomputed answer -- the EXPOSED summary
/// line over the session's checkout -- exactly like a partially-native
/// `PreToolUse`/`PostToolUse` call. The payload is forwarded unread-by-this
/// path (the answer depends on the checkout, not the input).
fn run_session_start(event: &str) -> Result<u8> {
    let mut input = String::new();
    std::io::stdin()
        .read_to_string(&mut input)
        .context("failed to read hook payload from stdin")?;

    let native_hooks = handlers::native_hook_names_from_env();
    let native_answers = handlers::native_answers_for_session_start(&native_hooks);

    let extra_env = env_for_answers(&native_answers)?;
    delegate(event, Some(input.as_bytes()), &extra_env)
}

/// Builds the `FORGE_NATIVE_HOOKS`/`FORGE_NATIVE_ANSWERS` env pair to forward
/// to the Python child: the explicit empty override when nothing was
/// answered natively this call, otherwise the answered names and their
/// serialized verdicts.
fn env_for_answers(native_answers: &HashMap<String, Value>) -> Result<Vec<(String, String)>> {
    if native_answers.is_empty() {
        return Ok(no_native_env());
    }
    let names: Vec<&str> = native_answers.keys().map(String::as_str).collect();
    let answers = serde_json::to_string(&native_answers)
        .context("failed to serialize native hook answers")?;
    Ok(vec![
        ("FORGE_NATIVE_HOOKS".to_string(), names.join(",")),
        ("FORGE_NATIVE_ANSWERS".to_string(), answers),
    ])
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
