#![cfg(feature = "python-compat-tests")]
//! Dispatcher merge contracts. Fixtures and assertions are owned by Rust.

#[path = "python_contracts/agent_comm_support.rs"]
#[allow(dead_code)]
mod comm_support;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use comm_support::py_json;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyBool, PyList};
use serde_json::{json, Value};
use support::{module, Case};

fn production(py: Python<'_>) -> Bound<'_, pyo3::types::PyModule> {
    module(py, "tooling.hooks.dispatch.merge")
}

fn pre(decision: Option<&str>, reason: &str, context: &str) -> Value {
    let mut specific = json!({"hookEventName": "PreToolUse"});
    if let Some(decision) = decision {
        specific["permissionDecision"] = json!(decision);
    }
    if !reason.is_empty() {
        specific["permissionDecisionReason"] = json!(reason);
    }
    if !context.is_empty() {
        specific["additionalContext"] = json!(context);
    }
    json!({"hookSpecificOutput": specific})
}

fn outcome<'py>(py: Python<'py>, name: &str, output: Option<Value>) -> Bound<'py, PyAny> {
    let output = output.map_or_else(|| py.None().into_bound(py), |v| py_json(py, v));
    production(py)
        .getattr("HookOutcome")
        .unwrap()
        .call1((name, output))
        .unwrap()
}

fn failed<'py>(py: Python<'py>, name: &str, error: &str, fail_closed: bool) -> Bound<'py, PyAny> {
    let kwargs = pyo3::types::PyDict::new(py);
    kwargs.set_item("error", error).unwrap();
    if fail_closed {
        kwargs.set_item("fail_closed", true).unwrap();
    }
    production(py)
        .getattr("HookOutcome")
        .unwrap()
        .call((name, py.None()), Some(&kwargs))
        .unwrap()
}

fn merged<'py>(py: Python<'py>, event: &str, outcomes: &[Bound<'py, PyAny>]) -> Bound<'py, PyAny> {
    production(py)
        .getattr("merge")
        .unwrap()
        .call1((event, PyList::new(py, outcomes).unwrap()))
        .unwrap()
}

fn specific<'py>(result: &Bound<'py, PyAny>) -> Bound<'py, PyAny> {
    result.get_item("hookSpecificOutput").unwrap()
}

fn separator(py: Python<'_>) -> String {
    production(py)
        .getattr("SEPARATOR")
        .unwrap()
        .extract()
        .unwrap()
}

fn error_line(py: Python<'_>, failed: &Bound<'_, PyAny>) -> String {
    production(py)
        .getattr("error_line")
        .unwrap()
        .call1((failed,))
        .unwrap()
        .extract()
        .unwrap()
}

#[test]
fn any_deny_wins_over_allow_and_ask() {
    let _case = Case::new();
    Python::attach(|py| {
        let result = merged(
            py,
            "PreToolUse",
            &[
                outcome(py, "a", Some(pre(Some("allow"), "", ""))),
                outcome(py, "b", Some(pre(Some("deny"), "no", ""))),
                outcome(py, "c", Some(pre(Some("ask"), "maybe", ""))),
                outcome(py, "d", Some(pre(Some("allow"), "", ""))),
            ],
        );
        let specific = specific(&result);
        assert!(specific
            .get_item("permissionDecision")
            .unwrap()
            .eq("deny")
            .unwrap());
        assert!(specific
            .get_item("permissionDecisionReason")
            .unwrap()
            .eq("no")
            .unwrap());
    });
}

#[test]
fn ask_beats_allow_when_nobody_denies() {
    let _case = Case::new();
    Python::attach(|py| {
        let result = merged(
            py,
            "PreToolUse",
            &[
                outcome(py, "a", Some(pre(Some("allow"), "", ""))),
                outcome(py, "b", Some(pre(Some("ask"), "why", ""))),
            ],
        );
        let specific = specific(&result);
        assert!(specific
            .get_item("permissionDecision")
            .unwrap()
            .eq("ask")
            .unwrap());
        assert!(specific
            .get_item("permissionDecisionReason")
            .unwrap()
            .eq("why")
            .unwrap());
    });
}

#[test]
fn all_allow_is_allow_and_quiet_hooks_do_not_vote() {
    let _case = Case::new();
    Python::attach(|py| {
        let result = merged(
            py,
            "PreToolUse",
            &[
                outcome(py, "a", Some(pre(Some("allow"), "", ""))),
                outcome(py, "b", None),
                outcome(py, "c", Some(pre(None, "", ""))),
            ],
        );
        assert!(specific(&result)
            .get_item("permissionDecision")
            .unwrap()
            .eq("allow")
            .unwrap());
        assert!(result.eq(py_json(py, json!({"hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "allow"}}))).unwrap());
    });
}

#[test]
fn no_votes_means_no_decision_key() {
    let _case = Case::new();
    Python::attach(|py| {
        let result = merged(
            py,
            "PostToolUse",
            &[
                outcome(py, "a", None),
                outcome(
                    py,
                    "b",
                    Some(json!({"hookSpecificOutput": {"hookEventName": "PostToolUse"}})),
                ),
            ],
        );
        assert!(result
            .eq(py_json(
                py,
                json!({"hookSpecificOutput": {"hookEventName": "PostToolUse"}})
            ))
            .unwrap());
    });
}

#[test]
fn every_denying_reason_is_kept_in_order() {
    let _case = Case::new();
    Python::attach(|py| {
        let result = merged(
            py,
            "PreToolUse",
            &[
                outcome(py, "a", Some(pre(Some("deny"), "first", ""))),
                outcome(py, "b", Some(pre(Some("allow"), "", ""))),
                outcome(py, "c", Some(pre(Some("deny"), "second", ""))),
            ],
        );
        assert!(specific(&result)
            .get_item("permissionDecisionReason")
            .unwrap()
            .eq(format!("first{}second", separator(py)))
            .unwrap());
    });
}

#[test]
fn losing_reasons_are_dropped() {
    let _case = Case::new();
    Python::attach(|py| {
        let result = merged(
            py,
            "PreToolUse",
            &[
                outcome(py, "a", Some(pre(Some("ask"), "ask-reason", ""))),
                outcome(py, "b", Some(pre(Some("deny"), "deny-reason", ""))),
            ],
        );
        assert!(specific(&result)
            .get_item("permissionDecisionReason")
            .unwrap()
            .eq("deny-reason")
            .unwrap());
    });
}

#[test]
fn additional_context_concatenates_in_hook_order() {
    let _case = Case::new();
    Python::attach(|py| {
        let result = merged(
            py,
            "PostToolUse",
            &[
                outcome(
                    py,
                    "a",
                    Some(json!({"hookSpecificOutput": {"additionalContext": "one"}})),
                ),
                outcome(py, "b", None),
                outcome(
                    py,
                    "c",
                    Some(json!({"hookSpecificOutput": {"additionalContext": "two"}})),
                ),
            ],
        );
        assert!(specific(&result)
            .get_item("additionalContext")
            .unwrap()
            .eq(format!("one{}two", separator(py)))
            .unwrap());
    });
}

#[test]
fn error_is_visible_in_system_message_and_context() {
    let _case = Case::new();
    Python::attach(|py| {
        let failed = failed(py, "read_budget", "ValueError: boom", false);
        let result = merged(
            py,
            "PostToolUse",
            &[outcome(py, "ok", Some(pre(None, "", ""))), failed.clone()],
        );
        let line = error_line(py, &failed);
        assert!(result
            .get_item("systemMessage")
            .unwrap()
            .extract::<String>()
            .unwrap()
            .contains(&line));
        assert!(specific(&result)
            .get_item("additionalContext")
            .unwrap()
            .extract::<String>()
            .unwrap()
            .contains(&line));
        assert!(!specific(&result).contains("permissionDecision").unwrap());
    });
}

#[test]
fn fail_closed_pretooluse_error_denies() {
    let _case = Case::new();
    Python::attach(|py| {
        let failed = failed(py, "crg_gate_verify", "timed out after 5s", true);
        let result = merged(
            py,
            "PreToolUse",
            &[
                outcome(py, "ok", Some(pre(Some("allow"), "", ""))),
                failed.clone(),
            ],
        );
        let line = error_line(py, &failed);
        assert!(specific(&result)
            .get_item("permissionDecision")
            .unwrap()
            .eq("deny")
            .unwrap());
        assert!(specific(&result)
            .get_item("permissionDecisionReason")
            .unwrap()
            .eq(&line)
            .unwrap());
        assert!(result.get_item("systemMessage").unwrap().eq(&line).unwrap());
    });
}

#[test]
fn fail_open_pretooluse_error_does_not_deny() {
    let _case = Case::new();
    Python::attach(|py| {
        let failed = failed(py, "post_edit", "exit 1", false);
        let result = merged(
            py,
            "PreToolUse",
            &[
                outcome(py, "ok", Some(pre(Some("allow"), "", ""))),
                failed.clone(),
            ],
        );
        assert!(specific(&result)
            .get_item("permissionDecision")
            .unwrap()
            .eq("allow")
            .unwrap());
        assert!(result
            .get_item("systemMessage")
            .unwrap()
            .extract::<String>()
            .unwrap()
            .contains(&error_line(py, &failed)));
    });
}

#[test]
fn fail_closed_only_applies_to_pretooluse() {
    let _case = Case::new();
    Python::attach(|py| {
        let failed = failed(py, "x", "exit 1", true);
        let result = merged(py, "PostToolUse", std::slice::from_ref(&failed));
        assert!(!specific(&result).contains("permissionDecision").unwrap());
        assert!(specific(&result)
            .get_item("additionalContext")
            .unwrap()
            .extract::<String>()
            .unwrap()
            .contains(&error_line(py, &failed)));
    });
}

#[test]
fn legacy_top_level_deny_counts_as_deny_on_pretooluse() {
    let _case = Case::new();
    Python::attach(|py| {
        let result = merged(
            py,
            "PreToolUse",
            &[
                outcome(py, "a", Some(pre(Some("allow"), "", ""))),
                outcome(
                    py,
                    "guard",
                    Some(json!({"decision": "deny", "reason": "claim"})),
                ),
            ],
        );
        assert!(specific(&result)
            .get_item("permissionDecision")
            .unwrap()
            .eq("deny")
            .unwrap());
        assert!(specific(&result)
            .get_item("permissionDecisionReason")
            .unwrap()
            .eq("claim")
            .unwrap());
    });
}

#[test]
fn post_tool_block_survives_with_reason() {
    let _case = Case::new();
    Python::attach(|py| {
        let result = merged(
            py,
            "PostToolUse",
            &[
                outcome(py, "a", None),
                outcome(py, "b", Some(json!({"decision": "block", "reason": "bad"}))),
            ],
        );
        assert!(result.get_item("decision").unwrap().eq("block").unwrap());
        assert!(result.get_item("reason").unwrap().eq("bad").unwrap());
    });
}

#[test]
fn continue_false_wins_and_stop_reasons_join() {
    let _case = Case::new();
    Python::attach(|py| {
        let result = merged(
            py,
            "PostToolUse",
            &[
                outcome(
                    py,
                    "a",
                    Some(json!({"continue": false, "stopReason": "halt"})),
                ),
                outcome(py, "b", Some(json!({"continue": true}))),
                outcome(
                    py,
                    "c",
                    Some(json!({"systemMessage": "note", "suppressOutput": true})),
                ),
            ],
        );
        assert!(result
            .get_item("continue")
            .unwrap()
            .is(PyBool::new(py, false)));
        assert!(result.get_item("stopReason").unwrap().eq("halt").unwrap());
        assert!(result
            .get_item("suppressOutput")
            .unwrap()
            .is(PyBool::new(py, true)));
        assert!(result
            .get_item("systemMessage")
            .unwrap()
            .eq("note")
            .unwrap());
    });
}

#[test]
fn updated_tool_output_last_writer_wins_and_conflict_is_loud() {
    let _case = Case::new();
    Python::attach(|py| {
        let result = merged(
            py,
            "PostToolUse",
            &[
                outcome(
                    py,
                    "a",
                    Some(json!({"hookSpecificOutput": {"updatedToolOutput": "first"}})),
                ),
                outcome(
                    py,
                    "b",
                    Some(json!({"hookSpecificOutput": {"updatedToolOutput": "second"}})),
                ),
            ],
        );
        assert!(specific(&result)
            .get_item("updatedToolOutput")
            .unwrap()
            .eq("second")
            .unwrap());
        assert!(result
            .get_item("systemMessage")
            .unwrap()
            .extract::<String>()
            .unwrap()
            .contains("HOOK CONFLICT [b]"));
    });
}

#[test]
fn event_name_is_always_the_dispatched_event() {
    let _case = Case::new();
    Python::attach(|py| {
        let result = merged(
            py,
            "SessionStart",
            &[outcome(
                py,
                "a",
                Some(json!({"hookSpecificOutput": {"hookEventName": "PreToolUse"}})),
            )],
        );
        assert!(specific(&result)
            .get_item("hookEventName")
            .unwrap()
            .eq("SessionStart")
            .unwrap());
    });
}

#[test]
fn schema_events_keep_hook_event_name_exactly() {
    let _case = Case::new();
    Python::attach(|py| {
        for event in ["PreToolUse", "PostToolUse", "SessionStart"] {
            let result = merged(py, event, &[outcome(py, "a", None)]);
            assert!(result
                .eq(py_json(
                    py,
                    json!({"hookSpecificOutput": {"hookEventName": event}})
                ))
                .unwrap());
        }
    });
}

#[test]
fn session_end_never_carries_hook_specific_output() {
    let _case = Case::new();
    Python::attach(|py| {
        let result = merged(
            py,
            "SessionEnd",
            &[
                outcome(py, "a", None),
                outcome(
                    py,
                    "b",
                    Some(json!({"hookSpecificOutput": {"hookEventName": "SessionEnd"}})),
                ),
            ],
        );
        assert!(result.eq(py_json(py, json!({}))).unwrap());
    });
}

#[test]
fn subagent_stop_and_stop_never_carry_hook_specific_output() {
    let _case = Case::new();
    Python::attach(|py| {
        for event in ["SubagentStop", "Stop"] {
            let result = merged(
                py,
                event,
                &[outcome(py, "a", Some(pre(Some("deny"), "no", "")))],
            );
            assert!(result.eq(py_json(py, json!({}))).unwrap());
        }
    });
}

#[test]
fn session_end_error_still_surfaces_in_system_message() {
    let _case = Case::new();
    Python::attach(|py| {
        let result = merged(py, "SessionEnd", &[failed(py, "a", "boom", false)]);
        assert!(!result.contains("hookSpecificOutput").unwrap());
        assert!(result
            .get_item("systemMessage")
            .unwrap()
            .extract::<String>()
            .unwrap()
            .contains("HOOK ERROR [a]: boom"));
    });
}

#[test]
fn non_dict_output_is_ignored_without_crashing() {
    let _case = Case::new();
    Python::attach(|py| {
        let list = PyList::new(py, ["not", "a", "dict"]).unwrap();
        let invalid = production(py)
            .getattr("HookOutcome")
            .unwrap()
            .call1(("b", list))
            .unwrap();
        let result = merged(py, "PreToolUse", &[outcome(py, "a", None), invalid]);
        assert!(result
            .eq(py_json(
                py,
                json!({"hookSpecificOutput": {"hookEventName": "PreToolUse"}})
            ))
            .unwrap());
    });
}
