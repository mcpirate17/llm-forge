//! Derives a subagent's own transcript path from a hook payload that only
//! carries the *parent's* `session_id`/`transcript_path` plus a populated
//! `agent_id` (`docs/routing.md` "Verdict B", probed 2026-09-13 in PR #50):
//! a `PreToolUse` fired for a tool call made inside a running subagent never
//! carries a distinct subagent `transcript_path` -- the caller has to derive
//! `<dirname(transcript_path)>/<session_id>/subagents/agent-<agent_id>.jsonl`
//! itself. Shared by `cap_enforce.rs` (item 2) and `subagent_stop.rs`
//! (item 1, whose `SubagentStop` payload may carry an explicit
//! `agent_transcript_path` the derivation should prefer when present).

use std::path::{Path, PathBuf};

use serde_json::Value;

/// Prefers an explicit `agent_transcript_path` field (documented for
/// `SubagentStop` payloads) over the derived path, so a harness version that
/// starts sending one directly needs no code change here to be used. Falls
/// back to the verified derivation from `session_id`/`transcript_path` plus
/// `agent_id`. `None` when the payload carries no non-empty `agent_id` at
/// all (nothing to derive) or is missing the fields the derivation needs.
pub fn derive(payload: &Value) -> Option<PathBuf> {
    if let Some(explicit) = payload
        .get("agent_transcript_path")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        return Some(PathBuf::from(explicit));
    }
    let agent_id = payload.get("agent_id").and_then(Value::as_str)?;
    if agent_id.is_empty() {
        return None;
    }
    let transcript_path = payload.get("transcript_path").and_then(Value::as_str)?;
    let session_id = payload.get("session_id").and_then(Value::as_str)?;
    let dir = Path::new(transcript_path).parent()?;
    Some(
        dir.join(session_id)
            .join("subagents")
            .join(format!("agent-{agent_id}.jsonl")),
    )
}

/// The non-empty `agent_id` a payload names, or `None` -- the fast-path
/// check every caller runs before doing any file I/O at all (item 2's
/// "payload without `agent_id` adds zero work").
pub fn agent_id(payload: &Value) -> Option<&str> {
    payload
        .get("agent_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn no_agent_id_derives_nothing() {
        let payload = json!({"tool_name": "Bash", "session_id": "s1"});
        assert!(agent_id(&payload).is_none());
        assert!(derive(&payload).is_none());
    }

    #[test]
    fn empty_agent_id_is_treated_as_absent() {
        let payload = json!({"agent_id": ""});
        assert!(agent_id(&payload).is_none());
        assert!(derive(&payload).is_none());
    }

    #[test]
    fn derives_the_verdict_b_path_from_parent_identity() {
        let payload = json!({
            "agent_id": "a4922348ac368160a",
            "session_id": "6f59a2ad-4405-44c8-9b66-5849152ec629",
            "transcript_path": "/home/tim/.claude/projects/-mnt-data-llm-scratch-forge-x/6f59a2ad-4405-44c8-9b66-5849152ec629.jsonl",
        });
        let path = derive(&payload).unwrap();
        assert_eq!(
            path,
            PathBuf::from(
                "/home/tim/.claude/projects/-mnt-data-llm-scratch-forge-x/6f59a2ad-4405-44c8-9b66-5849152ec629/subagents/agent-a4922348ac368160a.jsonl"
            )
        );
    }

    #[test]
    fn an_explicit_agent_transcript_path_wins_over_derivation() {
        let payload = json!({
            "agent_id": "abc",
            "session_id": "s1",
            "transcript_path": "/x/s1.jsonl",
            "agent_transcript_path": "/explicit/path.jsonl",
        });
        assert_eq!(
            derive(&payload).unwrap(),
            PathBuf::from("/explicit/path.jsonl")
        );
    }
}
