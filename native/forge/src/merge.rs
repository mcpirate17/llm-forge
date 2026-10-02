//! Shared native decision algebra; the Python binding uses the same core.

pub use conductor_native::hook_response::{merge, HookOutcome};
#[cfg(test)]
use serde_json::{json, Value};

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
