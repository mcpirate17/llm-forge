//! Native `SubagentStop` (Phase 3 step 3, item 1): the moment a dispatched
//! subagent's own transcript is complete and will never change again -- the
//! ideal moment to finalize its `task_dispatch` row (`billed_tokens`,
//! `tier`, `over_cap`) rather than waiting for the next full `forge ledger
//! rollup --repo` sweep. Mirrors `session_end.rs`'s shape exactly: a
//! best-effort, 2 s-bounded child re-invocation of this same binary, then
//! the event still delegates to the Python dispatcher unchanged --
//! `SubagentStop` owns no verdict of its own.
//!
//! `FORGE_LEDGER_DISABLE=1` skips the rollup entirely, same escape hatch as
//! `SessionEnd`. On success this also deletes item 2's live cap-enforcement
//! state file (`<ledger_root>/live/<agent_id>.json`) -- the agent is done,
//! so there is nothing left to enforce a cap against.

use std::path::Path;
use std::time::Duration;

use serde_json::Value;

use crate::bounded_child::run_bounded;
use crate::ledger::resolve_ledger_root;

/// Wall-clock bound for the rollup child, per the brief: 2 s over the one
/// subagent transcript path, never the whole session tree `SessionEnd`
/// walks.
const ROLLUP_TIMEOUT: Duration = Duration::from_secs(2);

/// Runs the `SubagentStop` rollup for `payload`, best-effort: every error
/// (missing identity, timeout, nonzero child) is one stderr line, never a
/// panic or a propagated error -- a `SubagentStop` hook must never fail
/// because the ledger could not be written.
pub fn rollup_ending_agent(payload: &str) {
    if std::env::var("FORGE_LEDGER_DISABLE").ok().as_deref() == Some("1") {
        eprintln!("forge: ledger rollup disabled (FORGE_LEDGER_DISABLE=1)");
        return;
    }
    let parsed: Value = match serde_json::from_str(payload) {
        Ok(value) => value,
        Err(err) => {
            eprintln!("forge: SubagentStop payload is not JSON ({err}); no ledger rollup");
            return;
        }
    };
    let Some(agent_id) = crate::subagent_transcript::agent_id(&parsed) else {
        eprintln!("forge: SubagentStop payload carries no agent_id; no ledger rollup");
        return;
    };
    let Some(transcript) = crate::subagent_transcript::derive(&parsed) else {
        eprintln!(
            "forge: SubagentStop payload for agent_id {agent_id} names no derivable transcript path; no ledger rollup"
        );
        return;
    };
    if !transcript.is_file() {
        eprintln!(
            "forge: SubagentStop transcript {} does not exist; no ledger rollup",
            transcript.display()
        );
        return;
    }
    let agent_type = parsed.get("agent_type").and_then(Value::as_str);
    if let Err(err) = run_rollup_agent_child(&transcript, agent_id, agent_type) {
        eprintln!("forge: subagent-stop ledger rollup failed: {err:#}");
    }
    delete_live_state(agent_id);
}

/// `agent_type` (the SubagentStop payload's third documented field,
/// alongside `agent_id`/`agent_transcript_path`) is forwarded as
/// `--subagent-type` so `rollup-agent` can resolve a cap via `forge route`
/// (item 1's "cap_tokens comes from forge route's decision for that row's
/// subagent_type/description") even though the subagent's own transcript
/// never carries its own dispatch metadata -- only the parent's dispatch
/// tool_use did, and this hook does not have that transcript open.
fn run_rollup_agent_child(
    transcript: &Path,
    agent_id: &str,
    agent_type: Option<&str>,
) -> anyhow::Result<()> {
    let mut args = vec![
        "ledger".to_string(),
        "rollup-agent".to_string(),
        transcript.display().to_string(),
        "--agent-id".to_string(),
        agent_id.to_string(),
    ];
    if let Some(agent_type) = agent_type {
        args.push("--subagent-type".to_string());
        args.push(agent_type.to_string());
    }
    run_bounded(&args, ROLLUP_TIMEOUT, "forge ledger rollup-agent")
}

/// Best-effort delete of item 2's per-agent live-cap state file: the agent
/// has stopped, so there is nothing left for the next `PreToolUse` to check
/// against. A missing file is not an error (the agent may never have made a
/// tool call that triggered cap enforcement); any other failure is one
/// stderr line.
fn delete_live_state(agent_id: &str) {
    let path = resolve_ledger_root(None)
        .join("live")
        .join(format!("{agent_id}.json"));
    match std::fs::remove_file(&path) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => eprintln!(
            "forge: could not remove live cap state {}: {err}",
            path.display()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_payload_without_agent_id_is_a_stderr_note_not_a_failure() {
        rollup_ending_agent("not json at all");
        rollup_ending_agent(r#"{"transcript_path":"/tmp/x.jsonl"}"#);
    }

    #[test]
    fn disable_env_skips_the_rollup_entirely() {
        std::env::set_var("FORGE_LEDGER_DISABLE", "1");
        rollup_ending_agent(
            r#"{"agent_id":"deadbeef","session_id":"s1","transcript_path":"/nonexistent/x.jsonl"}"#,
        );
        std::env::remove_var("FORGE_LEDGER_DISABLE");
    }

    #[test]
    fn a_nonexistent_derived_transcript_is_a_stderr_note_not_a_failure() {
        rollup_ending_agent(
            r#"{"agent_id":"deadbeef","session_id":"s1","transcript_path":"/nonexistent/dir/x.jsonl"}"#,
        );
    }

    // The end-to-end path (a real `forge ledger rollup-agent` child over a
    // real synthetic subagent transcript through `forge hook SubagentStop`)
    // needs the compiled `forge` binary (`CARGO_BIN_EXE_forge`), which only
    // integration tests get -- lives in `tests/hook_delegation.rs` beside
    // `session_end_rolls_the_ending_session_into_the_ledger`, same reason
    // `session_end.rs` documents for its own child-process path.
}
