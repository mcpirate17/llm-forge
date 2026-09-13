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
/// `FORGE_HOOK_STANDALONE=1` (the LLM monorepo's install, `docs/routing.md`)
/// routes to `run_hook_standalone` instead: forge never delegates to Python
/// and never runs the native Bash-guard/`crg_refresh_report_pre` branches --
/// only its own decisions (routing, live cap check, the `SubagentStop`
/// rollup, telemetry) -- because standalone means "installed as an
/// additional hook entry beside another project's own dispatcher," which
/// must never see forge second-guess a call it has no opinion on.
pub fn run_hook(event: &str) -> Result<u8> {
    if standalone_mode() {
        return run_hook_standalone(event);
    }
    match event {
        "PreToolUse" => run_pre_tool_use(event),
        "PostToolUse" => run_post_tool_use(event),
        "SessionStart" => run_session_start(event),
        "SessionEnd" => run_session_end(event),
        "SubagentStop" => run_subagent_stop(event),
        _ => {
            // No native handlers exist for any other event yet: read nothing,
            // change nothing, delegate exactly as before this PR.
            delegate(event, None, &no_native_env())
        }
    }
}

fn standalone_mode() -> bool {
    std::env::var("FORGE_HOOK_STANDALONE").as_deref() == Ok("1")
}

/// `FORGE_HOOK_STANDALONE=1`: `PreToolUse` (routing + live cap check + the
/// native Bash guard) and `SubagentStop` (the ledger rollup) have anything
/// forge wants to say; every other event -- and any `PreToolUse` call none
/// of those three has an opinion on -- prints nothing and exits 0, a bare
/// allow. No Python child is ever spawned on this path.
fn run_hook_standalone(event: &str) -> Result<u8> {
    match event {
        "PreToolUse" => run_pre_tool_use_standalone(event),
        "SubagentStop" => run_subagent_stop_standalone(event),
        _ => Ok(0),
    }
}

/// Standalone `PreToolUse`: the live cap check runs first, exactly as in the
/// non-standalone path (`Warn`/`Deny` short-circuit with their own verdict);
/// a `Bash` call runs the same fully-native guard path
/// (`handlers::run_bash_pretooluse_fully_native` over the default native set,
/// folded through `merge::merge` internally) the non-standalone path runs
/// for a fully-opted-in call -- Bash is the hottest tool and this is now the
/// only place its guard denials come from under standalone, so there is no
/// Python fallback to check `bash_pretooluse_fully_native` against first;
/// an `Agent` call gets forge's routing verdict alone (`route::
/// hook_outcome_for_agent`, run through `merge::merge` on its own so a
/// malformed embedded policy still fails closed the same way the merged,
/// non-standalone path does); anything else has nothing native to say under
/// standalone and prints nothing.
fn run_pre_tool_use_standalone(event: &str) -> Result<u8> {
    let start = Instant::now();
    let mut input = String::new();
    std::io::stdin()
        .read_to_string(&mut input)
        .context("failed to read hook payload from stdin")?;
    let parsed: Option<Value> = serde_json::from_str(&input).ok();

    if let Some(payload) = parsed.as_ref() {
        match crate::cap_enforce::check(payload) {
            crate::cap_enforce::CapCheck::NoOp => {}
            crate::cap_enforce::CapCheck::Warn(context_line) => {
                telemetry::record_native(event, start.elapsed().as_secs_f64() * 1000.0);
                return print_cap_verdict("allow", None, Some(&context_line));
            }
            crate::cap_enforce::CapCheck::Deny(reason) => {
                telemetry::record_native(event, start.elapsed().as_secs_f64() * 1000.0);
                return print_cap_verdict("deny", Some(&reason), None);
            }
        }
    }

    let tool_name = parsed
        .as_ref()
        .and_then(|payload| payload.get("tool_name"))
        .and_then(Value::as_str);
    if tool_name == Some("Bash") {
        let payload = parsed.as_ref().expect("tool_name implies parsed payload");
        let native_hooks = handlers::native_hook_names_from_env();
        let answer = handlers::run_bash_pretooluse_fully_native(payload, &native_hooks);
        telemetry::record_native(event, start.elapsed().as_secs_f64() * 1000.0);
        let mut stdout = std::io::stdout();
        stdout
            .write_all(answer.to_string().as_bytes())
            .context("failed to write the standalone hook verdict to stdout")?;
        stdout
            .write_all(b"\n")
            .context("failed to write the standalone hook verdict to stdout")?;
        return Ok(0);
    }
    if tool_name == Some("Agent") {
        let payload = parsed.as_ref().expect("tool_name implies parsed payload");
        let answer = crate::merge::merge(
            "PreToolUse",
            &[crate::route::hook_outcome_for_agent(payload)],
        );
        telemetry::record_native(event, start.elapsed().as_secs_f64() * 1000.0);
        let mut stdout = std::io::stdout();
        stdout
            .write_all(answer.to_string().as_bytes())
            .context("failed to write the standalone hook verdict to stdout")?;
        stdout
            .write_all(b"\n")
            .context("failed to write the standalone hook verdict to stdout")?;
        return Ok(0);
    }

    telemetry::record_native(event, start.elapsed().as_secs_f64() * 1000.0);
    Ok(0)
}

/// Standalone `SubagentStop`: the same narrow rollup the non-standalone path
/// runs, minus the Python delegation it would otherwise fall through to.
fn run_subagent_stop_standalone(event: &str) -> Result<u8> {
    let start = Instant::now();
    let mut input = String::new();
    std::io::stdin()
        .read_to_string(&mut input)
        .context("failed to read hook payload from stdin")?;
    crate::subagent_stop::rollup_ending_agent(&input);
    telemetry::record_native(event, start.elapsed().as_secs_f64() * 1000.0);
    Ok(0)
}

fn run_pre_tool_use(event: &str) -> Result<u8> {
    let mut input = String::new();
    std::io::stdin()
        .read_to_string(&mut input)
        .context("failed to read hook payload from stdin")?;

    let native_hooks = handlers::native_hook_names_from_env();
    let parsed: Option<Value> = serde_json::from_str(&input).ok();

    // Live cap enforcement (Phase 3 step 3, item 2): runs first,
    // unconditionally, for every `PreToolUse` call. `NoOp` (no `agent_id`,
    // or under 80% of a subagent's cap) is the fast path and falls through
    // to every branch below unchanged; `Warn`/`Deny` print their own JSON
    // verdict and return immediately -- a documented, accepted tradeoff
    // (`cap_enforce` module docs, `docs/routing.md`'s enforcement section):
    // only a call already at or past 80% of its subagent's cap ever skips
    // the native Bash/`Agent` branches and Python delegation for that one
    // call.
    if let Some(payload) = parsed.as_ref() {
        match crate::cap_enforce::check(payload) {
            crate::cap_enforce::CapCheck::NoOp => {}
            crate::cap_enforce::CapCheck::Warn(context_line) => {
                return print_cap_verdict("allow", None, Some(&context_line));
            }
            crate::cap_enforce::CapCheck::Deny(reason) => {
                return print_cap_verdict("deny", Some(&reason), None);
            }
        }
    }

    let tool_name = parsed
        .as_ref()
        .and_then(|payload| payload.get("tool_name"))
        .and_then(Value::as_str);
    let is_bash = tool_name == Some("Bash");
    let is_agent = tool_name == Some("Agent");

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

    // `Agent` `PreToolUse`: the routing policy (`docs/roadmap.md` Phase 3
    // step 2) decides the dispatch's tier natively. `crg_refresh_report_pre`
    // (matcher `.*`) is the only Python-registered `HookSpec` that also
    // matches `Agent`; once it is opted in, forge has full native coverage
    // for the call and never starts Python, exactly like the Bash fast path
    // above. Short of that (the `FORGE_NATIVE_HOOKS` escape hatch), routing
    // is silently skipped here and the call delegates to Python unchanged --
    // same behaviour as an opted-out Bash call.
    if is_agent && handlers::agent_pretooluse_fully_native(&native_hooks) {
        let payload = parsed.as_ref().expect("is_agent implies parsed payload");
        let start = Instant::now();
        let answer = handlers::run_agent_pretooluse_fully_native(payload);
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

/// Prints one `cap_enforce` verdict as the hook's whole stdout answer and
/// returns `Ok(0)` -- the same shape the Bash/`Agent` fully-native branches
/// already use, just with `hookEventName` fixed to `PreToolUse` and a
/// `permissionDecisionReason`/`additionalContext` instead of an
/// `updatedInput`.
fn print_cap_verdict(decision: &str, reason: Option<&str>, context: Option<&str>) -> Result<u8> {
    let mut specific = serde_json::Map::new();
    specific.insert(
        "hookEventName".to_string(),
        Value::String("PreToolUse".to_string()),
    );
    specific.insert(
        "permissionDecision".to_string(),
        Value::String(decision.to_string()),
    );
    if let Some(reason) = reason {
        specific.insert(
            "permissionDecisionReason".to_string(),
            Value::String(reason.to_string()),
        );
    }
    if let Some(context) = context {
        specific.insert(
            "additionalContext".to_string(),
            Value::String(context.to_string()),
        );
    }
    let answer = serde_json::json!({ "hookSpecificOutput": Value::Object(specific) });
    let mut stdout = std::io::stdout();
    stdout
        .write_all(answer.to_string().as_bytes())
        .context("failed to write the cap_enforce verdict to stdout")?;
    stdout
        .write_all(b"\n")
        .context("failed to write the cap_enforce verdict to stdout")?;
    Ok(0)
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
/// `SessionEnd` (design step 6): the ledger rollup of the ending session's
/// transcript runs natively first (best-effort, 2 s bound, stderr only --
/// `session_end::rollup_ending_session`), then the event delegates to the
/// Python dispatcher unchanged, exactly as before this arm existed.
fn run_session_end(event: &str) -> Result<u8> {
    let mut input = String::new();
    std::io::stdin()
        .read_to_string(&mut input)
        .context("failed to read hook payload from stdin")?;
    crate::session_end::rollup_ending_session(&input);
    delegate(event, Some(input.as_bytes()), &no_native_env())
}

/// `SubagentStop` (Phase 3 step 3, item 1): finalizes the ending agent's
/// `task_dispatch` row and deletes its live cap-enforcement state
/// (`subagent_stop::rollup_ending_agent`, best-effort, 2 s bound), then
/// delegates exactly as before -- `SubagentStop` owns no verdict of its own,
/// same shape as `run_session_end`.
fn run_subagent_stop(event: &str) -> Result<u8> {
    let mut input = String::new();
    std::io::stdin()
        .read_to_string(&mut input)
        .context("failed to read hook payload from stdin")?;
    crate::subagent_stop::rollup_ending_agent(&input);
    delegate(event, Some(input.as_bytes()), &no_native_env())
}

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
