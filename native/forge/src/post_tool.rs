//! Native PostToolUse handlers, bounded output projections and deterministic
//! matcher order. Errors are reported without denying an already-completed tool.

use std::env;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::{json, Value};

use crate::context_telemetry;
use crate::crg_refresh;
use crate::handlers::{command_from_payload, find_handler, registry, NativeHandler};
use crate::instant;
use crate::merge::{self, HookOutcome};
use crate::read_budget;
use crate::tool_quiet::{self, QuietConfig};

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

/// The bare `{"hookSpecificOutput": {"hookEventName": "PostToolUse"}}` shape:
/// `crg_graph_refresh._post()` with no context -- what `post_bash_graph`
/// returns for a command that does not rewrite the git tree, and every
/// ported PostToolUse body returns when it has nothing to add.
fn quiet_post() -> Value {
    json!({"hookSpecificOutput": {"hookEventName": "PostToolUse"}})
}

/// `crg_refresh_report_post`: surfaces a background graph-refresh failure or
/// warning once, then clears the marker -- the PostToolUse twin of
/// `CrgRefreshReportPre` (`adapters.crg_refresh_report` passes `ctx.event`
/// through, so only the event name differs).
pub struct CrgRefreshReportPost;

impl NativeHandler for CrgRefreshReportPost {
    fn name(&self) -> &'static str {
        "crg_refresh_report_post"
    }

    fn event(&self) -> &'static str {
        "PostToolUse"
    }

    fn run(&self, _payload: &Value) -> Result<Value> {
        let root = crate::interpreter::project_root();
        Ok(crg_refresh::failure_output("PostToolUse", &root))
    }
}

/// `read_budget`: the per-session Read-token ledger and its advisory line
/// (`adapters.read_budget` = `read_budget.hook_output(payload,
/// crg_gate._state_dir())`, both halves ported in `crate::read_budget`).
pub struct ReadBudget;

impl NativeHandler for ReadBudget {
    fn name(&self) -> &'static str {
        "read_budget"
    }

    fn event(&self) -> &'static str {
        "PostToolUse"
    }

    fn run(&self, payload: &Value) -> Result<Value> {
        let state = read_budget::state_dir();
        read_budget::hook_output(payload, &state)
    }
}

/// `crg_graph_refresh`: PostToolUse on Edit/Write/NotebookEdit -- queue the
/// edited graph-suffix files for the detached refresh worker and never wait
/// (`adapters.crg_graph_refresh` = `crg_graph_refresh.hook_output(payload)`,
/// ported in `crate::crg_refresh::edit_hook_output` with the `crg_gate`
/// repo-root ladder).
pub struct CrgGraphRefresh;

impl NativeHandler for CrgGraphRefresh {
    fn name(&self) -> &'static str {
        "crg_graph_refresh"
    }

    fn event(&self) -> &'static str {
        "PostToolUse"
    }

    fn run(&self, payload: &Value) -> Result<Value> {
        if native_edit_is_preview_or_failure(payload) {
            return Ok(Value::Null);
        }
        let root = crg_refresh::gate_repo_root();
        Ok(crg_refresh::edit_hook_output(payload, &root)?)
    }
}

fn native_edit_is_preview_or_failure(payload: &Value) -> bool {
    let name = payload["tool_name"].as_str().unwrap_or("");
    name.starts_with("mcp__forge__")
        && crate::pre_edit::is_edit_tool_name(name)
        && (payload["tool_input"]["apply"] != true || payload["tool_response"]["isError"] == true)
}

/// `post_edit`: format the edited file, then the structural audit
/// (`adapters.post_edit` = `_post_edit_audit.hook_output(payload)`, ported
/// in `crate::post_edit_audit`). The formatter runs inside the hook exactly
/// as Python runs it (to completion, silently, bounded at 12 s).
pub struct PostEdit;

impl NativeHandler for PostEdit {
    fn name(&self) -> &'static str {
        "post_edit"
    }

    fn event(&self) -> &'static str {
        "PostToolUse"
    }

    fn run(&self, payload: &Value) -> Result<Value> {
        crate::post_edit_audit::hook_output(payload)
    }
}

/// `obsidian_post_edit`: mirror a memory edit into the vault and append the
/// session accumulator line (`adapters.obsidian_post_edit` =
/// `obsidian_sync.cmd_post_edit()`); SessionEnd stays in Python and reads
/// what this writes. The body prints its own quiet ok JSON, which is the
/// answer returned here.
pub struct ObsidianPostEdit;

impl NativeHandler for ObsidianPostEdit {
    fn name(&self) -> &'static str {
        "obsidian_post_edit"
    }

    fn event(&self) -> &'static str {
        "PostToolUse"
    }

    fn run(&self, payload: &Value) -> Result<Value> {
        Ok(crate::obsidian_sync::post_edit_output(payload))
    }
}

/// `post_bash_graph`: after a Bash command that rewrites the git working tree
/// (`adapters.GIT_TREE_REWRITE`), queue one whole-tree graph refresh and say
/// so; any other Bash command gets bare silence (`adapters.post_bash_graph`'s
/// `QUIET_POST`). A matching run also writes the hook-context telemetry side
/// record (`adapters._telemetry`), which never fails the hook -- the Python
/// adapter prints to stderr and moves on, and `record_hook_context` degrades
/// the same way on a dead sink.
pub struct PostBashGraph;

impl NativeHandler for PostBashGraph {
    fn name(&self) -> &'static str {
        "post_bash_graph"
    }

    fn event(&self) -> &'static str {
        "PostToolUse"
    }

    fn run(&self, payload: &Value) -> Result<Value> {
        let root = crate::interpreter::project_root();
        if !command_from_payload(payload).is_some_and(crg_refresh::git_tree_rewrite_matches) {
            return Ok(quiet_post());
        }
        let output = crg_refresh::full_update_output(&root)?;
        let session_id = payload
            .get("session_id")
            .and_then(Value::as_str)
            .unwrap_or("");
        context_telemetry::record_hook_context("post-bash-graph", &output, session_id);
        Ok(output)
    }
}

/// `context_telemetry`: one JSONL record per PostToolUse event on the shared
/// sink (`adapters.context_telemetry` = `telemetry.record(telemetry.event(...),
/// path)`). The hook's own answer is always silence (`None` in Python); a
/// broken sink degrades to stderr inside `record_event`, never an error.
pub struct ContextTelemetry;

impl NativeHandler for ContextTelemetry {
    fn name(&self) -> &'static str {
        "context_telemetry"
    }

    fn event(&self) -> &'static str {
        "PostToolUse"
    }

    fn run(&self, payload: &Value) -> Result<Value> {
        context_telemetry::record_event(payload);
        Ok(Value::Null)
    }
}

/// The exact hook names, in the Python registry's own order
/// (`src/tooling/hooks/dispatch/registry.py`), that a `PostToolUse` call
/// matches for the nine ported hooks: `crg_refresh_report_post` (matcher
/// `.*`), `crg_graph_refresh` (`Edit|Write|NotebookEdit`), `post_edit`
/// (`Edit|Write`), `read_budget` (`Read`), `obsidian_post_edit`
/// (`Edit|Write`), `post_bash_graph` (`Bash`), `post_bash_quiet` (`Bash`),
/// `post_tool_quiet` (`Read|Grep|mcp__.*`) and `context_telemetry` (`.*`) --
/// every `PostToolUse` name the registry has, so no tool name is refused by
/// `post_tool_use_fully_native` any more.
pub const POST_TOOL_USE_HOOK_NAMES: [&str; 9] = [
    "crg_refresh_report_post",
    "crg_graph_refresh",
    "post_edit",
    "read_budget",
    "obsidian_post_edit",
    "post_bash_graph",
    "post_bash_quiet",
    "post_tool_quiet",
    "context_telemetry",
];

/// Whether `name`'s own Python `HookSpec.matcher` would fire for `tool_name`,
/// for each name in `POST_TOOL_USE_HOOK_NAMES` -- `native_answers_for_post_
/// tool_use` must not compute (or claim to answer) a hook that Python's own
/// `select()` would never have run for this call.
fn post_tool_use_matches(name: &str, tool_name: &str) -> bool {
    match name {
        "crg_refresh_report_post" | "context_telemetry" => true, // matcher ".*"
        "read_budget" => tool_name == "Read",
        "post_bash_graph" | "post_bash_quiet" => tool_name == "Bash",
        "post_tool_quiet" => {
            tool_name == "Read" || tool_name == "Grep" || tool_name.starts_with("mcp__")
        }
        "crg_graph_refresh" => crate::pre_edit::is_edit_tool_name(tool_name),
        // Transactional native edits already validate syntax and return exact
        // resulting hashes. A subsequent formatter would invalidate that result.
        "post_edit" | "obsidian_post_edit" => matches!(tool_name, "Edit" | "Write"),
        _ => false,
    }
}

/// The final, fully-merged answer for a `PostToolUse` call once
/// `post_tool_use_fully_native` is true: every matcher-eligible handler in
/// registry order runs (side effects included -- the ledger tally, the graph
/// queue, the telemetry records land exactly where the Python adapter run
/// would have put them), and `crate::merge` folds the outcomes exactly as
/// Python's own `merge()` would. Every PostToolUse spec is fail-open in
/// Python's registry, so an `Err` becomes a non-fatal `HookOutcome` error --
/// reported, never blocking. Call only once the caller's `native_hooks`
/// satisfied `post_tool_use_fully_native` (the contract
/// `run_bash_pretooluse_fully_native` works under, mirrored); every
/// matcher-eligible name runs unconditionally here.
pub fn run_post_tooluse_fully_native(payload: &Value) -> Value {
    let tool_name = payload
        .get("tool_name")
        .and_then(Value::as_str)
        .unwrap_or("");
    let handlers = registry();
    let outcomes: Vec<HookOutcome> = POST_TOOL_USE_HOOK_NAMES
        .iter()
        .filter(|name| post_tool_use_matches(name, tool_name))
        .map(|name| {
            let computed = find_handler(&handlers, name, "PostToolUse")
                .expect("every POST_TOOL_USE_HOOK_NAMES entry is registered")
                .run(payload);
            match computed {
                Ok(output) => HookOutcome {
                    name: (*name).to_string(),
                    output,
                    error: None,
                    fail_closed: false,
                },
                Err(err) => HookOutcome {
                    name: (*name).to_string(),
                    output: Value::Null,
                    error: Some(format!("{err:#}")),
                    fail_closed: false,
                },
            }
        })
        .collect();
    merge::merge("PostToolUse", &outcomes)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    // These tests share process-global env vars (`FORGE_NATIVE_HOOKS`,
    // `CLAUDE_PROJECT_DIR`, `CRG_*`) with `handlers`'s own embedded tests --
    // both modules' tests live in the same `cargo test` binary -- so they take
    // `handlers::tests::ENV_LOCK` rather than declaring a second mutex: two
    // different mutexes guarding the same env var provide no mutual exclusion
    // at all (the same convention `crg_refresh`'s `pub(crate)` lock documents).
    use crate::handlers::tests::ENV_LOCK;

    #[test]
    fn post_tool_matchers_mirror_the_registry_matchers() {
        // `.*` names match every tool; the others fire only on their own
        // matcher's tools; anything else claims no match, so a native answer
        // is never promised for a hook Python's own `select()` would not
        // have run.
        for tool in ["Bash", "Read", "Grep", "mcp__x__y", "Glob", "Edit"] {
            assert!(post_tool_use_matches("crg_refresh_report_post", tool));
            assert!(post_tool_use_matches("context_telemetry", tool));
        }
        assert!(post_tool_use_matches("read_budget", "Read"));
        assert!(!post_tool_use_matches("read_budget", "Bash"));
        assert!(post_tool_use_matches("post_bash_graph", "Bash"));
        assert!(!post_tool_use_matches("post_bash_graph", "Read"));
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
        // The edit family: `crg_graph_refresh` also takes NotebookEdit;
        // `post_edit`/`obsidian_post_edit` do not.
        assert!(post_tool_use_matches("crg_graph_refresh", "Edit"));
        assert!(post_tool_use_matches("crg_graph_refresh", "NotebookEdit"));
        assert!(!post_tool_use_matches("crg_graph_refresh", "Bash"));
        assert!(post_tool_use_matches("post_edit", "Edit"));
        assert!(post_tool_use_matches("post_edit", "Write"));
        assert!(!post_tool_use_matches("post_edit", "NotebookEdit"));
        assert!(post_tool_use_matches("obsidian_post_edit", "Write"));
        assert!(!post_tool_use_matches("obsidian_post_edit", "Read"));
        for tool in [
            "mcp__forge__code_replace",
            "mcp__forge__code_diff",
            "mcp__forge__code_restore",
        ] {
            assert!(post_tool_use_matches("crg_graph_refresh", tool));
            assert!(!post_tool_use_matches("post_edit", tool));
            assert!(!post_tool_use_matches("obsidian_post_edit", tool));
        }
    }

    #[test]
    fn run_post_tooluse_fully_native_merges_in_registry_order() {
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // The store dir (`CRG_DATA_DIR`) and the read-budget ledger
        // (`CRG_GATE_STATE_DIR`) are vars other modules' tests mutate under
        // their own locks; take those locks too, like
        // `crg_refresh_report_pre_handler_is_silent_with_no_marker_file`.
        let _crg_guard = crate::crg_refresh::tests::ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _gate_guard = crate::crg_gate::tests::ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // read_budget's tests mutate READ_BUDGET_STEP_TOKENS and
        // context_telemetry's mutate CONTEXT_TELEMETRY_PATH, both under their
        // own pub(crate) locks -- this test depends on both vars' defaults.
        let _budget_guard = crate::read_budget::tests::ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _telemetry_guard = crate::context_telemetry::tests::ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = std::env::temp_dir().join(format!(
            "forge-handlers-post-fully-native-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let repo = dir.join("repo");
        let state = dir.join("gate-state");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&state).unwrap();
        // A failed-refresh notice (reported by crg_refresh_report_post, the
        // first matching name) plus a ledger seeded one step under the
        // crossing (reported by read_budget, the second): the merged
        // additionalContext must carry them in that registry order.
        let store = repo.join(".code-review-graph");
        std::fs::create_dir_all(&store).unwrap();
        std::fs::write(
            store.join("refresh.failed"),
            String::from(r#"{"kind":"failure","paths":["a.py"],"text":"boom"}"#) + "\n",
        )
        .unwrap();
        std::env::set_var("CLAUDE_PROJECT_DIR", &repo);
        std::env::set_var("CRG_DATA_DIR", &store);
        std::env::set_var("CRG_GATE_STATE_DIR", &state);
        std::env::set_var("CONTEXT_TELEMETRY_PATH", dir.join("events.jsonl"));
        let key = {
            use sha2::{Digest, Sha256};
            format!("{:x}", Sha256::digest("s-post".as_bytes()))
        };
        std::fs::write(state.join(format!("{key}.read-tokens")), "29900\n").unwrap();
        let payload = json!({
            "session_id": "s-post", "tool_name": "Read",
            "tool_input": {"file_path": "a.py"},
            "tool_response": {"type": "text", "text": "x".repeat(4000)},
        });
        let out = run_post_tooluse_fully_native(&payload);
        std::env::remove_var("CLAUDE_PROJECT_DIR");
        std::env::remove_var("CRG_DATA_DIR");
        std::env::remove_var("CRG_GATE_STATE_DIR");
        std::env::remove_var("CONTEXT_TELEMETRY_PATH");
        // The telemetry sink got the context_telemetry event record.
        let telemetry = std::fs::read_to_string(dir.join("events.jsonl")).unwrap_or_default();
        assert!(
            telemetry.contains("\"event\":\"PostToolUse\""),
            "{telemetry}"
        );
        let _ = std::fs::remove_dir_all(&dir);
        let context = out["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .expect("both advisory lines");
        let failure_at = context
            .find("WARNING: background graph refresh FAILED")
            .unwrap();
        let budget_at = context.find("READ BUDGET: 30,901 tokens").unwrap();
        assert!(failure_at < budget_at, "registry order: {context}");
        assert_eq!(
            out["hookSpecificOutput"]["hookEventName"],
            json!("PostToolUse")
        );
        // The small response stays under TOOL_OUTPUT_QUIET_BYTES: no rewrite.
        assert!(out["hookSpecificOutput"].get("updatedToolOutput").is_none());
    }
}
