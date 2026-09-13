//! Extension point for native hook bodies.
//!
//! Two hook bodies are ported and registered here: `pre_bash` (command-position
//! deny rules, `crate::bash_guard`) and an informational write-target extractor
//! (`crate::write_targets`). Neither makes `fully_native("PreToolUse")` true --
//! Bash PreToolUse also matches `crg_refresh_report_pre`, `crg_gate_verify_bash`
//! and `current_work_guard_bash` in the Python registry
//! (`src/tooling/hooks/dispatch/registry.py`), and `pre_bash`'s own Python adapter
//! runs `_bash_impact.main()` on the allow path, which is not ported. So the
//! event is never claimed as fully native; `dispatch::run_hook` keeps delegating
//! to Python by default.
//!
//! What IS wired: `precheck_pretooluse_bash`, used by `dispatch::run_hook` only
//! when the caller opts in via `FORGE_NATIVE_HOOKS` (see `native_hook_names_from_env`).
//! It can answer a Bash PreToolUse call natively, with no Python subprocess at
//! all, in exactly one case: the native `pre_bash` verdict is a DENY. That case is
//! provably byte-identical to full delegation for `pre_bash`'s own contribution
//! (Python's `pre_bash` adapter also skips `_bash_impact` once `_bash_guard.check`
//! denies -- see `adapters.pre_bash`). It is NOT safe to bypass on ALLOW (loses
//! `_bash_impact`'s contribution) and, even on DENY, bypassing means the other
//! three Bash PreToolUse hooks never run at all -- their votes are moot once any
//! hook denies (deny beats everything in `merge.py`), but any side effects or
//! `additionalContext`/`systemMessage` they might have produced independently of
//! the vote are lost. That tradeoff is why this stays opt-in: default behaviour
//! (`FORGE_NATIVE_HOOKS` unset) is unchanged from before this PR. Documented as
//! debt in the PR body.

use std::collections::HashSet;
use std::env;

use anyhow::Result;
use serde_json::{json, Value};

use crate::bash_guard;
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

/// `pre_bash`'s command-position deny rules (`_bash_guard.py`). Returns the
/// exact `hookSpecificOutput` shape `adapters.pre_bash` returns on deny; `null`
/// otherwise (the ALLOW path still needs Python's `_bash_impact`, so this
/// handler never claims a positive allow verdict of its own).
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
            return Ok(Value::Null);
        };
        match bash_guard::check(command) {
            Some(reason) => Ok(json!({
                "hookSpecificOutput": {
                    "hookEventName": "PreToolUse",
                    "permissionDecision": "deny",
                    "permissionDecisionReason": reason.trim(),
                }
            })),
            None => Ok(Value::Null),
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

/// Parses `FORGE_NATIVE_HOOKS` (a comma-separated list of hook names, e.g.
/// `"pre_bash"`) into the set of hook names the caller has told forge it may
/// answer natively. Empty (including unset) means "opt out, behave exactly as
/// before this PR" -- see the module doc comment for why this defaults off.
pub fn native_hook_names_from_env() -> HashSet<String> {
    env::var("FORGE_NATIVE_HOOKS")
        .ok()
        .map(|raw| {
            raw.split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// Merges `extra`'s `hookSpecificOutput.additionalContext` (if any) into
/// `answer`'s, so a fully-native reply can still surface an informational
/// handler's contribution even though Python -- the thing that would normally
/// carry it -- never runs on this path.
fn merge_additional_context(answer: &mut Value, extra: &Value) {
    let Some(text) = extra
        .get("hookSpecificOutput")
        .and_then(|output| output.get("additionalContext"))
        .and_then(Value::as_str)
    else {
        return;
    };
    if let Some(map) = answer
        .get_mut("hookSpecificOutput")
        .and_then(Value::as_object_mut)
    {
        map.insert("additionalContext".to_string(), json!(text));
    }
}

/// Attempts to answer a `PreToolUse` call for the Bash tool entirely natively.
///
/// Looks up handlers by name in `registry()` (rather than constructing them
/// directly) so registering a handler here is the same act as wiring it into
/// routing. Returns `Some(json)` only for the one case documented at module
/// level that is safe to fully bypass Python for: `pre_bash` is opted in via
/// `native_hooks` and its native verdict is a DENY -- in which case
/// `bash_write_targets`'s informational contribution (if also opted in) is
/// folded into the same answer, since Python won't run to add it itself.
/// Returns `None` for every other case (allow verdict, non-Bash tool,
/// unparseable payload, or `pre_bash` not opted in) so the caller falls
/// through to full Python delegation.
pub fn precheck_pretooluse_bash(payload: &Value, native_hooks: &HashSet<String>) -> Option<Value> {
    if payload.get("tool_name").and_then(Value::as_str) != Some("Bash") {
        return None;
    }
    let handlers = registry();
    let pre_bash = handlers
        .iter()
        .find(|h| h.name() == "pre_bash" && h.event() == "PreToolUse")?;
    if !native_hooks.contains(pre_bash.name()) {
        return None;
    }
    let mut answer = match pre_bash.run(payload) {
        Ok(Value::Null) | Err(_) => return None,
        Ok(value) => value,
    };

    if let Some(write_targets) = handlers
        .iter()
        .find(|h| h.name() == "bash_write_targets" && h.event() == "PreToolUse")
    {
        if native_hooks.contains(write_targets.name()) {
            if let Ok(extra) = write_targets.run(payload) {
                merge_additional_context(&mut answer, &extra);
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
        assert!(native_hook_names_from_env().is_empty());
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
    fn precheck_never_answers_an_allow_verdict() {
        let payload = json!({"tool_name": "Bash", "tool_input": {"command": "echo hi"}});
        let mut opted_in = HashSet::new();
        opted_in.insert("pre_bash".to_string());
        assert!(precheck_pretooluse_bash(&payload, &opted_in).is_none());
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
