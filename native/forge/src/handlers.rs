//! Extension point for native hook bodies.
//!
//! Two hook bodies are wired into `registry()`: `pre_bash` (command-position
//! deny rules plus the impact-summary analyzer, `crate::bash_guard` and
//! `crate::bash_impact` -- `bash_impact` runs directly inside `PreBashGuard`,
//! not as its own registry entry, mirroring how `adapters.pre_bash` calls
//! `_bash_impact.main()` itself rather than through a separate `HookSpec`) and
//! an informational write-target extractor (`crate::write_targets`). Neither
//! makes `fully_native("PreToolUse")` true --
//! Bash PreToolUse also matches `crg_refresh_report_pre`, `crg_gate_verify_bash`
//! and `current_work_guard_bash` in the Python registry
//! (`src/tooling/hooks/dispatch/registry.py`), none of which are ported. So the
//! event is never claimed as fully native; `dispatch::run_hook` always still
//! invokes the Python dispatcher once per `PreToolUse` call, for those three.
//!
//! What IS wired: `precheck_pretooluse_bash`, used by `dispatch::run_hook` to
//! compute `pre_bash`'s full verdict (guard + impact, exactly what
//! `adapters.pre_bash` would have produced) without spawning Python for it.
//! `dispatch::run_hook` then tells the Python dispatcher, via `FORGE_NATIVE_HOOKS`
//! and `FORGE_NATIVE_ANSWERS`, to splice this precomputed answer in for the
//! `pre_bash` spec instead of re-running its adapter -- so the hook body runs
//! exactly once, in Rust, and Python's own `merge()` still folds it into the
//! final response at its correct registry position alongside the three
//! unported hooks. This is opt-in only in the sense that
//! `FORGE_NATIVE_HOOKS=""` (set to the empty string) is a documented escape
//! hatch back to the pre-port, all-Python behaviour; unset means "use the
//! native default", not "opt out" -- see `native_hook_names_from_env`.

use std::collections::HashSet;
use std::env;

use anyhow::Result;
use serde_json::{json, Value};

use crate::bash_guard;
use crate::bash_impact;
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

/// Handlers ported so far.
pub fn registry() -> Vec<Box<dyn NativeHandler>> {
    vec![Box::new(PreBashGuard), Box::new(BashWriteTargets)]
}

/// True once every hook the Python registry matches for `event` (any tool) is
/// covered by `registry()` and `dispatch::run_hook` can skip Python entirely.
/// Always false today: `PreToolUse` alone still needs `crg_refresh_report_pre`,
/// `crg_gate_verify_bash` and `current_work_guard_bash`, none of which are
/// ported. Kept as an explicit function (rather than deleted) so the day those
/// land, flipping this on is a one-line, reviewable change instead of a design
/// decision made under a deadline.
pub fn fully_native(_event: &str) -> bool {
    false
}

/// The hooks forge serves natively when the caller expresses no preference at
/// all (`FORGE_NATIVE_HOOKS` unset) -- the native path is the *default* as of
/// this PR, not an opt-in. `"pre_bash"` is the one name Python's registry
/// actually recognizes (see `registry.natively_served` in
/// `src/tooling/hooks/dispatch/registry.py`); `"bash_write_targets"` gates
/// forge's own additive telemetry handler and is a no-op name on the Python
/// side (there is no such `HookSpec`).
fn default_native_hook_names() -> HashSet<String> {
    ["pre_bash", "bash_write_targets"]
        .into_iter()
        .map(str::to_string)
        .collect()
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

/// Computes `pre_bash`'s full native verdict for a Bash `PreToolUse` call --
/// guard + impact, folding `bash_write_targets`'s informational contribution
/// in when it is also opted in. Returns `None` only when this isn't a Bash
/// `PreToolUse` call at all, or `pre_bash` itself isn't opted in via
/// `native_hooks` (the `FORGE_NATIVE_HOOKS=""` escape hatch passes an empty
/// set here). Unlike before this PR, this answers ALLOW verdicts too, not
/// just DENY -- `_bash_impact` is ported, so there is no longer a hidden
/// Python-only contribution being silently dropped on the allow path.
pub fn precheck_pretooluse_bash(payload: &Value, native_hooks: &HashSet<String>) -> Option<Value> {
    if payload.get("tool_name").and_then(Value::as_str) != Some("Bash") {
        return None;
    }
    if !native_hooks.contains("pre_bash") {
        return None;
    }
    let handlers = registry();
    let pre_bash = handlers
        .iter()
        .find(|h| h.name() == "pre_bash" && h.event() == "PreToolUse")?;
    let mut answer = pre_bash.run(payload).ok()?;

    if native_hooks.contains("bash_write_targets") {
        if let Some(write_targets) = handlers
            .iter()
            .find(|h| h.name() == "bash_write_targets" && h.event() == "PreToolUse")
        {
            if let Ok(extra) = write_targets.run(payload) {
                fold_additional_context(&mut answer, &extra);
            }
        }
    }

    Some(answer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_event_claims_full_native_coverage_yet() {
        assert!(!fully_native("PreToolUse"));
        assert!(!fully_native("PostToolUse"));
        assert_eq!(registry().len(), 2);
    }

    #[test]
    fn env_parsing_trims_and_drops_empties() {
        std::env::set_var("FORGE_NATIVE_HOOKS", " pre_bash ,, bash_write_targets");
        let names = native_hook_names_from_env();
        assert!(names.contains("pre_bash"));
        assert!(names.contains("bash_write_targets"));
        assert_eq!(names.len(), 2);
        std::env::remove_var("FORGE_NATIVE_HOOKS");
    }

    #[test]
    fn env_unset_defaults_to_native_on() {
        std::env::remove_var("FORGE_NATIVE_HOOKS");
        let names = native_hook_names_from_env();
        assert!(names.contains("pre_bash"));
        assert!(names.contains("bash_write_targets"));
    }

    #[test]
    fn env_set_empty_is_the_documented_escape_hatch() {
        std::env::set_var("FORGE_NATIVE_HOOKS", "");
        assert!(native_hook_names_from_env().is_empty());
        std::env::remove_var("FORGE_NATIVE_HOOKS");
    }

    #[test]
    fn precheck_denies_natively_only_when_opted_in() {
        let payload =
            json!({"tool_name": "Bash", "tool_input": {"command": "git push --force origin main"}});
        let empty = HashSet::new();
        assert!(precheck_pretooluse_bash(&payload, &empty).is_none());

        let mut opted_in = HashSet::new();
        opted_in.insert("pre_bash".to_string());
        let out = precheck_pretooluse_bash(&payload, &opted_in).expect("deny verdict");
        assert_eq!(
            out["hookSpecificOutput"]["permissionDecision"],
            json!("deny")
        );
    }

    #[test]
    fn precheck_answers_a_bare_allow_verdict_too() {
        let payload = json!({"tool_name": "Bash", "tool_input": {"command": "echo hi"}});
        let mut opted_in = HashSet::new();
        opted_in.insert("pre_bash".to_string());
        let out = precheck_pretooluse_bash(&payload, &opted_in).expect("allow verdict");
        assert_eq!(
            out["hookSpecificOutput"]["permissionDecision"],
            json!("allow")
        );
        assert!(out["hookSpecificOutput"]["additionalContext"].is_null());
    }

    #[test]
    fn precheck_answers_an_allow_with_impact_context() {
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
        let out = precheck_pretooluse_bash(&payload, &opted_in).expect("allow verdict");
        std::fs::remove_dir_all(&dir).ok();
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
    fn precheck_ignores_non_bash_tools() {
        let payload =
            json!({"tool_name": "Write", "tool_input": {"file_path": "x", "content": "y"}});
        let mut opted_in = HashSet::new();
        opted_in.insert("pre_bash".to_string());
        assert!(precheck_pretooluse_bash(&payload, &opted_in).is_none());
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
    fn precheck_folds_write_targets_into_a_native_deny_when_both_opted_in() {
        std::env::set_var("CLAUDE_PROJECT_DIR", "/home/tim");
        let payload = json!({
            "tool_name": "Bash",
            "tool_input": {"command": "rm -rf /home/tim/stuff"}
        });
        let mut opted_in = HashSet::new();
        opted_in.insert("pre_bash".to_string());
        opted_in.insert("bash_write_targets".to_string());
        let out = precheck_pretooluse_bash(&payload, &opted_in).expect("deny verdict");
        std::env::remove_var("CLAUDE_PROJECT_DIR");
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
    fn precheck_omits_write_targets_context_when_only_pre_bash_is_opted_in() {
        std::env::set_var("CLAUDE_PROJECT_DIR", "/home/tim");
        let payload = json!({
            "tool_name": "Bash",
            "tool_input": {"command": "rm -rf /home/tim/stuff"}
        });
        let mut opted_in = HashSet::new();
        opted_in.insert("pre_bash".to_string());
        let out = precheck_pretooluse_bash(&payload, &opted_in).expect("deny verdict");
        std::env::remove_var("CLAUDE_PROJECT_DIR");
        assert!(out["hookSpecificOutput"]["additionalContext"].is_null());
    }
}
