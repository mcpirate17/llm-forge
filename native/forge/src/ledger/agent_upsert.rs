//! `forge ledger rollup-agent` (Phase 3 step 3, item 1): the narrow
//! `SubagentStop`-triggered upsert of one `task_dispatch` row, from just
//! that one subagent's own transcript -- never the full `forge ledger
//! rollup` pipeline, which would key its dispatch-side row off the
//! *parent's* `tool_use_id` and would re-derive `session_rollup`/
//! `turn_attribution` rows keyed by the parent's own `session_id` (every
//! line of a subagent file carries the PARENT's `sessionId`, per
//! `rollup.rs::SessionRollupRow`'s doc comment) -- running the general
//! pipeline on a lone subagent file would collide with, not complement,
//! the parent's own eventual rollup.
//!
//! **Known debt** (documented here and in `docs/ledger.md`): this command
//! has no access to the parent transcript, so it never learns the real
//! `tool_use_id` a later full `forge ledger rollup --repo` sweep will use
//! to key that same dispatch's row. It upserts under a synthetic key
//! `agent-<agent_id>` instead -- stable and idempotent across repeated
//! `SubagentStop` calls for the same agent (the brief's explicit
//! requirement), but a *separate* row from the one a subsequent full sweep
//! writes for the same dispatch until something reconciles the two keys.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Args;

use super::agent::infer_tier;
use super::rollup::TaskDispatchRow;
use std::collections::BTreeSet;

#[derive(Args)]
pub struct AgentUpsertArgs {
    /// The one subagent transcript this call finalizes (`agent-<id>.jsonl`).
    pub transcript: PathBuf,

    /// The `SubagentStop` payload's `agent_id`; also the row's synthetic
    /// storage key (`agent-<agent-id>`).
    #[arg(long = "agent-id")]
    pub agent_id: String,

    /// The `SubagentStop` payload's `agent_type`, when present, so
    /// `over_cap` can be resolved via `forge route` (its `subagent_type`
    /// classification) instead of staying unresolved. Absent it, the row's
    /// class falls back to the policy's own `default_class`, same as any
    /// other dispatch `forge route` cannot classify.
    #[arg(long = "subagent-type")]
    pub subagent_type: Option<String>,

    /// Ledger root; defaults to `LEDGER_ROOT`, else the built-in default,
    /// same resolution as every other ledger subcommand.
    #[arg(long)]
    pub out: Option<PathBuf>,
}

/// `resolve_cap` is injected by the caller (`main.rs`) rather than called
/// here directly: `forge route`'s `Policy`/`AgentInput` live in the
/// top-level `route` module, which this file cannot `use crate::route::..`
/// from -- `ledger/mod.rs` is compiled standalone via `#[path]` in the
/// `tests/ledger_*.rs` integration binaries, none of which have a `route`
/// module at their crate root. Injecting the resolved cap keeps this file
/// buildable in both contexts while still letting the real binary compute
/// the cap from the embedded routing policy, same as `dispatch.rs`/
/// `cap_enforce.rs` do for the live `PreToolUse` seam.
pub fn run(args: AgentUpsertArgs, resolve_cap: impl Fn(Option<&str>) -> u64) -> Result<i32> {
    let ledger_root = super::resolve_ledger_root(args.out);
    let summary = super::reader::read_transcript_file(&args.transcript)
        .with_context(|| format!("reading subagent transcript {}", args.transcript.display()))?;

    let cap_tokens = resolve_cap(args.subagent_type.as_deref());
    let row = build_row(
        &args.agent_id,
        args.subagent_type.as_deref(),
        cap_tokens,
        &summary,
    );
    let day = row
        .last_ts
        .as_deref()
        .or(row.first_ts.as_deref())
        .map(|ts| ts[..ts.len().min(10)].to_string())
        .unwrap_or_else(super::rollup::today_utc_date);

    let value = serde_json::to_value(&row)?;
    super::writer::write_day_file(&ledger_root, "task_dispatch", &day, "tool_use_id", &[value])
        .with_context(|| format!("upserting task_dispatch row for agent_id {}", args.agent_id))?;
    Ok(0)
}

/// The synthetic, agent_id-keyed row this command owns. `tool_use_id` here
/// is never a real `Agent` tool_use id -- see the module doc's "known debt".
fn build_row(
    agent_id: &str,
    subagent_type: Option<&str>,
    cap_tokens: u64,
    summary: &super::schema::TranscriptSummary,
) -> TaskDispatchRow {
    let models: BTreeSet<&str> = summary
        .turns
        .iter()
        .filter_map(|turn| turn.model.as_deref())
        .collect();
    let tier = infer_tier(&models);
    let billed = summary.total_input_tokens
        + summary.total_output_tokens
        + summary.total_cache_creation_input_tokens;
    let (first_ts, last_ts) = ts_range(summary);

    TaskDispatchRow {
        parent_session_id: summary.parent_session_id.clone(),
        dispatch_ts: None,
        tool_use_id: format!("agent-{agent_id}"),
        agent_id: Some(agent_id.to_string()),
        subagent_type: subagent_type.map(str::to_string),
        description: None,
        model_requested: None,
        model_used: Some(models.iter().map(|m| (*m).to_string()).collect()),
        tier: Some(tier),
        n_turns: Some(summary.turns_with_usage),
        billed_tokens: Some(billed),
        total_cache_read: Some(summary.total_cache_read_input_tokens),
        over_cap: Some(billed > cap_tokens),
        first_ts,
        last_ts,
        landed: None,
        required_rework: None,
        ci_red_on_first_push: None,
    }
}

fn ts_range(summary: &super::schema::TranscriptSummary) -> (Option<String>, Option<String>) {
    let mut first: Option<&str> = None;
    let mut last: Option<&str> = None;
    for turn in &summary.turns {
        if let Some(ts) = turn.timestamp.as_deref() {
            if first.is_none_or(|f| ts < f) {
                first = Some(ts);
            }
            if last.is_none_or(|l| ts > l) {
                last = Some(ts);
            }
        }
    }
    (first.map(str::to_string), last.map(str::to_string))
}

#[cfg(test)]
mod tests {
    use super::super::agent::DEFAULT_CAP;
    use super::*;
    use serde_json::Value;
    use std::io::Write;

    struct ScratchDir(PathBuf);
    impl ScratchDir {
        fn new(tag: &str) -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!(
                "forge-agent-upsert-test-{tag}-{}-{n}",
                std::process::id()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            ScratchDir(dir)
        }
        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }
    impl Drop for ScratchDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn write_fixture_transcript(path: &std::path::Path) {
        let mut f = std::fs::File::create(path).unwrap();
        let lines = [
            r#"{"type":"assistant","sessionId":"parent-1","uuid":"t1","timestamp":"2026-09-13T10:00:00Z","message":{"role":"assistant","model":"claude-haiku-4","usage":{"input_tokens":100,"output_tokens":50,"cache_read_input_tokens":0,"cache_creation_input_tokens":0},"content":[{"type":"text","text":"hi"}]}}"#,
            r#"{"type":"assistant","sessionId":"parent-1","uuid":"t2","timestamp":"2026-09-13T10:01:00Z","message":{"role":"assistant","model":"claude-haiku-4","usage":{"input_tokens":200,"output_tokens":75,"cache_read_input_tokens":10,"cache_creation_input_tokens":5},"content":[{"type":"text","text":"bye"}]}}"#,
        ];
        for line in lines {
            writeln!(f, "{line}").unwrap();
        }
    }

    #[test]
    fn upserting_twice_for_the_same_agent_id_rewrites_one_row_not_two() {
        let scratch = ScratchDir::new("basic");
        let transcript_path = scratch.path().join("agent-abc123.jsonl");
        write_fixture_transcript(&transcript_path);
        let ledger_root = scratch.path().join("ledger");

        let args1 = AgentUpsertArgs {
            transcript: transcript_path.clone(),
            agent_id: "abc123".to_string(),
            subagent_type: Some("general-purpose".to_string()),
            out: Some(ledger_root.clone()),
        };
        run(args1, |_| DEFAULT_CAP).unwrap();

        let args2 = AgentUpsertArgs {
            transcript: transcript_path.clone(),
            agent_id: "abc123".to_string(),
            subagent_type: Some("general-purpose".to_string()),
            out: Some(ledger_root.clone()),
        };
        run(args2, |_| DEFAULT_CAP).unwrap();

        let day_file = ledger_root.join("task_dispatch").join("2026-09-13.jsonl");
        let contents = std::fs::read_to_string(&day_file).unwrap();
        let rows: Vec<&str> = contents.lines().filter(|l| !l.trim().is_empty()).collect();
        assert_eq!(
            rows.len(),
            1,
            "a second rollup-agent call for the same agent_id must rewrite, not duplicate, the row"
        );
        let parsed: Value = serde_json::from_str(rows[0]).unwrap();
        assert_eq!(parsed["agent_id"], "abc123");
        assert_eq!(parsed["billed_tokens"], 430); // 100+50+200+75+5, cache_read excluded
    }

    #[test]
    fn the_injected_cap_resolver_is_used_for_over_cap() {
        let scratch = ScratchDir::new("cap");
        let transcript_path = scratch.path().join("agent-xyz.jsonl");
        write_fixture_transcript(&transcript_path);
        let ledger_root = scratch.path().join("ledger");

        // billed_tokens for the fixture is 430 (100+50+200+75+5); a cap of
        // 1 forces over_cap = true, proving the resolver's return value
        // reaches the stored row rather than some hardcoded default.
        let args = AgentUpsertArgs {
            transcript: transcript_path,
            agent_id: "xyz".to_string(),
            subagent_type: Some("some-type-the-policy-has-never-heard-of".to_string()),
            out: Some(ledger_root.clone()),
        };
        run(args, |_| 1).unwrap();

        let day_file = ledger_root.join("task_dispatch").join("2026-09-13.jsonl");
        let contents = std::fs::read_to_string(&day_file).unwrap();
        let parsed: Value = serde_json::from_str(contents.lines().next().unwrap()).unwrap();
        assert_eq!(parsed["over_cap"], true);
    }
}
