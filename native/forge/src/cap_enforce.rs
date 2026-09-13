//! Live cap enforcement (Phase 3 step 3, item 2, verdict B's derived-path
//! branch): a `PreToolUse` fired for a tool call happening *inside* a
//! running subagent carries the parent's `session_id`/`transcript_path`
//! plus a populated `agent_id` (`docs/routing.md` "Verdict B") -- this
//! module derives that subagent's own transcript path
//! (`subagent_transcript::derive`), reads only the bytes appended since the
//! last check (`<ledger_root>/live/<agent_id>.json` holds the byte offset
//! and running billed total), and denies the call outright once the
//! subagent's billed tokens exceed its class's cap.
//!
//! **The chosen branch, and why** (documented again in `docs/routing.md`):
//! `dispatch.rs::run_pre_tool_use` calls `check()` first, unconditionally,
//! for every `PreToolUse` call -- before the existing Bash/`Agent`
//! fully-native branches. `NoOp` (no `agent_id`, no derivable transcript,
//! within budget and under 80% of cap) falls through completely unchanged,
//! so the overwhelming majority of calls (anything outside a subagent, and
//! every call inside one that is nowhere near its cap) pay zero cost beyond
//! one field lookup. `Warn`/`Deny` fully short-circuit: they print their own
//! JSON verdict and skip both the native Bash/`Agent` paths and the Python
//! delegation entirely for that one call. This is an accepted, documented
//! tradeoff -- only calls already at or past 80% of a subagent's cap ever
//! lose the other `PreToolUse` hooks' say for that one call, and stopping a
//! runaway subagent's *next* tool call outweighs one skipped lint-style
//! hook on that same call.
//!
//! **Budget**: the whole check (state load, incremental read, route lookup,
//! state save) is bounded to 200 ms wall clock. On overrun the read-so-far
//! state is still persisted (the work already happened; only the verdict is
//! downgraded) and the call is allowed, with one stderr line -- never a
//! denial manufactured by a slow disk.

use std::fs::File;
use std::io::{Read as _, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::route::{self, AgentInput};

const BUDGET: Duration = Duration::from_millis(200);
const WARN_FRACTION: f64 = 0.8;

/// This check's verdict. `NoOp` is the only variant `dispatch.rs` lets fall
/// through to the rest of `run_pre_tool_use` unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CapCheck {
    NoOp,
    Warn(String),
    Deny(String),
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct LiveState {
    /// Byte offset into the subagent transcript already folded into
    /// `billed_total` -- never re-summed on the next call.
    offset: u64,
    /// Running `input + output + cache_creation` tokens across every turn
    /// read so far (cache reads excluded, same basis as `agent_rollup` and
    /// `task_dispatch`'s `billed_tokens`).
    billed_total: u64,
    /// Set once the 80%-of-cap warning has fired, so it is appended at most
    /// once per agent for the whole live check's lifetime.
    warned: bool,
}

/// Production entry point: `dispatch.rs` calls this first, unconditionally,
/// for every `PreToolUse` payload.
pub fn check(payload: &Value) -> CapCheck {
    let deadline = Instant::now() + BUDGET;
    let ledger_root = crate::ledger::resolve_ledger_root(None);
    check_with_deadline(payload, deadline, &ledger_root)
}

/// The full check, parameterized for tests: a caller-supplied `deadline`
/// (so a test can force an overrun with one already in the past) and
/// `ledger_root` (so tests never touch the real ledger).
pub(crate) fn check_with_deadline(
    payload: &Value,
    deadline: Instant,
    ledger_root: &Path,
) -> CapCheck {
    let Some(agent_id) = crate::subagent_transcript::agent_id(payload) else {
        return CapCheck::NoOp; // the fast path: zero further work.
    };
    let agent_id = agent_id.to_string();

    let Some(transcript) = crate::subagent_transcript::derive(payload) else {
        return CapCheck::NoOp;
    };
    if !transcript.is_file() {
        // The subagent has not written a transcript yet (or Verdict B's
        // derivation guessed a path that does not exist this call) --
        // nothing to enforce against.
        return CapCheck::NoOp;
    }

    if Instant::now() >= deadline {
        eprintln!("forge: cap_enforce exceeded its 200ms budget before starting; allowing");
        return CapCheck::NoOp;
    }

    let state_path = live_state_path(ledger_root, &agent_id);
    let mut state = load_state(&state_path);

    let overran = accumulate_billed_tokens(&transcript, &mut state, deadline);

    // Persist whatever was actually read, whether or not the deadline was
    // hit -- the read work already happened; only the verdict below may be
    // downgraded on overrun.
    if let Err(err) = save_state(&state_path, &state) {
        eprintln!(
            "forge: cap_enforce could not persist live state {}: {err:#}",
            state_path.display()
        );
    }

    if overran {
        eprintln!("forge: cap_enforce exceeded its 200ms budget mid-read; allowing");
        return CapCheck::NoOp;
    }
    if Instant::now() >= deadline {
        eprintln!("forge: cap_enforce exceeded its 200ms budget after reading; allowing");
        return CapCheck::NoOp;
    }

    let (class, cap_tokens) = resolve_class_and_cap(ledger_root, &agent_id, payload);
    verdict_for(&mut state, &state_path, &class, cap_tokens)
}

fn live_state_path(ledger_root: &Path, agent_id: &str) -> PathBuf {
    ledger_root.join("live").join(format!("{agent_id}.json"))
}

fn load_state(path: &Path) -> LiveState {
    let Ok(text) = std::fs::read_to_string(path) else {
        return LiveState::default();
    };
    serde_json::from_str(&text).unwrap_or_else(|err| {
        eprintln!(
            "forge: cap_enforce live state {} is corrupt ({err}); starting fresh",
            path.display()
        );
        LiveState::default()
    })
}

fn save_state(path: &Path, state: &LiveState) -> anyhow::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let text = serde_json::to_string(state)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Reads `transcript` from `state.offset` to EOF, summing each fully
/// present JSONL line's billed tokens into `state.billed_total` and
/// advancing `state.offset` past it. Returns `true` if `deadline` was hit
/// before the file was fully drained (the offset still only advances past
/// lines actually folded in, so no partial line and no counted-twice line
/// is possible on the next call).
fn accumulate_billed_tokens(transcript: &Path, state: &mut LiveState, deadline: Instant) -> bool {
    let mut file = match File::open(transcript) {
        Ok(f) => f,
        Err(err) => {
            eprintln!(
                "forge: cap_enforce could not open {}: {err}",
                transcript.display()
            );
            return false;
        }
    };
    if file.seek(SeekFrom::Start(state.offset)).is_err() {
        return false;
    }
    let mut buf = String::new();
    if file.read_to_string(&mut buf).is_err() {
        // A non-UTF8 tail (a line still being written) is not an error --
        // just stop here for this call, offset unchanged.
        return false;
    }

    let mut consumed: u64 = 0;
    for line in buf.split_inclusive('\n') {
        if Instant::now() >= deadline {
            return true;
        }
        if !line.ends_with('\n') {
            // The last line is still being written; do not count it or
            // advance the offset past it.
            break;
        }
        consumed += line.len() as u64;
        if let Some(billed) = billed_tokens_in_line(line) {
            state.billed_total += billed;
        }
    }
    state.offset += consumed;
    false
}

/// Sums `input_tokens + output_tokens + cache_creation_input_tokens` off one
/// transcript line's `message.usage`, the same billed-token basis
/// `rollup.rs`/`agent.rs` use everywhere else. `None` for a line with no
/// usage block (most tool_use/tool_result lines) -- nothing to add, not a
/// zero worth logging.
fn billed_tokens_in_line(line: &str) -> Option<u64> {
    let value: Value = serde_json::from_str(line.trim_end()).ok()?;
    let usage = value.get("message")?.get("usage")?;
    let field = |key: &str| usage.get(key).and_then(Value::as_u64).unwrap_or(0);
    Some(field("input_tokens") + field("output_tokens") + field("cache_creation_input_tokens"))
}

/// Class name + cap for this agent: the matching `task_dispatch` row's own
/// `subagent_type`/`description` when one exists on disk, else the
/// `PreToolUse` payload's own top-level `agent_type` field with no
/// description (the same field `SubagentStop`'s payload carries, per the
/// SubagentStop handler in item one). `forge route`'s classification
/// handles an unrecognized or absent `subagent_type` by falling back to
/// the policy's `default_class`, so this never fails to produce an answer.
fn resolve_class_and_cap(ledger_root: &Path, agent_id: &str, payload: &Value) -> (String, u64) {
    let (subagent_type, description) = find_task_dispatch_hint(ledger_root, agent_id)
        .unwrap_or_else(|| {
            (
                payload
                    .get("agent_type")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                None,
            )
        });
    let policy = match route::Policy::embedded() {
        Ok(policy) => policy,
        Err(err) => {
            eprintln!(
                "forge: cap_enforce could not parse the embedded routing policy ({err:#}); allowing"
            );
            return ("unknown".to_string(), u64::MAX);
        }
    };
    let input = AgentInput {
        subagent_type,
        requested_model: None,
        description,
    };
    let decision = route::route(&policy, &input);
    (decision.class, decision.cap_tokens)
}

/// Scans every `task_dispatch/*.jsonl` day file for a row whose `agent_id`
/// matches, returning its `(subagent_type, description)`. `None` when no
/// ledger exists yet or no row matches -- an ordinary condition (a subagent
/// dispatched but never yet rolled up), not an error.
fn find_task_dispatch_hint(
    ledger_root: &Path,
    agent_id: &str,
) -> Option<(Option<String>, Option<String>)> {
    let dir = ledger_root.join("task_dispatch");
    let entries = std::fs::read_dir(&dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        for line in text.lines() {
            let Ok(row) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            if row.get("agent_id").and_then(Value::as_str) == Some(agent_id) {
                let subagent_type = row
                    .get("subagent_type")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                let description = row
                    .get("description")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                return Some((subagent_type, description));
            }
        }
    }
    None
}

/// The actual over/under-cap decision, given the now-current `billed_total`.
/// Mutates and re-persists `state.warned` when the 80% warning fires for the
/// first time -- a second call past 80% (but still under cap) is then a
/// silent `NoOp`.
fn verdict_for(state: &mut LiveState, state_path: &Path, class: &str, cap_tokens: u64) -> CapCheck {
    if state.billed_total > cap_tokens {
        return CapCheck::Deny(format!(
            "over the {cap_tokens} token cap for class {class} ({} billed): stop, write your final report now; the parent will re-dispatch what is left",
            state.billed_total
        ));
    }
    let warn_threshold = (cap_tokens as f64 * WARN_FRACTION) as u64;
    if state.billed_total >= warn_threshold && !state.warned {
        state.warned = true;
        if let Err(err) = save_state(state_path, state) {
            eprintln!(
                "forge: cap_enforce could not persist the warned flag for {}: {err:#}",
                state_path.display()
            );
        }
        return CapCheck::Warn(format!(
            "at {:.0}% of the {cap_tokens} token cap for class {class} ({} billed): consider wrapping up soon",
            WARN_FRACTION * 100.0,
            state.billed_total
        ));
    }
    CapCheck::NoOp
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    struct ScratchDir(PathBuf);
    impl ScratchDir {
        fn new(tag: &str) -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!(
                "forge-cap-enforce-test-{tag}-{}-{n}",
                std::process::id()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            ScratchDir(dir)
        }
    }
    impl Drop for ScratchDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn far_future_deadline() -> Instant {
        Instant::now() + Duration::from_secs(60)
    }

    fn payload_for(agent_id: &str, transcript_dir: &Path) -> Value {
        // Verdict B shape: parent's transcript_path + session_id, plus
        // agent_id -- `subagent_transcript::derive` builds
        // `<dirname>/<session_id>/subagents/agent-<id>.jsonl` from these.
        serde_json::json!({
            "session_id": "sess-1",
            "transcript_path": transcript_dir.join("sess-1.jsonl").to_string_lossy(),
            "agent_id": agent_id,
            "agent_type": "general-purpose",
            "tool_name": "Bash",
        })
    }

    fn subagent_transcript_path(transcript_dir: &Path, agent_id: &str) -> PathBuf {
        let dir = transcript_dir.join("sess-1").join("subagents");
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(format!("agent-{agent_id}.jsonl"))
    }

    fn line_with_usage(input: u64, output: u64, cache_creation: u64) -> String {
        format!(
            r#"{{"type":"assistant","message":{{"model":"claude-sonnet-4","usage":{{"input_tokens":{input},"output_tokens":{output},"cache_read_input_tokens":0,"cache_creation_input_tokens":{cache_creation}}}}}}}"#
        ) + "\n"
    }

    #[test]
    fn a_payload_without_agent_id_is_the_fast_path_noop() {
        let scratch = ScratchDir::new("fastpath");
        let payload = serde_json::json!({"tool_name": "Bash"});
        let verdict = check_with_deadline(&payload, far_future_deadline(), &scratch.0);
        assert_eq!(verdict, CapCheck::NoOp);
    }

    #[test]
    fn a_missing_subagent_transcript_is_noop() {
        let scratch = ScratchDir::new("missingfile");
        let payload = payload_for("agent-x", &scratch.0);
        let verdict = check_with_deadline(&payload, far_future_deadline(), &scratch.0);
        assert_eq!(verdict, CapCheck::NoOp);
    }

    #[test]
    fn grown_across_three_calls_it_warns_then_denies() {
        let scratch = ScratchDir::new("threecalls");
        let ledger_root = scratch.0.join("ledger");
        let agent_id = "agentthree";
        let transcript_path = subagent_transcript_path(&scratch.0, agent_id);
        let payload = payload_for(agent_id, &scratch.0);

        // `general-purpose` has no configured cap override in the shipped
        // policy fixture used by these tests -- resolve whatever the
        // embedded policy actually gives it, so the test tracks the real
        // cap rather than hardcoding a number that could silently drift
        // out of sync with `ledger/routing_policy.toml`.
        let policy = route::Policy::embedded().unwrap();
        let decision = route::route(
            &policy,
            &AgentInput {
                subagent_type: Some("general-purpose".to_string()),
                requested_model: None,
                description: None,
            },
        );
        let cap = decision.cap_tokens;

        // Call 1: well under 80%.
        {
            let mut f = File::create(&transcript_path).unwrap();
            f.write_all(line_with_usage(cap / 10, 0, 0).as_bytes())
                .unwrap();
        }
        let v1 = check_with_deadline(&payload, far_future_deadline(), &ledger_root);
        assert_eq!(v1, CapCheck::NoOp, "call 1 should be far under cap");

        // Call 2: append enough to cross 80%.
        {
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&transcript_path)
                .unwrap();
            f.write_all(line_with_usage((cap as f64 * 0.75) as u64, 0, 0).as_bytes())
                .unwrap();
        }
        let v2 = check_with_deadline(&payload, far_future_deadline(), &ledger_root);
        assert!(
            matches!(v2, CapCheck::Warn(_)),
            "call 2 should cross the 80% warning threshold, got {v2:?}"
        );

        // Call 2 again with no new bytes: already warned, must not re-warn.
        let v2b = check_with_deadline(&payload, far_future_deadline(), &ledger_root);
        assert_eq!(
            v2b,
            CapCheck::NoOp,
            "the 80% warning fires at most once per agent"
        );

        // Call 3: push well past the cap.
        {
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&transcript_path)
                .unwrap();
            f.write_all(line_with_usage(cap, 0, 0).as_bytes()).unwrap();
        }
        let v3 = check_with_deadline(&payload, far_future_deadline(), &ledger_root);
        assert!(
            matches!(v3, CapCheck::Deny(_)),
            "call 3 should be denied for exceeding the cap, got {v3:?}"
        );
        if let CapCheck::Deny(reason) = v3 {
            assert!(reason.contains("token cap for class"));
            assert!(reason.contains("stop, write your final report now"));
        }
    }

    #[test]
    fn an_already_past_deadline_is_a_stderr_allow_not_a_panic() {
        let scratch = ScratchDir::new("overrun");
        let agent_id = "agentoverrun";
        let transcript_path = subagent_transcript_path(&scratch.0, agent_id);
        std::fs::write(&transcript_path, line_with_usage(1, 1, 0)).unwrap();
        let payload = payload_for(agent_id, &scratch.0);
        let past_deadline = Instant::now() - Duration::from_secs(1);
        let verdict = check_with_deadline(&payload, past_deadline, &scratch.0.join("ledger"));
        assert_eq!(verdict, CapCheck::NoOp);
    }

    #[test]
    fn a_second_call_never_recounts_bytes_already_folded_in() {
        let scratch = ScratchDir::new("norecount");
        let ledger_root = scratch.0.join("ledger");
        let agent_id = "agentnorecount";
        let transcript_path = subagent_transcript_path(&scratch.0, agent_id);
        let payload = payload_for(agent_id, &scratch.0);

        std::fs::write(&transcript_path, line_with_usage(100, 50, 0)).unwrap();
        check_with_deadline(&payload, far_future_deadline(), &ledger_root);
        let state_path = live_state_path(&ledger_root, agent_id);
        let state_after_1 = load_state(&state_path);
        assert_eq!(state_after_1.billed_total, 150);

        // Re-running with no new bytes must not double the total.
        check_with_deadline(&payload, far_future_deadline(), &ledger_root);
        let state_after_2 = load_state(&state_path);
        assert_eq!(state_after_2.billed_total, 150);
    }
}
