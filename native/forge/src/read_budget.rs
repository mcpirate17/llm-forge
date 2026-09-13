//! Port of `tooling/hooks/agent/read_budget.py`: the PostToolUse read-token
//! budget. Every Read response's text is counted (~4 chars/token) into a
//! per-session ledger beside the graph-gate state; each time the running
//! total crosses another `READ_BUDGET_STEP_TOKENS` (default 30,000) the hook
//! adds one advisory line pointing at the cheaper graph tools. It never
//! blocks.
//!
//! State discipline mirrors Python exactly: the ledger is
//! `<sha256(session_id)>.read-tokens` in the gate's state directory
//! (`CRG_GATE_STATE_DIR`, default `/tmp/claude-crg-gate`, created 0o700),
//! an unreadable or corrupt previous value reads as 0, and the new total is
//! written back as `"<total>\n"`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

pub const STEP_ENV: &str = "READ_BUDGET_STEP_TOKENS";
pub const DEFAULT_STEP: u64 = 30_000;
const CHARS_PER_TOKEN: u64 = 4;
const MAX_COUNTED_CHARS: usize = 4_000_000;

pub const ADVICE: &str = "Prefer locate_tool / ast_context_tool / symbol_source_tool / query_graph, \
     Read with offset+limit, or delegate bulk reading to a subagent.";

/// `crg_gate._state_dir`: the gate's state directory, created 0o700.
pub fn state_dir() -> PathBuf {
    let configured = std::env::var("CRG_GATE_STATE_DIR").unwrap_or_default();
    let path = if configured.trim().is_empty() {
        PathBuf::from("/tmp/claude-crg-gate")
    } else {
        PathBuf::from(configured.trim())
    };
    let mut builder = std::fs::DirBuilder::new();
    builder.mode(0o700).recursive(true);
    let _ = builder.create(&path);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700));
    }
    path
}

/// `crg_gate._state_key`: the sha256 of the session id, else `None`.
fn state_key(payload: &Value) -> Option<String> {
    let pick = |name: &str| {
        let value = payload.get(name)?;
        match value {
            Value::String(text) if !text.is_empty() => Some(text.clone()),
            _ => None,
        }
    };
    let session_id = pick("session_id").or_else(|| pick("sessionId"))?;
    Some(format!("{:x}", Sha256::digest(session_id.as_bytes())))
}

/// `response_chars`: total characters of text in a tool response, walked
/// recursively with a bound (Python's `min(len, limit)` and early return).
pub fn response_chars(value: &Value, limit: usize) -> usize {
    if let Value::String(text) = value {
        return text.chars().count().min(limit);
    }
    let items: Vec<&Value> = match value {
        Value::Object(map) => map.values().collect(),
        Value::Array(items) => items.iter().collect(),
        _ => return 0,
    };
    let mut total = 0usize;
    for item in items {
        total += response_chars(item, limit - total);
        if total >= limit {
            return limit;
        }
    }
    total
}

/// `step_tokens`: the env override or the default; an unparseable value
/// fails loud (Python's `int(...)` `ValueError`), not silently.
fn step_tokens() -> Result<u64> {
    let raw = std::env::var(STEP_ENV).unwrap_or_default();
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(DEFAULT_STEP);
    }
    trimmed
        .parse()
        .with_context(|| format!("{STEP_ENV} is not an integer: {trimmed:?}"))
}

/// `tally`: add `tokens` to the session ledger, returning
/// `(previous_total, new_total)`.
fn tally(state_dir: &Path, key: &str, tokens: u64) -> std::io::Result<(u64, u64)> {
    let path = state_dir.join(format!("{key}.read-tokens"));
    let previous = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| text.trim().parse().ok())
        .unwrap_or(0);
    let new_total = previous + tokens;
    std::fs::write(&path, format!("{new_total}\n"))?;
    Ok((previous, new_total))
}

/// Python's `f"{value:,}"`: thousands-separated decimal.
fn thousands(value: u64) -> String {
    let digits = value.to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    let leading = digits.len() % 3;
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && index % 3 == leading {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
}

/// `read_budget.hook_output`: the PostToolUse answer with the advisory line
/// when this response crossed another step boundary.
pub fn hook_output(payload: &Value, state_dir: &Path) -> Result<Value> {
    if !payload.is_object() {
        return Ok(quiet_post());
    }
    let Some(key) = state_key(payload) else {
        return Ok(quiet_post());
    };
    let tokens = response_chars(payload.get("tool_response").unwrap_or(&Value::Null), MAX_COUNTED_CHARS) as u64
        / CHARS_PER_TOKEN;
    if tokens == 0 {
        return Ok(quiet_post());
    }
    let (previous, total) = tally(state_dir, &key, tokens)
        .with_context(|| format!("cannot update the read-token ledger in {}", state_dir.display()))?;
    let step = step_tokens()?;
    if total / step > previous / step {
        return Ok(json!({
            "hookSpecificOutput": {
                "hookEventName": "PostToolUse",
                "additionalContext": format!(
                    "READ BUDGET: {} tokens pulled into context via Read this \
                     session (crossed {}). {ADVICE}",
                    thousands(total),
                    thousands(step * (total / step)),
                ),
            }
        }));
    }
    Ok(quiet_post())
}

fn quiet_post() -> Value {
    json!({"hookSpecificOutput": {"hookEventName": "PostToolUse"}})
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use serde_json::json;

    pub(crate) static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn scratch(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "forge-read-budget-{}-{label}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn quiet_for_a_small_read_and_a_missing_session() {
        let dir = scratch("quiet");
        let small = json!({
            "session_id": "s-quiet", "tool_name": "Read",
            "tool_response": {"type": "text", "text": "just one line"},
        });
        assert_eq!(hook_output(&small, &dir).unwrap(), quiet_post());
        let no_session = json!({
            "tool_name": "Read", "tool_response": {"text": "x".repeat(100)},
        });
        assert_eq!(hook_output(&no_session, &dir).unwrap(), quiet_post());
        let not_a_dict = json!("garbage");
        assert_eq!(hook_output(&not_a_dict, &dir).unwrap(), quiet_post());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn crossing_a_step_emits_the_advisory_line_and_commas() {
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        std::env::remove_var(STEP_ENV);
        let dir = scratch("crossing");
        let key = format!("{:x}", Sha256::digest("s-cross".as_bytes()));
        // Seed 29,900 tokens: a 4,000-char response (1,000 tokens) crosses
        // the default 30,000 step.
        std::fs::write(dir.join(format!("{key}.read-tokens")), "29900\n").unwrap();
        let payload = json!({
            "session_id": "s-cross", "tool_name": "Read",
            "tool_response": {"type": "text", "text": "x".repeat(4000)},
        });
        let out = hook_output(&payload, &dir).unwrap();
        let line = out["hookSpecificOutput"]["additionalContext"].as_str().unwrap();
        assert!(line.starts_with("READ BUDGET: 30,900 tokens pulled into context via Read this session (crossed 30,000)."), "{line}");
        assert!(line.contains("query_graph"));
        // The ledger now holds the new total.
        assert_eq!(
            std::fs::read_to_string(dir.join(format!("{key}.read-tokens"))).unwrap(),
            "30900\n"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn no_new_line_when_no_boundary_is_crossed() {
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        std::env::set_var(STEP_ENV, "1000000");
        let dir = scratch("under");
        let payload = json!({
            "session_id": "s-under", "tool_name": "Read",
            "tool_response": {"type": "text", "text": "y".repeat(40)},
        });
        assert_eq!(hook_output(&payload, &dir).unwrap(), quiet_post());
        std::env::remove_var(STEP_ENV);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn response_chars_walks_and_bounds_like_python() {
        assert_eq!(response_chars(&json!("abc"), MAX_COUNTED_CHARS), 3);
        assert_eq!(response_chars(&json!(null), MAX_COUNTED_CHARS), 0);
        assert_eq!(response_chars(&json!(42), MAX_COUNTED_CHARS), 0);
        assert_eq!(response_chars(&json!([null, "ab", ["cd"]]), MAX_COUNTED_CHARS), 4);
        assert_eq!(response_chars(&json!({"a": "xyz", "b": 1}), MAX_COUNTED_CHARS), 3);
        // The bound caps at exactly the limit, never past it.
        let big = json!({"deep": ["a".repeat(50), "b".repeat(50)]});
        assert_eq!(response_chars(&big, 60), 60);
    }

    #[test]
    fn an_unparseable_step_fails_loud() {
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        std::env::set_var(STEP_ENV, "30k");
        let dir = scratch("badstep");
        let payload = json!({
            "session_id": "s-bad", "tool_name": "Read",
            "tool_response": {"text": "z".repeat(4000)},
        });
        let err = hook_output(&payload, &dir).unwrap_err().to_string();
        std::env::remove_var(STEP_ENV);
        assert!(err.contains("READ_BUDGET_STEP_TOKENS"), "{err}");
        std::fs::remove_dir_all(&dir).ok();
    }
}
