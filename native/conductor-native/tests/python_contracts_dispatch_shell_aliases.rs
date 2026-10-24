#![cfg(feature = "python-compat-tests")]
//! Provider shell inputs share dispatch selection and stdin without changing commands.

#[path = "python_contracts/agent_comm_support.rs"]
#[allow(dead_code)]
mod comm_support;
#[path = "python_contracts/dispatch_runner_support.rs"]
#[allow(dead_code)]
mod runner_support;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::prelude::*;
use pyo3::types::PyBytes;
use runner_support::{case, context, deny, outcome_names, output, runner};
use serde_json::json;
use support::module;

const COMMAND: &str = "set -euo pipefail\nrg -n 'a b|c' -- 'path with spaces'";
const ALIASES: &[&str] = &[
    "Bash",
    "bash",
    "shell",
    "exec_command",
    "shell_command",
    "run_shell_command",
    "run_terminal_command",
    "functions.exec_command",
    "functions.shell",
    "functions.run_shell_command",
];

#[test]
fn shell_aliases_share_selection_and_exact_command_text() {
    let case = case();
    Python::attach(|py| {
        let hooks = runner(py).getattr("select").unwrap();
        let expected = hooks
            .call1(("PreToolUse", output(py, json!({"tool_name":"Bash"}))))
            .unwrap();
        for alias in ALIASES {
            for field in ["tool_name", "toolName"] {
                for key in ["command", "cmd"] {
                    let body = json!({field:alias,"tool_input":{key:COMMAND,"timeout":600},"session_id":"keep"});
                    let original = output(py, body.clone());
                    let selected = hooks.call1(("PreToolUse", &original)).unwrap();
                    assert_eq!(
                        outcome_names(&selected),
                        outcome_names(&expected),
                        "{alias}"
                    );
                    let ctx = context(py, "PreToolUse", body, case.root());
                    let payload = ctx.getattr("payload").unwrap();
                    assert_eq!(
                        payload
                            .get_item("tool_name")
                            .unwrap()
                            .extract::<String>()
                            .unwrap(),
                        "Bash"
                    );
                    let input = payload.get_item("tool_input").unwrap();
                    assert_eq!(
                        input
                            .get_item("command")
                            .unwrap()
                            .extract::<String>()
                            .unwrap(),
                        COMMAND
                    );
                    assert_eq!(
                        input.get_item(key).unwrap().extract::<String>().unwrap(),
                        COMMAND
                    );
                    assert_eq!(
                        input.get_item("timeout").unwrap().extract::<i32>().unwrap(),
                        600
                    );
                    let decoded = module(py, "json")
                        .getattr("loads")
                        .unwrap()
                        .call1((ctx.getattr("raw").unwrap(),))
                        .unwrap();
                    assert!(decoded.eq(&payload).unwrap());
                    assert!(
                        !original.contains("tool_name").unwrap()
                            || original
                                .get_item("tool_name")
                                .unwrap()
                                .extract::<String>()
                                .unwrap()
                                == *alias
                    );
                }
            }
        }
    });
}

#[test]
fn malformed_known_shell_inputs_fail_before_any_hook_can_be_skipped() {
    let case = case();
    Python::attach(|py| {
        for body in [
            json!({"tool_name":"exec_command"}),
            json!({"tool_name":"exec_command","tool_input":[]}),
            json!({"tool_name":"Bash","tool_input":{}}),
            json!({"tool_name":"shell","tool_input":{"command":["sh","-c","echo ok"]}}),
            json!({"tool_name":"functions.exec_command","tool_input":{"cmd":null}}),
            json!({"tool_name":"Bash","tool_input":{"command":"one","cmd":"two"}}),
        ] {
            let raw = serde_json::to_vec(&body).unwrap();
            let error = runner(py)
                .getattr("build_context")
                .unwrap()
                .call1((
                    "PreToolUse",
                    PyBytes::new(py, &raw),
                    support::path(py, case.root()),
                ))
                .unwrap_err();
            assert!(error.is_instance_of::<pyo3::exceptions::PyValueError>(py));
            assert!(error.to_string().contains("shell tool"));
            let result = runner(py)
                .getattr("dispatch")
                .unwrap()
                .call1((
                    "PreToolUse",
                    PyBytes::new(py, &raw),
                    support::path(py, case.root()),
                ))
                .unwrap()
                .get_item(0)
                .unwrap();
            let specific = result.get_item("hookSpecificOutput").unwrap();
            assert_eq!(
                specific
                    .get_item("permissionDecision")
                    .unwrap()
                    .extract::<String>()
                    .unwrap(),
                "deny"
            );
            assert!(specific
                .get_item("permissionDecisionReason")
                .unwrap()
                .extract::<String>()
                .unwrap()
                .contains("shell tool"));
        }
    });
}

#[test]
fn unknown_tools_and_lifecycle_payloads_keep_original_bytes() {
    let case = case();
    Python::attach(|py| {
        for (event, raw) in [
            (
                "PreToolUse",
                br#"{ "tool_name": "mcp__custom__exec_command", "tool_input": {"cmd": []} }"#
                    .as_slice(),
            ),
            (
                "SessionStart",
                br#"{ "source": "resume", "tool_name": "exec_command" }"#.as_slice(),
            ),
            (
                "SessionEnd",
                br#"{ "reason": "exit", "tool_name": "exec_command" }"#.as_slice(),
            ),
        ] {
            let ctx = runner(py)
                .getattr("build_context")
                .unwrap()
                .call1((event, PyBytes::new(py, raw), support::path(py, case.root())))
                .unwrap();
            assert_eq!(
                ctx.getattr("raw").unwrap().extract::<Vec<u8>>().unwrap(),
                raw
            );
            let subject = runner(py)
                .getattr("subject_of")
                .unwrap()
                .call1((event, ctx.getattr("payload").unwrap()))
                .unwrap()
                .extract::<String>()
                .unwrap();
            assert_eq!(
                subject,
                match event {
                    "SessionStart" => "resume",
                    "SessionEnd" => "exit",
                    _ => "mcp__custom__exec_command",
                }
            );
        }
    });
}

#[test]
fn normalized_aliases_retain_already_served_native_denials() {
    let mut case = case();
    case.set_env("FORGE_NATIVE_HOOKS", "pre_bash");
    case.set_env(
        "FORGE_NATIVE_ANSWERS",
        &json!({"pre_bash":deny()}).to_string(),
    );
    Python::attach(|py| {
        let payload = output(
            py,
            json!({"tool_name":"functions.exec_command","tool_input":{"cmd":COMMAND}}),
        );
        let selected = runner(py)
            .getattr("select")
            .unwrap()
            .call1(("PreToolUse", &payload))
            .unwrap();
        assert!(!outcome_names(&selected).contains(&"pre_bash".to_owned()));
        let outcomes = runner(py)
            .getattr("_splice_native_outcomes")
            .unwrap()
            .call1(("PreToolUse", &payload, pyo3::types::PyList::empty(py)))
            .unwrap();
        assert_eq!(outcome_names(&outcomes), ["pre_bash"]);
        assert!(outcomes
            .get_item(0)
            .unwrap()
            .getattr("output")
            .unwrap()
            .eq(output(py, deny()))
            .unwrap());
    });
}

#[test]
fn stdin_reading_adapters_receive_the_canonical_payload() {
    let case = case();
    Python::attach(|py| {
        let _io = runner_support::IoRestore::new(py);
        let ctx = context(
            py,
            "PreToolUse",
            json!({"toolName":"run_shell_command","tool_input":{"cmd":COMMAND,"cwd":"keep"}}),
            case.root(),
        );
        let callback = runner_support::callback(py, "ctx", |py, _ctx| {
            Ok(module(py, "json")
                .getattr("load")?
                .call1((module(py, "sys").getattr("stdin")?,))?
                .unbind())
        });
        let _patch = runner_support::OptionalPatch::new(
            runner_support::adapters(py).as_any(),
            "read_canonical_stdin",
            callback.as_any(),
        );
        let spec = runner_support::spec(py, "stdin_contract", Some("read_canonical_stdin"), &[], 5);
        let outcome = runner(py)
            .getattr("run_one")
            .unwrap()
            .call1((spec, &ctx))
            .unwrap();
        assert!(outcome.getattr("error").unwrap().is_none());
        assert!(outcome
            .getattr("output")
            .unwrap()
            .eq(ctx.getattr("payload").unwrap())
            .unwrap());
    });
}
