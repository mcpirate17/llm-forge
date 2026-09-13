//! Native port of `tooling.hooks.dispatch.merge._merge_reference`: the
//! decision algebra that folds every hook's JSON into one response for a
//! single Claude Code event. Used only once forge has full native coverage
//! for a call (`handlers::bash_pretooluse_fully_native`) and can produce the
//! final merged answer itself, without starting Python at all.
//!
//! `merge.py`'s own `merge()` actually calls into `conductor._native`'s
//! `hook_merge_native` (a `conductor-native` PyO3 extension) rather than
//! `_merge_reference` directly, but the module docstring states the two must
//! agree byte for byte -- `_merge_reference` is the readable reference
//! implementation and mutation-tested contract this port follows, so as not
//! to take `conductor-native` as a dependency of `forge` for one function.

use serde_json::{json, Map, Value};

const SEPARATOR: &str = "\n\n";

/// Events whose Claude Code schema defines `hookSpecificOutput`; everything
/// else folds to the top-level fields only (`merge.py`'s module docstring
/// is the contract -- Claude Code 2.1.268 rejects the field on SessionEnd).
const SPECIFIC_SCHEMA_EVENTS: [&str; 3] = ["PreToolUse", "PostToolUse", "SessionStart"];

/// One hook's contribution: the same shape `HookOutcome` carries in
/// `merge.py` (minus `elapsed_ms`, which never affects the merge).
pub struct HookOutcome {
    pub name: String,
    pub output: Value,
    pub error: Option<String>,
    pub fail_closed: bool,
}

fn rank(decision: &str) -> Option<u8> {
    match decision {
        "allow" => Some(1),
        "ask" => Some(2),
        "deny" => Some(3),
        _ => None,
    }
}

fn error_line(outcome: &HookOutcome) -> String {
    format!(
        "HOOK ERROR [{}]: {}",
        outcome.name,
        outcome.error.as_deref().unwrap_or("")
    )
}

fn specific(output: &Value) -> Map<String, Value> {
    output
        .get("hookSpecificOutput")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default()
}

/// Folds `outcomes` (already in the Python registry's own order for `event`)
/// into the one response Claude Code sees for the whole event.
pub fn merge(event: &str, outcomes: &[HookOutcome]) -> Value {
    let mut votes: Vec<(&str, String)> = Vec::new();
    let mut contexts: Vec<String> = Vec::new();
    let mut system: Vec<String> = Vec::new();
    let mut block_reasons: Vec<String> = Vec::new();
    let mut stop_reasons: Vec<String> = Vec::new();
    let mut rewrites: Map<String, Value> = Map::new();
    let mut extra_specific: Map<String, Value> = Map::new();
    let mut stop = false;
    let mut suppress = false;

    const REWRITE_KEYS: [&str; 2] = ["updatedToolOutput", "updatedMCPToolOutput"];
    const RESERVED_SPECIFIC_KEYS: [&str; 4] = [
        "hookEventName",
        "permissionDecision",
        "permissionDecisionReason",
        "additionalContext",
    ];

    for outcome in outcomes {
        if outcome.error.is_some() {
            let line = error_line(outcome);
            system.push(line.clone());
            if event == "PreToolUse" && outcome.fail_closed {
                votes.push(("deny", line));
            } else {
                contexts.push(line);
            }
        }
        let output = &outcome.output;
        if !output.is_object() {
            continue;
        }
        let spec = specific(output);
        if let Some(decision) = spec.get("permissionDecision").and_then(Value::as_str) {
            if rank(decision).is_some() {
                let reason = spec
                    .get("permissionDecisionReason")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                votes.push((decision_label(decision), reason));
            }
        }
        if let Some(context) = spec.get("additionalContext").and_then(Value::as_str) {
            if !context.is_empty() {
                contexts.push(context.to_string());
            }
        }
        for key in REWRITE_KEYS {
            if let Some(value) = spec.get(key) {
                if rewrites.contains_key(key) {
                    system.push(format!(
                        "HOOK CONFLICT [{}]: {key} already set by an earlier hook; \
                         the later value wins",
                        outcome.name
                    ));
                }
                rewrites.insert(key.to_string(), value.clone());
            }
        }
        for (key, value) in spec.iter() {
            let reserved = RESERVED_SPECIFIC_KEYS.contains(&key.as_str())
                || REWRITE_KEYS.contains(&key.as_str());
            if !reserved {
                extra_specific.insert(key.clone(), value.clone());
            }
        }
        let top_decision = output.get("decision").and_then(Value::as_str);
        if matches!(top_decision, Some("block") | Some("deny")) {
            let reason = output
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            if event == "PreToolUse" {
                votes.push(("deny", reason));
            } else {
                block_reasons.push(reason);
            }
        }
        if output.get("continue") == Some(&Value::Bool(false)) {
            stop = true;
            if let Some(reason) = output.get("stopReason").and_then(Value::as_str) {
                if !reason.is_empty() {
                    stop_reasons.push(reason.to_string());
                }
            }
        }
        if output.get("suppressOutput") == Some(&Value::Bool(true)) {
            suppress = true;
        }
        if let Some(message) = output.get("systemMessage").and_then(Value::as_str) {
            if !message.is_empty() {
                system.push(message.to_string());
            }
        }
    }

    let mut specific_out: Map<String, Value> = Map::new();
    specific_out.insert("hookEventName".to_string(), json!(event));
    if let Some((winner, _)) = votes
        .iter()
        .max_by_key(|(decision, _)| rank(decision).unwrap())
    {
        let winner = *winner;
        specific_out.insert("permissionDecision".to_string(), json!(winner));
        let reasons: Vec<&str> = votes
            .iter()
            .filter(|(decision, reason)| *decision == winner && !reason.is_empty())
            .map(|(_, reason)| reason.as_str())
            .collect();
        if !reasons.is_empty() {
            specific_out.insert(
                "permissionDecisionReason".to_string(),
                json!(reasons.join(SEPARATOR)),
            );
        }
    }
    if !contexts.is_empty() {
        specific_out.insert(
            "additionalContext".to_string(),
            json!(contexts.join(SEPARATOR)),
        );
    }
    for (key, value) in rewrites {
        specific_out.insert(key, value);
    }
    for (key, value) in extra_specific {
        specific_out.insert(key, value);
    }

    let mut result: Map<String, Value> = Map::new();
    if SPECIFIC_SCHEMA_EVENTS.contains(&event) {
        result.insert(
            "hookSpecificOutput".to_string(),
            Value::Object(specific_out),
        );
    }
    if !block_reasons.is_empty() {
        result.insert("decision".to_string(), json!("block"));
        result.insert(
            "reason".to_string(),
            json!(block_reasons
                .iter()
                .filter(|r| !r.is_empty())
                .cloned()
                .collect::<Vec<_>>()
                .join(SEPARATOR)),
        );
    }
    if stop {
        result.insert("continue".to_string(), json!(false));
        if !stop_reasons.is_empty() {
            result.insert(
                "stopReason".to_string(),
                json!(stop_reasons.join(SEPARATOR)),
            );
        }
    }
    if suppress {
        result.insert("suppressOutput".to_string(), json!(true));
    }
    if !system.is_empty() {
        result.insert("systemMessage".to_string(), json!(system.join(SEPARATOR)));
    }
    Value::Object(result)
}

/// Interns a validated decision string to a `&'static str` label -- `votes`
/// pass back one of the three literals it already recognizes, so this never
/// actually leaks memory in practice -- it exists only because
/// `spec.get(...).and_then(Value::as_str)` hands back a borrow tied to
/// `spec`, which does not outlive this function, while `"deny"` pushed
/// elsewhere in this function is `&'static str`. Interning through the fixed
/// set of valid decisions sidesteps the lifetime mismatch without cloning
/// every vote's decision string.
fn decision_label(decision: &str) -> &'static str {
    match decision {
        "allow" => "allow",
        "ask" => "ask",
        "deny" => "deny",
        _ => unreachable!("rank() already filtered to allow/ask/deny"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcome(name: &str, output: Value, fail_closed: bool) -> HookOutcome {
        HookOutcome {
            name: name.to_string(),
            output,
            error: None,
            fail_closed,
        }
    }

    #[test]
    fn deny_outranks_allow_and_joins_only_the_winning_reasons() {
        let outcomes = vec![
            outcome(
                "a",
                json!({"hookSpecificOutput": {"permissionDecision": "allow"}}),
                false,
            ),
            outcome(
                "b",
                json!({"hookSpecificOutput": {
                    "permissionDecision": "deny",
                    "permissionDecisionReason": "no",
                }}),
                false,
            ),
        ];
        let merged = merge("PreToolUse", &outcomes);
        assert_eq!(
            merged["hookSpecificOutput"]["permissionDecision"],
            json!("deny")
        );
        assert_eq!(
            merged["hookSpecificOutput"]["permissionDecisionReason"],
            json!("no")
        );
    }

    #[test]
    fn additional_context_concatenates_blank_line_separated_in_order() {
        let outcomes = vec![
            outcome(
                "a",
                json!({"hookSpecificOutput": {"additionalContext": "first"}}),
                false,
            ),
            outcome(
                "b",
                json!({"hookSpecificOutput": {"additionalContext": "second"}}),
                false,
            ),
        ];
        let merged = merge("PreToolUse", &outcomes);
        assert_eq!(
            merged["hookSpecificOutput"]["additionalContext"],
            json!("first\n\nsecond")
        );
    }

    #[test]
    fn a_top_level_grok_deny_becomes_a_permission_decision_vote() {
        let outcomes = vec![outcome(
            "a",
            json!({"decision": "deny", "reason": "nope"}),
            false,
        )];
        let merged = merge("PreToolUse", &outcomes);
        assert_eq!(
            merged["hookSpecificOutput"]["permissionDecision"],
            json!("deny")
        );
        assert_eq!(
            merged["hookSpecificOutput"]["permissionDecisionReason"],
            json!("nope")
        );
        assert!(merged.get("decision").is_none());
    }

    #[test]
    fn a_fail_closed_error_votes_deny_and_is_reported() {
        let outcomes = vec![HookOutcome {
            name: "pre_bash".to_string(),
            output: Value::Null,
            error: Some("boom".to_string()),
            fail_closed: true,
        }];
        let merged = merge("PreToolUse", &outcomes);
        assert_eq!(
            merged["hookSpecificOutput"]["permissionDecision"],
            json!("deny")
        );
        assert!(merged["systemMessage"]
            .as_str()
            .unwrap()
            .contains("HOOK ERROR [pre_bash]: boom"));
    }

    #[test]
    fn a_non_fail_closed_error_becomes_context_not_a_deny_vote() {
        let outcomes = vec![HookOutcome {
            name: "current_work_guard_bash".to_string(),
            output: Value::Null,
            error: Some("boom".to_string()),
            fail_closed: false,
        }];
        let merged = merge("PreToolUse", &outcomes);
        assert!(merged["hookSpecificOutput"]
            .get("permissionDecision")
            .is_none());
        assert!(merged["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap()
            .contains("HOOK ERROR"));
    }

    #[test]
    fn no_contribution_from_any_outcome_is_a_bare_hook_event_name() {
        let outcomes = vec![
            outcome("a", Value::Null, false),
            outcome("b", Value::Null, false),
        ];
        let merged = merge("PreToolUse", &outcomes);
        assert_eq!(
            merged,
            json!({"hookSpecificOutput": {"hookEventName": "PreToolUse"}})
        );
    }

    #[test]
    fn system_messages_from_every_outcome_are_collected_in_order() {
        let outcomes = vec![
            outcome("a", json!({"systemMessage": "one"}), false),
            outcome("b", json!({"systemMessage": "two"}), false),
        ];
        let merged = merge("PreToolUse", &outcomes);
        assert_eq!(merged["systemMessage"], json!("one\n\ntwo"));
    }

    #[test]
    fn later_rewrite_wins_and_flags_a_conflict() {
        let outcomes = vec![
            outcome(
                "a",
                json!({"hookSpecificOutput": {"updatedToolOutput": "a-out"}}),
                false,
            ),
            outcome(
                "b",
                json!({"hookSpecificOutput": {"updatedToolOutput": "b-out"}}),
                false,
            ),
        ];
        let merged = merge("PreToolUse", &outcomes);
        assert_eq!(
            merged["hookSpecificOutput"]["updatedToolOutput"],
            json!("b-out")
        );
        assert!(merged["systemMessage"]
            .as_str()
            .unwrap()
            .contains("HOOK CONFLICT [b]"));
    }

    /// Byte-for-byte twin of `test_session_end_folds_to_top_level_fields_only`
    /// in `conductor-native`'s `hook_merge.rs` and the `merge.py` tests: only
    /// PreToolUse/PostToolUse/SessionStart carry `hookSpecificOutput`; a
    /// quiet SessionEnd folds to `{}` (Claude Code 2.1.268 rejects the field
    /// there), and SubagentStop/Stop drop even a voting hook's wrapper.
    #[test]
    fn non_schema_events_fold_to_top_level_fields_only() {
        let quiet = merge("SessionEnd", &[outcome("telemetry", json!({}), false)]);
        assert_eq!(quiet, json!({}));

        let errored = HookOutcome {
            name: "ledger".to_string(),
            output: json!({}),
            error: Some("rollup failed".to_string()),
            fail_closed: false,
        };
        let merged = merge("SessionEnd", &[errored]);
        assert!(merged.get("hookSpecificOutput").is_none());
        assert!(merged["systemMessage"]
            .as_str()
            .unwrap()
            .contains("rollup failed"));

        for event in ["SubagentStop", "Stop"] {
            let voting = outcome(
                "gate",
                json!({"hookSpecificOutput": {"permissionDecision": "deny"}}),
                false,
            );
            assert_eq!(
                merge(event, &[voting]),
                json!({}),
                "{event} has no schema for the wrapper or the decision"
            );
        }

        for event in ["PreToolUse", "PostToolUse", "SessionStart"] {
            let merged = merge(event, &[outcome("quiet", json!({}), false)]);
            assert_eq!(
                merged["hookSpecificOutput"]["hookEventName"],
                json!(event),
                "schema events keep the wrapper exactly as before"
            );
        }
    }
}
