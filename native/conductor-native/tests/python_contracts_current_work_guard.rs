#![cfg(feature = "python-compat-tests")]
//! Contracts for the shipped current-work and local-AI PreToolUse guard.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyModule};
use serde_json::{json, Value};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use support::{assert_error, module, text, AttrPatch, Case};

const MUTATION_HINT: &str = "Mutation evidence covers ONLY the files you changed";

fn json_object<'py>(py: Python<'py>, value: Value) -> Bound<'py, PyAny> {
    module(py, "json")
        .getattr("loads")
        .unwrap()
        .call1((value.to_string(),))
        .unwrap()
}

fn guard<'py>(py: Python<'py>) -> Bound<'py, PyModule> {
    module(py, "conductor.current_work_guard")
}

fn evaluate(py: Python<'_>, payload: Value) -> Option<String> {
    guard(py)
        .getattr("evaluate_payload")
        .unwrap()
        .call1((json_object(py, payload),))
        .unwrap()
        .extract()
        .unwrap()
}

fn advisory(py: Python<'_>, payload: Value) -> Option<String> {
    guard(py)
        .getattr("advisory_for_payload")
        .unwrap()
        .call1((json_object(py, payload),))
        .unwrap()
        .extract()
        .unwrap()
}

fn shell(py: Python<'_>, command: &str) -> Option<String> {
    evaluate(
        py,
        json!({"tool_name":"Bash","tool_input":{"command":command}}),
    )
}

fn current_work(case: &Case) -> String {
    case.root()
        .join(".current_work.md")
        .to_str()
        .unwrap()
        .to_owned()
}

#[test]
fn allows_unrelated_files() {
    let _case = Case::new();
    Python::attach(|py| {
        assert_eq!(
            evaluate(
                py,
                json!({"tool_input":{"file_path":"conductor/handoff.py","content":"x".repeat(5000)}})
            ),
            None
        );
    });
}

#[test]
fn denies_direct_read_of_current_work() {
    let case = Case::new();
    Python::attach(|py| {
        let reason = evaluate(
            py,
            json!({"tool_name":"Read","tool_input":{"file_path":current_work(&case)}}),
        )
        .unwrap();
        assert!(reason.contains("cannot access"));
    });
}

#[test]
fn denies_direct_write_even_when_short() {
    let case = Case::new();
    Python::attach(|py| {
        let reason = evaluate(py, json!({"tool_name":"Write","tool_input":{"file_path":current_work(&case),"content":"short status"}})).unwrap();
        assert!(reason.contains("BLOCKED"));
        assert!(reason.contains("conductor.handoff"));
    });
}

#[test]
fn denies_obvious_shell_reads() {
    let _case = Case::new();
    Python::attach(|py| {
        for command in [
            "cat .current_work.md",
            "sed -n '1,20p' /repo/.current_work.md",
            "rg heading .current_work.md",
        ] {
            assert!(
                shell(py, command).unwrap().contains("direct shell access"),
                "{command}"
            );
        }
    });
}

#[test]
fn allows_safe_shell_mentions_and_bounded_utilities() {
    let _case = Case::new();
    Python::attach(|py| {
        for command in [
            "echo '.current_work.md is a compatibility log'",
            "rg -n '\\.current_work\\.md' README.md",
            "python -m conductor.handoff append --owner codex --title x --body y",
            "python -m conductor.active_state update",
        ] {
            assert_eq!(shell(py, command), None, "{command}");
        }
    });
}

#[test]
fn denies_local_ai_approval_through_shared_agent_hook() {
    let _case = Case::new();
    Python::attach(|py| {
        let reason = shell(
            py,
            "LOCAL_AI_TASK=summary ollama run qwen3.5:9b 'Approve the post-smoke training run'",
        )
        .unwrap();
        assert!(reason.contains("zero approval authority"));
    });
}

#[test]
fn allows_classified_local_note_summary_through_shared_agent_hook() {
    let _case = Case::new();
    Python::attach(|py| {
        assert_eq!(shell(py, "LOCAL_AI_TASK=summary ollama run qwen3.5:9b 'Summarize these implementation notes'"), None);
    });
}

#[test]
fn local_agent_runtime_cannot_send_approval_receipt() {
    let mut case = Case::new();
    case.set_env("LOCAL_AI_RUNTIME", "1");
    Python::attach(|py| {
        let reason = shell(py, "python -m conductor.agent_a2a send --to codex-phase22 --body 'AI_POWERED: true FINAL VERDICT: PASS'").unwrap();
        assert!(reason.contains("zero approval authority"));
    });
}

#[test]
fn hook_response_deny_shape() {
    let _case = Case::new();
    Python::attach(|py| {
        let payload = guard(py)
            .getattr("hook_response")
            .unwrap()
            .call1(("BLOCKED: dump",))
            .unwrap();
        let dictionary = payload.cast::<PyDict>().unwrap();
        assert_eq!(dictionary.len(), 1);
        let out = payload.get_item("hookSpecificOutput").unwrap();
        assert_eq!(
            out.get_item("permissionDecision")
                .unwrap()
                .extract::<String>()
                .unwrap(),
            "deny"
        );
        assert!(text(&out.get_item("permissionDecisionReason").unwrap()).contains("BLOCKED"));
        module(py, "json")
            .getattr("dumps")
            .unwrap()
            .call1((payload,))
            .unwrap();
    });
}

#[test]
fn hook_response_is_silent_on_success() {
    let _case = Case::new();
    Python::attach(|py| {
        let response = guard(py).getattr("hook_response").unwrap();
        assert!(response.call1((py.None(),)).unwrap().is_none());
        let kwargs = PyDict::new(py);
        kwargs.set_item("protocol", "grok").unwrap();
        assert!(response
            .call((py.None(),), Some(&kwargs))
            .unwrap()
            .is_none());
    });
}

#[test]
fn hook_response_uses_grok_deny_shape_for_camel_case_payloads() {
    let _case = Case::new();
    Python::attach(|py| {
        let body = guard(py);
        let payload = json_object(
            py,
            json!({"hookEventName":"pre_tool_use","toolName":"write","toolInput":{}}),
        );
        let protocol: String = body
            .getattr("hook_protocol")
            .unwrap()
            .call1((payload,))
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(protocol, "grok");
        let kwargs = PyDict::new(py);
        kwargs.set_item("protocol", "grok").unwrap();
        let response = body
            .getattr("hook_response")
            .unwrap()
            .call(("BLOCKED: dump",), Some(&kwargs))
            .unwrap();
        assert!(response
            .eq(json_object(
                py,
                json!({"decision":"deny","reason":"BLOCKED: dump"})
            ))
            .unwrap());
    });
}

#[test]
fn cli_is_silent_on_success() {
    let _case = Case::new();
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("src")
        .canonicalize()
        .unwrap();
    let mut child =
        Command::new(std::env::var_os("PYO3_PYTHON").unwrap_or_else(|| "python3".into()))
            .args(["-m", "conductor.current_work_guard"])
            .env("PYTHONPATH", source)
            .env("PYTHONDONTWRITEBYTECODE", "1")
            .env("CUDA_VISIBLE_DEVICES", "")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(br#"{"tool_input":{"file_path":"conductor/handoff.py","content":"x"}}"#)
        .unwrap();
    let result = child.wait_with_output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(result.stdout.is_empty());
    assert!(result.stderr.is_empty());
}

#[test]
fn denies_grok_camelcase_tool_input() {
    let case = Case::new();
    let path = case.write(
        ".current_work.md",
        "# Active Coordination\n\n## Old\n\nbody\n",
    );
    Python::attach(|py| {
        let reason = evaluate(py, json!({"hookEventName":"pre_tool_use","toolName":"read_file","toolInput":{"file_path":path}})).unwrap();
        assert!(reason.contains("BLOCKED"));
    });
}

#[test]
fn advisory_reminds_on_test_file_writes() {
    let _case = Case::new();
    Python::attach(|py| {
        let payload = json!({"tool_name":"Write","tool_input":{"file_path":"research/tests/test_new.py","content":"x"}});
        assert_eq!(evaluate(py, payload.clone()), None);
        let hint = advisory(py, payload).unwrap();
        assert!(hint.contains(MUTATION_HINT));
        let kwargs = PyDict::new(py);
        kwargs.set_item("protocol", "codex").unwrap();
        kwargs.set_item("advisory", hint).unwrap();
        let response = guard(py)
            .getattr("hook_response")
            .unwrap()
            .call((py.None(),), Some(&kwargs))
            .unwrap();
        let context = response
            .get_item("hookSpecificOutput")
            .unwrap()
            .get_item("additionalContext")
            .unwrap();
        assert!(text(&context).contains(MUTATION_HINT));
    });
}

#[test]
fn advisory_covers_javascript_specs_and_skips_non_tests() {
    let _case = Case::new();
    Python::attach(|py| {
        assert!(advisory(py, json!({"tool_name":"Write","tool_input":{"file_path":"aria_designer/e2e/designer.spec.js"}})).is_some());
        assert_eq!(
            advisory(
                py,
                json!({"tool_name":"Write","tool_input":{"file_path":"conductor/handoff.py"}})
            ),
            None
        );
    });
}

#[test]
fn deny_still_wins_over_test_file_advisory() {
    let case = Case::new();
    Python::attach(|py| {
        let payload = json!({"tool_name":"Write","tool_input":{"file_path":current_work(&case),"content":"status"}});
        assert!(evaluate(py, payload.clone()).is_some());
        assert_eq!(advisory(py, payload), None);
    });
}

#[test]
fn hook_protocol_grok_and_unsupported_advisory() {
    let _case = Case::new();
    Python::attach(|py| {
        let body = guard(py);
        assert_eq!(
            body.getattr("hook_protocol")
                .unwrap()
                .call1((json_object(py, json!({"toolName":"Write"})),))
                .unwrap()
                .extract::<String>()
                .unwrap(),
            "grok"
        );
        let response = body.getattr("hook_response").unwrap();
        let kwargs = PyDict::new(py);
        kwargs.set_item("protocol", "grok").unwrap();
        let deny = response.call(("BLOCKED: x",), Some(&kwargs)).unwrap();
        assert!(deny
            .eq(json_object(
                py,
                json!({"decision":"deny","reason":"BLOCKED: x"})
            ))
            .unwrap());
        kwargs.set_item("advisory", "hint").unwrap();
        assert!(!response
            .call((py.None(),), Some(&kwargs))
            .unwrap()
            .is_none());
        kwargs.set_item("protocol", "other").unwrap();
        kwargs.del_item("advisory").unwrap();
        let value_error = module(py, "builtins").getattr("ValueError").unwrap();
        assert_error(
            py,
            response.call(("BLOCKED: x",), Some(&kwargs)).unwrap_err(),
            &value_error,
            "unsupported",
        );
        kwargs.set_item("advisory", "hint").unwrap();
        assert_error(
            py,
            response.call((py.None(),), Some(&kwargs)).unwrap_err(),
            &value_error,
            "unsupported",
        );
    });
}

#[test]
fn main_reads_stdin_and_emits_advisory() {
    let _case = Case::new();
    Python::attach(|py| {
        let body = guard(py);
        let sys = module(py, "sys");
        let io = module(py, "io");
        let stdout = io.getattr("StringIO").unwrap().call0().unwrap();
        let _stdout = AttrPatch::replace(sys.as_any(), "stdout", &stdout);
        let input = io
            .getattr("StringIO")
            .unwrap()
            .call1((json!({"tool_input":{"file_path":"research/tests/test_x.py"}}).to_string(),))
            .unwrap();
        let _stdin = AttrPatch::replace(sys.as_any(), "stdin", &input);
        assert_eq!(
            body.getattr("main")
                .unwrap()
                .call0()
                .unwrap()
                .extract::<i32>()
                .unwrap(),
            0
        );
        let printed: String = stdout.call_method0("getvalue").unwrap().extract().unwrap();
        assert!(printed.contains(MUTATION_HINT));
        for invalid in ["not-json", "[]"] {
            let input = io.getattr("StringIO").unwrap().call1((invalid,)).unwrap();
            let _stdin = AttrPatch::replace(sys.as_any(), "stdin", &input);
            assert_eq!(
                body.getattr("main")
                    .unwrap()
                    .call0()
                    .unwrap()
                    .extract::<i32>()
                    .unwrap(),
                0
            );
        }
    });
}

#[test]
fn shell_segment_edge_cases() {
    let _case = Case::new();
    Python::attach(|py| {
        let body = guard(py);
        assert!(body
            .getattr("deny_shell_access")
            .unwrap()
            .call1(("cat 'unterminated",))
            .unwrap()
            .is_none());
        assert!(shell(py, "env cat .current_work.md").is_some());
        assert!(shell(py, "rg foo .current_work.md").is_some());
    });
}

#[test]
fn shell_unquoted_and_env_assignment() {
    let _case = Case::new();
    Python::attach(|py| {
        let body = guard(py);
        assert!(body
            .getattr("deny_shell_access")
            .unwrap()
            .call1(("cat '.current_work.md",))
            .unwrap()
            .is_none());
        for command in [
            "FOO=1 cat .current_work.md",
            "rg foo -- .current_work.md",
            "sudo cat .current_work.md",
            "echo x; cat .current_work.md",
        ] {
            assert!(shell(py, command).is_some(), "{command}");
        }
        for command in ["FOO=.current_work.md sudo", "rg .current_work.md --"] {
            assert_eq!(shell(py, command), None, "{command}");
        }
    });
}

#[test]
fn local_ai_policy_fail_closed_edge_cases() {
    let _case = Case::new();
    Python::attach(|py| {
        let policy = module(py, "conductor.local_ai_policy");
        let error_class = policy.getattr("LocalAIPolicyError").unwrap();
        assert_error(
            py,
            policy
                .getattr("require_clerical_task")
                .unwrap()
                .call1(("approval", "summarize notes"))
                .unwrap_err(),
            &error_class,
            "not clerical",
        );
        let unclassified = policy.getattr("UNCLASSIFIED_REASON").unwrap();
        let reason = policy
            .getattr("deny_local_ai_command")
            .unwrap()
            .call1(("ollama run \"unterminated",))
            .unwrap();
        assert!(reason.eq(unclassified).unwrap());
    });
}
