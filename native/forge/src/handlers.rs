//! Extension point for native hook bodies.
//!
//! Every hook the Python registry matches for a Bash `PreToolUse` call
//! (`src/tooling/hooks/dispatch/registry.py`) is wired into `registry()`:
//! `crg_refresh_report_pre` (matcher `.*`, so it also fires for every other
//! tool -- out of scope here, only its Bash contribution matters),
//! `crg_gate_verify_bash`, `pre_bash` (command-position deny rules plus the
//! impact-summary analyzer, `crate::bash_guard` and `crate::bash_impact` --
//! `bash_impact` runs directly inside `PreBashGuard`, not as its own registry
//! entry, mirroring how `adapters.pre_bash` calls `_bash_impact.main()` itself
//! rather than through a separate `HookSpec`) and `current_work_guard_bash`.
//! Plus `bash_write_targets`, an informational write-target extractor
//! (`crate::write_targets`) with no Python-recognized name of its own --
//! see `BashWriteTargets`'s own docs for why it folds into `pre_bash` instead.
//!
//! `bash_pretooluse_fully_native` is true once every one of the four real
//! names above is opted in; `dispatch::run_hook` uses it to decide whether a
//! Bash `PreToolUse` call can be answered without starting Python at all
//! (`run_bash_pretooluse_fully_native`, which folds every handler's outcome
//! together with `crate::merge`, exactly as Python's own `merge()` would).
//! Short of that, `native_answers_for_bash` computes just the opted-in
//! subset, each hook's own unmerged answer, for `dispatch::run_hook` to pass
//! to the Python dispatcher via `FORGE_NATIVE_HOOKS`/`FORGE_NATIVE_ANSWERS` so
//! it can splice them into its own per-hook outcome list at their normal
//! registry position (`registry.native_answers`, `runner.dispatch`) instead of
//! re-running their adapters -- so a hook body never runs twice, and Python's
//! `merge()` still decides the final response whenever it still has hooks of
//! its own left to run. This is opt-in only in the sense that
//! `FORGE_NATIVE_HOOKS=""` (set to the empty string) is a documented escape
//! hatch back to the pre-port, all-Python behaviour; unset means "use the
//! native default", not "opt out" -- see `native_hook_names_from_env`.
//!
//! `SessionStart` gets the same partial-splice treatment as `PostToolUse`:
//! `workspace_exposure_session` (the EXPOSED one-liner,
//! `crate::workspace_hygiene`) is the only ported name, and there is
//! deliberately no fully-native fast path for the event --
//! `crg_refresh_report_session`, the legacy `session_start`/`session_handoff`
//! shell bodies and `native_freshness` all still run in Python for every
//! session start, so Python always starts and only this one body is skipped
//! when forge has answered it.

use std::collections::{HashMap, HashSet};
use std::env;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::{json, Value};

use crate::bash_guard;
use crate::bash_impact;
use crate::crg_gate;
use crate::crg_refresh;
use crate::current_work_guard;
use crate::identity;
use crate::instant;
use crate::merge::{self, HookOutcome};
use crate::tool_quiet::{self, QuietConfig};
use crate::workspace_hygiene;
use crate::write_targets;

/// A hook body ported to native Rust.
pub trait NativeHandler {
    /// Registry key, e.g. `"pre_bash"` -- mirrors the Python `HookSpec.name`
    /// it replaces (or, for a handler with no Python HookSpec equivalent,
    /// a name documenting the capability it exposes).
    fn name(&self) -> &'static str;

    /// The Claude Code event this handler answers, e.g. `"PreToolUse"`.
    fn event(&self) -> &'static str;

    /// Run against the raw hook JSON payload, returning the JSON this hook body
    /// contributes to the merged output -- the same shape its Python adapter
    /// returns today, or `Value::Null` for "no contribution" (Python's `None`/
    /// `ALLOW`-equivalent silence).
    fn run(&self, payload: &Value) -> Result<Value>;
}

fn command_from_payload(payload: &Value) -> Option<&str> {
    payload.get("tool_input")?.get("command")?.as_str()
}

/// The bare `{"permissionDecision": "allow"}` shape, matching `adapters.ALLOW`
/// and `_bash_impact`'s `_emit("allow")` with no `additionalContext`.
fn bare_allow() -> Value {
    json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "allow",
        }
    })
}

/// `pre_bash`'s full verdict: `_bash_guard.check` first (a hit denies and
/// short-circuits, `_bash_impact` never runs -- mirrors `adapters.pre_bash`
/// exactly), otherwise `_bash_impact`'s allow/soft_warn classification. Always
/// returns a verdict (never `Value::Null`): a missing command mirrors
/// `adapters.pre_bash`'s own `if not command: return ALLOW` early exit.
pub struct PreBashGuard;

impl NativeHandler for PreBashGuard {
    fn name(&self) -> &'static str {
        "pre_bash"
    }

    fn event(&self) -> &'static str {
        "PreToolUse"
    }

    fn run(&self, payload: &Value) -> Result<Value> {
        let Some(command) = command_from_payload(payload) else {
            return Ok(bare_allow());
        };
        if let Some(reason) = bash_guard::check(command) {
            return Ok(json!({
                "hookSpecificOutput": {
                    "hookEventName": "PreToolUse",
                    "permissionDecision": "deny",
                    "permissionDecisionReason": reason.trim(),
                }
            }));
        }
        match bash_impact::additional_context(command) {
            Some(context) => Ok(json!({
                "hookSpecificOutput": {
                    "hookEventName": "PreToolUse",
                    "permissionDecision": "allow",
                    "additionalContext": context,
                }
            })),
            None => Ok(bare_allow()),
        }
    }
}

/// Informational write-target extraction (`bash_write_targets.py`). Not a real
/// Python `HookSpec` -- `bash_write_targets.repo_write_targets` today runs
/// inside `crg_gate.verify_bash`'s claim logic, which stays in Python (out of
/// scope for this PR). This handler exposes the same extraction natively, under
/// `additionalContext`, as new, additive telemetry that nothing downstream
/// consumes yet -- wiring it into `crg_gate`'s actual claim check is future work
/// (see the PR body's Debt section).
pub struct BashWriteTargets;

impl NativeHandler for BashWriteTargets {
    fn name(&self) -> &'static str {
        "bash_write_targets"
    }

    fn event(&self) -> &'static str {
        "PreToolUse"
    }

    fn run(&self, payload: &Value) -> Result<Value> {
        let Some(command) = command_from_payload(payload) else {
            return Ok(Value::Null);
        };
        let root = crate::interpreter::project_root();
        let targets = write_targets::repo_write_targets(command, &root);
        if targets.is_empty() {
            return Ok(Value::Null);
        }
        Ok(json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "additionalContext": format!(
                    "[forge/bash_write_targets] would write: {}",
                    targets.join(", ")
                ),
            }
        }))
    }
}

/// `crg_refresh_report_pre`: surfaces a background graph-refresh failure or
/// warning once, then clears the marker. Pure file I/O -- see `crg_refresh`'s
/// own module docs for what stays out of scope (the queueing worker).
pub struct CrgRefreshReportPre;

impl NativeHandler for CrgRefreshReportPre {
    fn name(&self) -> &'static str {
        "crg_refresh_report_pre"
    }

    fn event(&self) -> &'static str {
        "PreToolUse"
    }

    fn run(&self, _payload: &Value) -> Result<Value> {
        let root = crate::interpreter::project_root();
        Ok(crg_refresh::failure_output("PreToolUse", &root))
    }
}

/// `crg_gate_verify_bash`: the claim gate applied to a Bash command's own
/// write targets. Resolves `owner`/`repo_root`/`repo_common_dir`/`env` at call
/// time exactly as `adapters.crg_gate_verify_bash` and its `_owner` helper do
/// -- `identity::resolve_owner`'s only error (`OwnerIdentityError`) squashed
/// to `""`, matching `_owner`'s narrower catch (not the CLI-only
/// `_default_owner`, which also swallows `OSError`/`ValueError`).
pub struct CrgGateVerifyBash;

impl NativeHandler for CrgGateVerifyBash {
    fn name(&self) -> &'static str {
        "crg_gate_verify_bash"
    }

    fn event(&self) -> &'static str {
        "PreToolUse"
    }

    fn run(&self, payload: &Value) -> Result<Value> {
        let root = crate::interpreter::project_root();
        let common_dir = crg_gate::checkout_of(&root).map(|(_root, common)| common);
        let env: HashMap<String, String> = env::vars().collect();
        let session_root = crg_gate::session_checkout(payload, &root, common_dir.as_deref());
        let owner = identity::resolve_owner(Some(&session_root), &env).unwrap_or_default();
        Ok(crg_gate::verify_bash(
            payload,
            &owner,
            &root,
            common_dir.as_deref(),
            &env,
        ))
    }
}

/// `current_work_guard_bash`: cooperative `.current_work.md` and local-AI
/// clerical gates for a Bash call. `current_work_guard::run` already handles
/// both the codex and grok payload protocols.
pub struct CurrentWorkGuardBash;

impl NativeHandler for CurrentWorkGuardBash {
    fn name(&self) -> &'static str {
        "current_work_guard_bash"
    }

    fn event(&self) -> &'static str {
        "PreToolUse"
    }

    fn run(&self, payload: &Value) -> Result<Value> {
        Ok(current_work_guard::run(payload))
    }
}

/// Resolves the four env vars `_bash_quiet.py`/`post_tool_quiet.py` read at
/// module scope, so `PostBashQuiet`/`PostToolQuiet` can build a
/// `tool_quiet::QuietConfig` without duplicating the resolution logic.
fn quiet_save_dir(repo_root: &Path) -> PathBuf {
    let configured = env::var("BASH_QUIET_SAVE_DIR").unwrap_or_default();
    let trimmed = configured.trim();
    if trimmed.is_empty() {
        return std::env::temp_dir().join("agent-bash-output");
    }
    let path = PathBuf::from(trimmed);
    if path.is_absolute() {
        path
    } else {
        repo_root.join(path)
    }
}

fn quiet_output_field() -> String {
    env::var("BASH_QUIET_OUTPUT_FIELD").unwrap_or_else(|_| "updatedToolOutput".to_string())
}

/// `int(os.environ.get("BASH_QUIET_LIMIT_BYTES", "8000"))` -- an unparseable
/// value fails loud, matching Python's `int(...)` `ValueError` rather than
/// silently falling back to the default.
fn bash_quiet_limit() -> Result<usize> {
    match env::var("BASH_QUIET_LIMIT_BYTES") {
        Err(_) => Ok(tool_quiet::BASH_QUIET_LIMIT_DEFAULT),
        Ok(raw) => raw
            .parse()
            .with_context(|| format!("BASH_QUIET_LIMIT_BYTES is not an integer: {raw:?}")),
    }
}

/// `_cap()`: `int(os.environ.get("TOOL_OUTPUT_QUIET_BYTES", "16000"))`, plus
/// the `<= 0` disables-bounding reading `post_tool_quiet.bound_response`
/// applies itself. Returns `(cap, disabled)`; `cap` is `0` when disabled.
fn tool_output_quiet_cap() -> Result<(usize, bool)> {
    let raw = match env::var("TOOL_OUTPUT_QUIET_BYTES") {
        Err(_) => return Ok((tool_quiet::TOOL_OUTPUT_QUIET_DEFAULT, false)),
        Ok(raw) => raw,
    };
    let parsed: i64 = raw
        .parse()
        .with_context(|| format!("TOOL_OUTPUT_QUIET_BYTES is not an integer: {raw:?}"))?;
    if parsed <= 0 {
        Ok((0, true))
    } else {
        Ok((parsed as usize, false))
    }
}

/// `post_bash_quiet`: bounds a Bash tool response's `stdout`/`stderr`/`output`
/// fields over `BASH_QUIET_LIMIT_BYTES` (default 8000 bytes), matching
/// `_bash_quiet.hook_output`.
pub struct PostBashQuiet;

impl NativeHandler for PostBashQuiet {
    fn name(&self) -> &'static str {
        "post_bash_quiet"
    }

    fn event(&self) -> &'static str {
        "PostToolUse"
    }

    fn run(&self, payload: &Value) -> Result<Value> {
        let root = crate::interpreter::project_root();
        let save_dir = quiet_save_dir(&root);
        let output_field = quiet_output_field();
        let limit = bash_quiet_limit()?;
        let now_stamp = instant::format_compact_utc(instant::now());
        let cfg = QuietConfig {
            save_dir: &save_dir,
            repo_root: &root,
            now_stamp: &now_stamp,
            output_field: &output_field,
        };
        Ok(tool_quiet::rewrite_envelope_bash(payload, limit, &cfg))
    }
}

/// `post_tool_quiet`: bounds a Read/Grep/MCP tool response over
/// `TOOL_OUTPUT_QUIET_BYTES` (default 16000 bytes; `<= 0` disables), matching
/// `post_tool_quiet.hook_output`. An unrecognized response shape still
/// passes through unbounded, warning on stderr exactly as the Python body
/// does (`post_tool_quiet._warn_unrecognized`).
pub struct PostToolQuiet;

impl NativeHandler for PostToolQuiet {
    fn name(&self) -> &'static str {
        "post_tool_quiet"
    }

    fn event(&self) -> &'static str {
        "PostToolUse"
    }

    fn run(&self, payload: &Value) -> Result<Value> {
        let root = crate::interpreter::project_root();
        let save_dir = quiet_save_dir(&root);
        let output_field = quiet_output_field();
        let (cap, disabled) = tool_output_quiet_cap()?;
        let now_stamp = instant::format_compact_utc(instant::now());
        let cfg = QuietConfig {
            save_dir: &save_dir,
            repo_root: &root,
            now_stamp: &now_stamp,
            output_field: &output_field,
        };
        let (value, warned) = tool_quiet::rewrite_envelope_tool(payload, cap, disabled, &cfg);
        if let Some(kind) = warned {
            eprintln!(
                "post-tool-quiet: unrecognized tool_response shape ({kind}); \
                 passing through unbounded"
            );
        }
        Ok(value)
    }
}

/// `workspace_exposure_session`: the SessionStart EXPOSED summary line
/// (`adapters.workspace_exposure`, exactly `workspace_hygiene.exposure_line`).
/// The repo it reports on is the session's checkout
/// (`interpreter::project_root`), which is what the Python adapter's
/// `ctx.root` resolves to for the same call.
pub struct WorkspaceExposureSession;

impl NativeHandler for WorkspaceExposureSession {
    fn name(&self) -> &'static str {
        "workspace_exposure_session"
    }

    fn event(&self) -> &'static str {
        "SessionStart"
    }

    fn run(&self, _payload: &Value) -> Result<Value> {
        let root = crate::interpreter::project_root();
        Ok(json!({
            "hookSpecificOutput": {
                "hookEventName": "SessionStart",
                "additionalContext": workspace_hygiene::exposure_line(&root),
            }
        }))
    }
}

/// Handlers ported so far.
pub fn registry() -> Vec<Box<dyn NativeHandler>> {
    vec![
        Box::new(PreBashGuard),
        Box::new(BashWriteTargets),
        Box::new(CrgRefreshReportPre),
        Box::new(CrgGateVerifyBash),
        Box::new(CurrentWorkGuardBash),
        Box::new(PostBashQuiet),
        Box::new(PostToolQuiet),
        Box::new(WorkspaceExposureSession),
    ]
}

/// The exact hook names, in the Python registry's own order
/// (`src/tooling/hooks/dispatch/registry.py`), that a `PostToolUse` call
/// matches for the two output-bounding hooks: `post_bash_quiet` (matcher
/// `Bash`) and `post_tool_quiet` (matcher `Read|Grep|mcp__.*`) -- mutually
/// exclusive on `tool_name`, so at most one of them ever contributes an
/// answer for a given call. Other `PostToolUse` hooks
/// (`crg_refresh_report_post`, `crg_graph_refresh`, `post_edit`,
/// `read_budget`, `obsidian_post_edit`, `post_bash_graph`,
/// `context_telemetry`) are not ported: Python still starts for every
/// `PostToolUse` event to run those, but never runs `post_bash_quiet` or
/// `post_tool_quiet`'s own body when this crate already answered it.
pub const POST_TOOL_USE_HOOK_NAMES: [&str; 2] = ["post_bash_quiet", "post_tool_quiet"];

/// Whether `name`'s own Python `HookSpec.matcher` would fire for `tool_name`
/// -- `post_bash_quiet` and `post_tool_quiet`'s matchers specifically, since
/// `native_answers_for_post_tool_use` must not compute (or claim to answer)
/// a hook that Python's own `select()` would never have run for this call.
fn post_tool_use_matches(name: &str, tool_name: &str) -> bool {
    match name {
        "post_bash_quiet" => tool_name == "Bash",
        "post_tool_quiet" => {
            tool_name == "Read" || tool_name == "Grep" || tool_name.starts_with("mcp__")
        }
        _ => false,
    }
}

/// Computes each opted-in, matcher-eligible native `PostToolUse` hook's own
/// answer, keyed by its Python-recognized name -- the `PostToolUse` twin of
/// `native_answers_for_bash`. There is no "fully native" fast path here
/// (unlike Bash `PreToolUse`): `crg_refresh_report_post` and
/// `context_telemetry` match every `PostToolUse` call and are not ported, so
/// Python always still runs for the event; this only lets it skip the two
/// hook bodies this crate already answered.
pub fn native_answers_for_post_tool_use(
    payload: &Value,
    native_hooks: &HashSet<String>,
) -> HashMap<String, Value> {
    let mut answers = HashMap::new();
    let Some(tool_name) = payload.get("tool_name").and_then(Value::as_str) else {
        return answers;
    };
    let handlers = registry();
    for name in POST_TOOL_USE_HOOK_NAMES {
        if !native_hooks.contains(name) || !post_tool_use_matches(name, tool_name) {
            continue;
        }
        let Some(handler) = find_handler(&handlers, name, "PostToolUse") else {
            continue;
        };
        if let Ok(value) = handler.run(payload) {
            answers.insert(name.to_string(), value);
        }
    }
    answers
}

/// The exact hook names, in the Python registry's own order
/// (`src/tooling/hooks/dispatch/registry.py`), that a Bash `PreToolUse` call
/// matches: `crg_refresh_report_pre` (matcher `.*`, which also matches
/// `Bash`), `crg_gate_verify_bash`, `pre_bash`, `current_work_guard_bash` (all
/// matcher `Bash`). `bash_write_targets` never appears here: it has no
/// Python-recognized name of its own, and instead always folds into
/// `pre_bash`'s own `additionalContext` when it is opted in alongside it.
pub const BASH_PRETOOLUSE_HOOK_NAMES: [&str; 4] = [
    "crg_refresh_report_pre",
    "crg_gate_verify_bash",
    "pre_bash",
    "current_work_guard_bash",
];

/// The exact hook name, in the Python registry's own order, that a
/// `SessionStart` call matches and this crate serves: the EXPOSED summary
/// line. The event's other names (`crg_refresh_report_session`,
/// `session_start`, `session_handoff`, `native_freshness`) are not ported;
/// Python still runs them for every session start.
pub const SESSIONSTART_HOOK_NAMES: [&str; 1] = ["workspace_exposure_session"];

/// The `SessionStart` twin of `native_answers_for_post_tool_use`: the
/// opted-in subset of `SESSIONSTART_HOOK_NAMES`, each hook's own unmerged
/// answer. There is no "fully native" fast path for this event (see module
/// docs); this only lets the Python dispatcher skip the one body this crate
/// already answered. Unlike the tool events the answer needs no payload --
/// the line depends on the checkout, not on the hook input.
pub fn native_answers_for_session_start(native_hooks: &HashSet<String>) -> HashMap<String, Value> {
    let mut answers = HashMap::new();
    let handlers = registry();
    for name in SESSIONSTART_HOOK_NAMES {
        if !native_hooks.contains(name) {
            continue;
        }
        let Some(handler) = find_handler(&handlers, name, "SessionStart") else {
            continue;
        };
        if let Ok(value) = handler.run(&Value::Null) {
            answers.insert(name.to_string(), value);
        }
    }
    answers
}

/// True once every name in `BASH_PRETOOLUSE_HOOK_NAMES` is opted in: forge has
/// full native coverage for a Bash `PreToolUse` call and `dispatch::run_hook`
/// can answer it without starting Python at all.
pub fn bash_pretooluse_fully_native(native_hooks: &HashSet<String>) -> bool {
    BASH_PRETOOLUSE_HOOK_NAMES
        .iter()
        .all(|name| native_hooks.contains(*name))
}

/// The hooks forge serves natively when the caller expresses no preference at
/// all (`FORGE_NATIVE_HOOKS` unset) -- the native path is the *default* as of
/// this PR, not an opt-in. The four `BASH_PRETOOLUSE_HOOK_NAMES`, the two
/// `POST_TOOL_USE_HOOK_NAMES` and `workspace_exposure_session` are the names
/// Python's registry actually recognizes; `"bash_write_targets"` gates
/// forge's own additive telemetry handler and is a no-op name on the Python
/// side (there is no such `HookSpec`).
fn default_native_hook_names() -> HashSet<String> {
    let mut names: HashSet<String> = BASH_PRETOOLUSE_HOOK_NAMES
        .iter()
        .map(|s| s.to_string())
        .collect();
    names.insert("bash_write_targets".to_string());
    names.extend(POST_TOOL_USE_HOOK_NAMES.iter().map(|s| s.to_string()));
    names.extend(SESSIONSTART_HOOK_NAMES.iter().map(|s| s.to_string()));
    names
}

/// Parses `FORGE_NATIVE_HOOKS` (a comma-separated list of hook names, e.g.
/// `"pre_bash"`) into the set of hook names forge should answer natively.
/// Unset means "use the default" (`default_native_hook_names`) -- the native
/// path is on by default. Set to the empty string is a deliberate escape
/// hatch back to full, pre-port Python delegation (documented in the PR body
/// and in `dispatch::run_hook`); set to any other value uses exactly the
/// names given, trimmed, empties dropped -- mainly useful for tests that want
/// to exercise one native handler without the other.
pub fn native_hook_names_from_env() -> HashSet<String> {
    match env::var("FORGE_NATIVE_HOOKS") {
        Err(_) => default_native_hook_names(),
        Ok(raw) => raw
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect(),
    }
}

/// Folds `extra`'s `hookSpecificOutput.additionalContext` (if any) into
/// `answer`'s, concatenating blank-line separated (matching
/// `dispatch.merge`'s `SEPARATOR`) rather than overwriting, since `answer` may
/// already carry `_bash_impact`'s own context.
fn fold_additional_context(answer: &mut Value, extra: &Value) {
    let Some(text) = extra
        .get("hookSpecificOutput")
        .and_then(|output| output.get("additionalContext"))
        .and_then(Value::as_str)
    else {
        return;
    };
    let Some(map) = answer
        .get_mut("hookSpecificOutput")
        .and_then(Value::as_object_mut)
    else {
        return;
    };
    match map.get("additionalContext").and_then(Value::as_str) {
        Some(existing) if !existing.is_empty() => {
            let combined = format!("{existing}\n\n{text}");
            map.insert("additionalContext".to_string(), json!(combined));
        }
        _ => {
            map.insert("additionalContext".to_string(), json!(text));
        }
    }
}

fn find_handler<'a>(
    handlers: &'a [Box<dyn NativeHandler>],
    name: &str,
    event: &str,
) -> Option<&'a dyn NativeHandler> {
    handlers
        .iter()
        .find(|h| h.name() == name && h.event() == event)
        .map(Box::as_ref)
}

/// Runs `pre_bash`, folding `bash_write_targets`'s informational contribution
/// into it when both are opted in -- the one merge decision that predates
/// `crate::merge` and stays hand-rolled here, since `bash_write_targets` is
/// forge-only telemetry with no Python-recognized name for `crate::merge` (or
/// Python's own `merge()`) to fold in on its own.
fn pre_bash_answer(
    handlers: &[Box<dyn NativeHandler>],
    payload: &Value,
    native_hooks: &HashSet<String>,
) -> Result<Value> {
    let pre_bash =
        find_handler(handlers, "pre_bash", "PreToolUse").expect("pre_bash always registered");
    let mut answer = pre_bash.run(payload)?;
    if native_hooks.contains("bash_write_targets") {
        if let Some(write_targets) = find_handler(handlers, "bash_write_targets", "PreToolUse") {
            if let Ok(extra) = write_targets.run(payload) {
                fold_additional_context(&mut answer, &extra);
            }
        }
    }
    Ok(answer)
}

/// Computes each opted-in native hook's own, unmerged answer for a Bash
/// `PreToolUse` payload, keyed by its Python-recognized name -- for splicing
/// into Python's own per-hook outcome list via `FORGE_NATIVE_ANSWERS`
/// (`dispatch::run_hook`), one entry per name in `BASH_PRETOOLUSE_HOOK_NAMES`
/// that `native_hooks` opts into. Empty for anything that isn't a Bash
/// `PreToolUse` call, or when `native_hooks` opts into none of the four names
/// (the `FORGE_NATIVE_HOOKS=""` escape hatch passes an empty set here).
pub fn native_answers_for_bash(
    payload: &Value,
    native_hooks: &HashSet<String>,
) -> HashMap<String, Value> {
    let mut answers = HashMap::new();
    if payload.get("tool_name").and_then(Value::as_str) != Some("Bash") {
        return answers;
    }
    let handlers = registry();
    for name in BASH_PRETOOLUSE_HOOK_NAMES {
        if !native_hooks.contains(name) {
            continue;
        }
        let computed = if name == "pre_bash" {
            pre_bash_answer(&handlers, payload, native_hooks)
        } else {
            find_handler(&handlers, name, "PreToolUse")
                .expect("every BASH_PRETOOLUSE_HOOK_NAMES entry is registered")
                .run(payload)
        };
        if let Ok(value) = computed {
            answers.insert(name.to_string(), value);
        }
    }
    answers
}

/// The final, fully-merged answer for a Bash `PreToolUse` call once
/// `bash_pretooluse_fully_native` is true: every handler in
/// `BASH_PRETOOLUSE_HOOK_NAMES` runs, `bash_write_targets` folds into
/// `pre_bash` exactly as in `native_answers_for_bash`, and `crate::merge`
/// folds the four real outcomes together exactly as Python's own `merge()`
/// would -- so a caller can print this and never start Python for the event.
/// A handler that returns `Err` becomes a `HookOutcome` error, matching how
/// `runner.run_one` turns an adapter exception into `HookOutcome.error`
/// instead of dropping the call.
pub fn run_bash_pretooluse_fully_native(payload: &Value, native_hooks: &HashSet<String>) -> Value {
    let handlers = registry();
    let outcomes: Vec<HookOutcome> = BASH_PRETOOLUSE_HOOK_NAMES
        .iter()
        .map(|name| {
            let computed = if *name == "pre_bash" {
                pre_bash_answer(&handlers, payload, native_hooks)
            } else {
                find_handler(&handlers, name, "PreToolUse")
                    .expect("every BASH_PRETOOLUSE_HOOK_NAMES entry is registered")
                    .run(payload)
            };
            let fail_closed = *name == "pre_bash";
            match computed {
                Ok(output) => HookOutcome {
                    name: (*name).to_string(),
                    output,
                    error: None,
                    fail_closed,
                },
                Err(err) => HookOutcome {
                    name: (*name).to_string(),
                    output: Value::Null,
                    error: Some(format!("{err:#}")),
                    fail_closed,
                },
            }
        })
        .collect();
    merge::merge("PreToolUse", &outcomes)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::Mutex;

    /// `FORGE_NATIVE_HOOKS`/`CLAUDE_PROJECT_DIR`/`CRG_GATE_STATE_DIR` are
    /// process-global env vars that several tests below mutate; without
    /// serializing them, a test reading defaults can observe another
    /// (concurrently running) test's override. Every test in this module
    /// that sets, unsets, or depends on the default of any of them takes
    /// this lock first.
    pub(crate) static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn all_four() -> HashSet<String> {
        BASH_PRETOOLUSE_HOOK_NAMES
            .iter()
            .map(|s| s.to_string())
            .collect()
    }

    #[test]
    fn registry_covers_every_bash_pretooluse_hook_plus_write_targets() {
        // 5 Bash PreToolUse handlers (4 real HookSpec names plus
        // bash_write_targets) + PostBashQuiet + PostToolQuiet
        // + WorkspaceExposureSession.
        assert_eq!(registry().len(), 8);
        assert!(bash_pretooluse_fully_native(&all_four()));
        let mut missing_one = all_four();
        missing_one.remove("current_work_guard_bash");
        assert!(!bash_pretooluse_fully_native(&missing_one));
        assert!(!bash_pretooluse_fully_native(&HashSet::new()));
    }

    #[test]
    fn env_parsing_trims_and_drops_empties() {
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        std::env::set_var("FORGE_NATIVE_HOOKS", " pre_bash ,, bash_write_targets");
        let names = native_hook_names_from_env();
        assert!(names.contains("pre_bash"));
        assert!(names.contains("bash_write_targets"));
        assert_eq!(names.len(), 2);
        std::env::remove_var("FORGE_NATIVE_HOOKS");
    }

    #[test]
    fn env_unset_defaults_to_native_on_for_every_bash_hook() {
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        std::env::remove_var("FORGE_NATIVE_HOOKS");
        let names = native_hook_names_from_env();
        for name in BASH_PRETOOLUSE_HOOK_NAMES {
            assert!(names.contains(name), "{name} missing from the default set");
        }
        for name in POST_TOOL_USE_HOOK_NAMES {
            assert!(names.contains(name), "{name} missing from the default set");
        }
        for name in SESSIONSTART_HOOK_NAMES {
            assert!(names.contains(name), "{name} missing from the default set");
        }
        assert!(names.contains("bash_write_targets"));
        assert!(bash_pretooluse_fully_native(&names));
    }

    #[test]
    fn session_start_answers_carry_the_exposure_line_once_opted_in() {
        use std::process::Command;

        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // The exposure line resolves the integration line, which reads
        // CONDUCTOR_INTEGRATION_BRANCH; workspace_hygiene's env-override test
        // sets that variable under this same lock.
        let _integration = crate::workspace_hygiene::INTEGRATION_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // A scratch git repo so the line is a real count, not the degrade
        // text; CLAUDE_PROJECT_DIR is what the handler reports on.
        let dir = std::env::temp_dir().join(format!(
            "forge-handlers-session-start-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Command::new("git")
            .args(["init", "--quiet", "-b", "trunk"])
            .current_dir(&dir)
            .output()
            .expect("git init");
        Command::new("git")
            .args(["config", "user.email", "forge@example.invalid"])
            .current_dir(&dir)
            .output()
            .expect("git config");
        Command::new("git")
            .args(["config", "user.name", "forge"])
            .current_dir(&dir)
            .output()
            .expect("git config");
        std::fs::write(dir.join("seed.txt"), b"seed\n").unwrap();
        Command::new("git")
            .args(["add", "seed.txt"])
            .current_dir(&dir)
            .output()
            .expect("git add");
        Command::new("git")
            .args(["commit", "--quiet", "-m", "seed"])
            .current_dir(&dir)
            .output()
            .expect("git commit");
        std::env::set_var("CLAUDE_PROJECT_DIR", &dir);

        let empty = HashSet::new();
        assert!(native_answers_for_session_start(&empty).is_empty());
        let mut opted_in = HashSet::new();
        opted_in.insert("workspace_exposure_session".to_string());
        let answers = native_answers_for_session_start(&opted_in);

        std::env::remove_var("CLAUDE_PROJECT_DIR");
        std::fs::remove_dir_all(&dir).ok();
        let out = answers.get("workspace_exposure_session").expect("answered");
        assert_eq!(
            out["hookSpecificOutput"]["hookEventName"],
            json!("SessionStart")
        );
        let line = out["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .expect("the EXPOSED line");
        // trunk + no remote + no conductor table: one local-only commit and
        // no integration line to judge worktrees against, so "unknown".
        assert!(
            line.starts_with(
                "EXPOSED: 1 local-only commit(s), 0 stale dirty file(s), unknown finished worktree(s)"
            ),
            "{line}"
        );
    }

    #[test]
    fn env_set_empty_is_the_documented_escape_hatch() {
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        std::env::set_var("FORGE_NATIVE_HOOKS", "");
        let names = native_hook_names_from_env();
        assert!(names.is_empty());
        assert!(!bash_pretooluse_fully_native(&names));
        std::env::remove_var("FORGE_NATIVE_HOOKS");
    }

    #[test]
    fn native_answers_denies_natively_only_when_opted_in() {
        let payload =
            json!({"tool_name": "Bash", "tool_input": {"command": "git push --force origin main"}});
        let empty = HashSet::new();
        assert!(native_answers_for_bash(&payload, &empty).is_empty());

        let mut opted_in = HashSet::new();
        opted_in.insert("pre_bash".to_string());
        let answers = native_answers_for_bash(&payload, &opted_in);
        let out = answers.get("pre_bash").expect("pre_bash answered");
        assert_eq!(
            out["hookSpecificOutput"]["permissionDecision"],
            json!("deny")
        );
    }

    #[test]
    fn native_answers_answers_a_bare_allow_verdict_too() {
        let payload = json!({"tool_name": "Bash", "tool_input": {"command": "echo hi"}});
        let mut opted_in = HashSet::new();
        opted_in.insert("pre_bash".to_string());
        let answers = native_answers_for_bash(&payload, &opted_in);
        let out = answers.get("pre_bash").expect("pre_bash answered");
        assert_eq!(
            out["hookSpecificOutput"]["permissionDecision"],
            json!("allow")
        );
        assert!(out["hookSpecificOutput"]["additionalContext"].is_null());
    }

    #[test]
    fn native_answers_answers_an_allow_with_impact_context() {
        // `find ... -delete` (unlike `rm -rf`) isn't a bash_guard deny target,
        // so a real directory here reaches `_bash_impact`'s soft_warn path --
        // an allow verdict that also carries additionalContext.
        let dir = std::env::temp_dir().join("forge_precheck_impact_test");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("f.txt"), b"data").unwrap();
        let payload = json!({
            "tool_name": "Bash",
            "tool_input": {"command": format!("find {} -delete", dir.display())}
        });
        let mut opted_in = HashSet::new();
        opted_in.insert("pre_bash".to_string());
        let answers = native_answers_for_bash(&payload, &opted_in);
        std::fs::remove_dir_all(&dir).ok();
        let out = answers.get("pre_bash").expect("pre_bash answered");
        assert_eq!(
            out["hookSpecificOutput"]["permissionDecision"],
            json!("allow")
        );
        assert!(out["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .expect("impact context")
            .contains("find -delete"));
    }

    #[test]
    fn native_answers_ignores_non_bash_tools() {
        let payload =
            json!({"tool_name": "Write", "tool_input": {"file_path": "x", "content": "y"}});
        let mut opted_in = HashSet::new();
        opted_in.insert("pre_bash".to_string());
        assert!(native_answers_for_bash(&payload, &opted_in).is_empty());
    }

    /// `rm`'s "dangerous target" deny rule only fires for targets outside the
    /// invocation's own directory tree (absolute paths, `~`, `..`), so a denied
    /// `rm` never has a repo-relative write target under the *real* project
    /// root. To exercise the merge with a target that lands inside some repo
    /// root, these two tests point `CLAUDE_PROJECT_DIR` (what
    /// `interpreter::project_root` reads) at the command's own absolute
    /// prefix. Mutates a process-wide env var like the pre-existing
    /// `env_parsing_trims_and_drops_empties` test does; restored before return.
    #[test]
    fn native_answers_folds_write_targets_into_a_native_deny_when_both_opted_in() {
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        std::env::set_var("CLAUDE_PROJECT_DIR", "/home/tim");
        let payload = json!({
            "tool_name": "Bash",
            "tool_input": {"command": "rm -rf /home/tim/stuff"}
        });
        let mut opted_in = HashSet::new();
        opted_in.insert("pre_bash".to_string());
        opted_in.insert("bash_write_targets".to_string());
        let answers = native_answers_for_bash(&payload, &opted_in);
        std::env::remove_var("CLAUDE_PROJECT_DIR");
        let out = answers.get("pre_bash").expect("pre_bash answered");
        assert_eq!(
            out["hookSpecificOutput"]["permissionDecision"],
            json!("deny")
        );
        let context = out["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .expect("bash_write_targets context should be folded in");
        assert!(context.contains("stuff"));
    }

    #[test]
    fn native_answers_omits_write_targets_context_when_only_pre_bash_is_opted_in() {
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        std::env::set_var("CLAUDE_PROJECT_DIR", "/home/tim");
        let payload = json!({
            "tool_name": "Bash",
            "tool_input": {"command": "rm -rf /home/tim/stuff"}
        });
        let mut opted_in = HashSet::new();
        opted_in.insert("pre_bash".to_string());
        let answers = native_answers_for_bash(&payload, &opted_in);
        std::env::remove_var("CLAUDE_PROJECT_DIR");
        let out = answers.get("pre_bash").expect("pre_bash answered");
        assert!(out["hookSpecificOutput"]["additionalContext"].is_null());
    }

    #[test]
    fn crg_refresh_report_pre_handler_is_silent_with_no_marker_file() {
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // `CrgRefreshReportPre.run` reads the store through `store_dir`, which
        // prefers the process-global `CRG_DATA_DIR` -- a var THIS module's lock
        // does not cover. `crg_refresh`'s own tests mutate it under their own
        // lock, and two different mutexes guarding the same env var provide no
        // mutual exclusion at all (its `pub(crate)` exists for exactly this),
        // so take both: this module's for `CLAUDE_PROJECT_DIR`, theirs for
        // `CRG_DATA_DIR`.
        let _crg_guard = crate::crg_refresh::tests::ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir =
            std::env::temp_dir().join(format!("forge-handlers-crg-refresh-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("CLAUDE_PROJECT_DIR", &dir);
        let out = CrgRefreshReportPre.run(&json!({})).unwrap();
        std::env::remove_var("CLAUDE_PROJECT_DIR");
        std::fs::remove_dir_all(&dir).ok();
        assert!(out.is_null());
    }

    #[test]
    fn crg_gate_verify_bash_handler_allows_a_read_only_command() {
        // No write targets at all, so `verify_bash` never touches the
        // graph-used state file or the claim store -- this exercises the
        // owner/repo_root/env plumbing without needing a scratch state dir.
        let payload = json!({"tool_name": "Bash", "tool_input": {"command": "echo hi"}});
        let out = CrgGateVerifyBash.run(&payload).unwrap();
        assert!(out.is_null());
    }

    #[test]
    fn current_work_guard_bash_handler_denies_direct_shell_access() {
        let payload =
            json!({"tool_name": "Bash", "tool_input": {"command": "cat .current_work.md"}});
        let out = CurrentWorkGuardBash.run(&payload).unwrap();
        assert_eq!(
            out["hookSpecificOutput"]["permissionDecision"],
            json!("deny")
        );
    }

    #[test]
    fn fully_native_merges_all_four_outcomes_in_registry_order() {
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = std::env::temp_dir().join(format!(
            "forge-handlers-fully-native-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("CLAUDE_PROJECT_DIR", &dir);
        let payload = json!({"tool_name": "Bash", "tool_input": {"command": "echo hi"}});
        let out = run_bash_pretooluse_fully_native(&payload, &all_four());
        std::env::remove_var("CLAUDE_PROJECT_DIR");
        std::fs::remove_dir_all(&dir).ok();
        // `pre_bash` always casts an explicit "allow" vote (`bare_allow`),
        // even with nothing else to report; the other three hooks abstain
        // (`Value::Null`) for a plain `echo`. One allow vote and no deny/ask
        // votes: `merge()`'s winner is that lone allow, with no reason to
        // join since `bare_allow` carries none.
        assert_eq!(
            out,
            json!({"hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "allow"}})
        );
    }

    #[test]
    fn fully_native_denies_when_pre_bash_denies() {
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = std::env::temp_dir().join(format!(
            "forge-handlers-fully-native-deny-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("CLAUDE_PROJECT_DIR", &dir);
        let payload =
            json!({"tool_name": "Bash", "tool_input": {"command": "git push --force origin main"}});
        let out = run_bash_pretooluse_fully_native(&payload, &all_four());
        std::env::remove_var("CLAUDE_PROJECT_DIR");
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(
            out["hookSpecificOutput"]["permissionDecision"],
            json!("deny")
        );
    }

    #[test]
    fn post_tool_matchers_mirror_the_registry_matchers() {
        // `post_bash_quiet` is Bash-only; `post_tool_quiet` is the
        // Read/Grep/mcp__ triple; anything else claims no match, so a native
        // answer is never promised for a hook Python's own `select()` would
        // not have run.
        assert!(post_tool_use_matches("post_bash_quiet", "Bash"));
        assert!(!post_tool_use_matches("post_bash_quiet", "Read"));
        assert!(post_tool_use_matches("post_tool_quiet", "Read"));
        assert!(post_tool_use_matches("post_tool_quiet", "Grep"));
        assert!(post_tool_use_matches(
            "post_tool_quiet",
            "mcp__code_review_graph__x"
        ));
        assert!(!post_tool_use_matches("post_tool_quiet", "Bash"));
        assert!(!post_tool_use_matches("post_tool_quiet", "Edit"));
        assert!(!post_tool_use_matches("context_telemetry", "Read"));
    }

    #[test]
    fn post_tool_answers_cover_only_the_opted_in_matcher_eligible_hooks() {
        let payload = serde_json::json!({"tool_name": "Bash"});
        let mut hooks = HashSet::new();
        hooks.insert("post_bash_quiet".to_string());
        let answers = native_answers_for_post_tool_use(&payload, &hooks);
        assert!(
            answers.contains_key("post_bash_quiet"),
            "the opted-in, matcher-eligible hook must be answered: {answers:?}"
        );
        // `post_tool_quiet` opted in but its matcher excludes Bash: no answer
        // may be claimed for a hook Python would not have run.
        hooks.insert("post_tool_quiet".to_string());
        let answers = native_answers_for_post_tool_use(&payload, &hooks);
        assert!(!answers.contains_key("post_tool_quiet"));
    }
}
