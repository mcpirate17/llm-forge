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
//! **Row identity** (fixed 2026-09-13, was debt from PR #52): this command
//! has no access to the parent transcript, so it never learns the real
//! `tool_use_id` a later full `forge ledger rollup --repo` sweep would use
//! to key that same dispatch's row. It writes a synthetic `tool_use_id`
//! (`agent-<agent_id>`) into the row as a plain field, but keys the
//! `task_dispatch` day-file upsert by `agent_id` instead -- the one field
//! both this live path and the full sweep (`rollup.rs::build_task_dispatch`,
//! from the dispatch's `tool_result` `agentId:` line) agree on. A later full
//! sweep for the same dispatch therefore supersedes this live row instead of
//! sitting beside it as a second one; the two identities never diverge.

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

/// The routing verdict `main.rs` recomputes for a `subagent_type` alone
/// (no requested model, no description -- the same narrowed `AgentInput`
/// `resolve_agent_route`'s cap-only predecessor already used): everything
/// this row's `decision`/`applied` and `over_cap` need from `forge route`'s
/// embedded policy, without this module ever depending on the `route`
/// module directly (see `run`'s doc comment below).
#[derive(Debug, Clone)]
pub struct AgentRouteResolution {
    /// `route::Decision::cap_tokens` for this class -- unrelated to
    /// `decision` below; feeds only `over_cap` (unchanged fact, cap bullet).
    pub cap_tokens: u64,
    /// `route::Decision::decision` as `"allow"` or `"deny"` -- the routing
    /// bullet's "computed decision", recomputed retrospectively from
    /// `subagent_type` alone since the live `PreToolUse` call's own
    /// requested-model/description/justify context is never persisted for
    /// this upsert to join against. Documented approximation, same status
    /// as `resolve_agent_cap`'s own subagent_type-only cap resolution.
    pub decision: String,
    /// Whether the policy would assign this class a model at all
    /// (`route::Decision::model.is_some()`) -- an inherit-class allow (or
    /// any deny) never does. Together with `decision`, this is enough to
    /// derive `applied` without ever seeing `route::Verdict` here.
    pub would_assign_model: bool,
}

/// `resolve` is injected by the caller (`main.rs`) rather than called here
/// directly: `forge route`'s `Policy`/`AgentInput`/`Verdict` live in the
/// top-level `route` module, which this file cannot `use crate::route::..`
/// from -- `ledger/mod.rs` is compiled standalone via `#[path]` in the
/// `tests/ledger_*.rs` integration binaries, none of which have a `route`
/// module at their crate root. Injecting the resolution keeps this file
/// buildable in both contexts while still letting the real binary compute
/// it from the embedded routing policy, same as `dispatch.rs`/
/// `cap_enforce.rs` do for the live `PreToolUse` seam.
pub fn run(
    args: AgentUpsertArgs,
    resolve: impl Fn(Option<&str>) -> AgentRouteResolution,
) -> Result<i32> {
    let ledger_root = super::resolve_ledger_root(args.out);
    let summary = super::reader::read_transcript_file(&args.transcript)
        .with_context(|| format!("reading subagent transcript {}", args.transcript.display()))?;

    let resolution = resolve(args.subagent_type.as_deref());
    let row = build_row(
        &args.agent_id,
        args.subagent_type.as_deref(),
        &resolution,
        &summary,
    );
    let day = row
        .last_ts
        .as_deref()
        .or(row.first_ts.as_deref())
        .map(|ts| ts[..ts.len().min(10)].to_string())
        .unwrap_or_else(super::rollup::today_utc_date);

    let value = serde_json::to_value(&row)?;
    // Keyed by `agent_id`, the same identity `rollup.rs`'s full sweep now
    // uses for `task_dispatch` (PR #52 debt item 0a) -- not `tool_use_id`,
    // whose value here is only ever the synthetic `agent-<agent_id>`
    // placeholder (see the module doc above), never the real dispatch-side
    // id a later sweep would key by. Keying both paths by `agent_id` lets a
    // subsequent full sweep's row for this same dispatch supersede this
    // live row instead of becoming a second one.
    super::writer::write_day_file(&ledger_root, "task_dispatch", &day, "agent_id", &[value])
        .with_context(|| format!("upserting task_dispatch row for agent_id {}", args.agent_id))?;
    Ok(0)
}

/// The synthetic, agent_id-keyed row this command owns. `tool_use_id` here
/// is never a real `Agent` tool_use id -- see the module doc's "known debt".
fn build_row(
    agent_id: &str,
    subagent_type: Option<&str>,
    resolution: &AgentRouteResolution,
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
    let over_cap = billed > resolution.cap_tokens;
    let mode = resolve_mode();
    // `model_requested` is always `None` on this row (see field below), so
    // -- same equivalence `route::hook_outcome_for_agent`'s own
    // `already_had_model` check reduces to -- "would reroute" is exactly
    // "the policy has a model opinion for this class".
    let would_reroute = resolution.would_assign_model;
    // In enforce mode nothing this upsert can see was ever overridden: both
    // a real deny and a real reroute already happened for real. In warn
    // mode, a deny or a reroute would both have been advisory only --
    // never applied -- while a bare allow-with-no-change has nothing to
    // apply/not-apply, so it counts as applied.
    let applied = mode == "enforce" || (resolution.decision != "deny" && !would_reroute);
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
        // Unchanged fact (cap bullet): the live cap breach, independent of
        // routing's own `decision`/`applied` below.
        over_cap: Some(over_cap),
        first_ts,
        last_ts,
        landed: None,
        required_rework: None,
        ci_red_on_first_push: None,
        decision: Some(resolution.decision.clone()),
        mode: Some(mode.to_string()),
        applied: Some(applied),
    }
}

/// `FORGE_MODE`, duplicated from `route::resolve_mode` (this module cannot
/// `use crate::route` -- see the module doc comment above): `"warn"` opts
/// in, anything else (including unset or an unrecognized value) is
/// `"enforce"`. Unlike `route::resolve_mode`, this narrow upsert path does
/// not print the malformed-value warning -- `forge hook`'s own call into
/// `route::resolve_mode` already does, once per hook invocation, and this
/// command runs from the very same `SubagentStop` hook process.
fn resolve_mode() -> &'static str {
    match std::env::var("FORGE_MODE").ok().as_deref() {
        Some("warn") => "warn",
        _ => "enforce",
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
        run(args1, |_| allow_resolution(DEFAULT_CAP)).unwrap();

        let args2 = AgentUpsertArgs {
            transcript: transcript_path.clone(),
            agent_id: "abc123".to_string(),
            subagent_type: Some("general-purpose".to_string()),
            out: Some(ledger_root.clone()),
        };
        run(args2, |_| allow_resolution(DEFAULT_CAP)).unwrap();

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

    /// A neutral resolution: high cap (never over), routed as `"allow"`
    /// with no model opinion -- the injected closure's return value for
    /// tests that only care about a different field on the row.
    fn allow_resolution(cap_tokens: u64) -> AgentRouteResolution {
        AgentRouteResolution {
            cap_tokens,
            decision: "allow".to_string(),
            would_assign_model: false,
        }
    }

    fn row_from(
        ledger_root: &std::path::Path,
        transcript: std::path::PathBuf,
        agent_id: &str,
        resolution: AgentRouteResolution,
    ) -> Value {
        let args = AgentUpsertArgs {
            transcript,
            agent_id: agent_id.to_string(),
            subagent_type: Some("general-purpose".to_string()),
            out: Some(ledger_root.to_path_buf()),
        };
        run(args, move |_| resolution.clone()).unwrap();
        let day_file = ledger_root.join("task_dispatch").join("2026-09-13.jsonl");
        serde_json::from_str(std::fs::read_to_string(&day_file).unwrap().trim()).unwrap()
    }

    #[test]
    fn the_row_carries_decision_mode_and_applied() {
        // `resolve_mode` reads `FORGE_MODE` fresh, same as `route::
        // resolve_mode`; serialize against the same convention used
        // elsewhere in this crate (e.g. `cap_enforce::tests::ENV_LOCK`).
        static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());

        let scratch = ScratchDir::new("modeapplied");
        let transcript_path = scratch.path().join("agent-modefield.jsonl");
        write_fixture_transcript(&transcript_path);

        // A deny resolution, cap kept out of the way (high) so `over_cap`
        // stays false throughout -- proves `decision`/`applied` are their
        // own routing-only fact, independent of the cap bullet's `over_cap`.
        let deny = AgentRouteResolution {
            cap_tokens: 1_000_000,
            decision: "deny".to_string(),
            would_assign_model: false,
        };

        std::env::remove_var("FORGE_MODE");
        let row = row_from(
            &scratch.path().join("ledger-enforce"),
            transcript_path.clone(),
            "modefield",
            deny.clone(),
        );
        assert_eq!(row["decision"], "deny");
        assert_eq!(row["mode"], "enforce");
        assert_eq!(row["applied"], true, "enforce mode always applies: {row}");
        assert_eq!(row["over_cap"], false);

        std::env::set_var("FORGE_MODE", "warn");
        let row = row_from(
            &scratch.path().join("ledger-warn-deny"),
            transcript_path.clone(),
            "modefield",
            deny,
        );
        assert_eq!(row["decision"], "deny");
        assert_eq!(row["mode"], "warn");
        assert_eq!(
            row["applied"], false,
            "warn mode's would-be deny is advisory, never applied: {row}"
        );

        // An allow-with-reroute resolution in warn mode: also unapplied --
        // this is the fact `report.rs`'s `would_route` column reads.
        let reroute = AgentRouteResolution {
            cap_tokens: 1_000_000,
            decision: "allow".to_string(),
            would_assign_model: true,
        };
        let row = row_from(
            &scratch.path().join("ledger-warn-reroute"),
            transcript_path.clone(),
            "modefield",
            reroute,
        );
        assert_eq!(row["decision"], "allow");
        assert_eq!(
            row["applied"], false,
            "warn mode's would-be reroute is also unapplied: {row}"
        );

        // A plain allow-with-no-change resolution: nothing to override, so
        // it counts as applied even in warn mode.
        let row = row_from(
            &scratch.path().join("ledger-warn-noop"),
            transcript_path,
            "modefield",
            allow_resolution(1_000_000),
        );
        assert_eq!(row["decision"], "allow");
        assert_eq!(
            row["applied"], true,
            "an allow with nothing to change is applied in every mode: {row}"
        );

        std::env::remove_var("FORGE_MODE");
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
        run(args, |_| allow_resolution(1)).unwrap();

        let day_file = ledger_root.join("task_dispatch").join("2026-09-13.jsonl");
        let contents = std::fs::read_to_string(&day_file).unwrap();
        let parsed: Value = serde_json::from_str(contents.lines().next().unwrap()).unwrap();
        assert_eq!(parsed["over_cap"], true);
    }
}
