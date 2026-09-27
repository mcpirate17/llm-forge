#![cfg(feature = "python-compat-tests")]
//! Rust-owned contracts for the local-model authority boundary and hook wiring.

#[path = "python_contracts/hook_matrix_fixture.rs"]
mod hook_matrix_fixture;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use hook_matrix_fixture::HookMatrix;
use pyo3::prelude::*;
use pyo3::types::PyDict;
use serde_json::{json, Value};
use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use support::{assert_error, module, text, Case};

fn hook_output(hook: &Path, payload: Value) -> Value {
    let mut child = Command::new(hook)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start temporary hook");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.to_string().as_bytes())
        .unwrap();
    let output = child.wait_with_output().expect("finish temporary hook");
    assert!(
        output.status.success(),
        "{}: {}",
        hook.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&output.stdout).expect("JSON hook response");
    value["hookSpecificOutput"].clone()
}

fn policy_reason(py: Python<'_>, command: &str, local_runtime: bool) -> Option<String> {
    let policy = module(py, "conductor.local_ai_policy");
    let kw = PyDict::new(py);
    kw.set_item("local_runtime", local_runtime).unwrap();
    policy
        .getattr("deny_local_ai_command")
        .unwrap()
        .call((command,), Some(&kw))
        .unwrap()
        .extract()
        .unwrap()
}

#[test]
fn clerical_classes_accept_low_risk_prompts() {
    let _case = Case::new();
    Python::attach(|py| {
        let policy = module(py, "conductor.local_ai_policy");
        let allowed = policy.getattr("ALLOWED_LOCAL_TASKS").unwrap();
        assert_eq!(allowed.len().unwrap(), 4);
        for task in ["notes", "summary", "organization", "compaction"] {
            assert!(allowed.contains(task).unwrap(), "missing {task}");
            let accepted = policy
                .getattr("require_clerical_task")
                .unwrap()
                .call1((task, "Summarize and organize these notes"))
                .unwrap();
            assert_eq!(text(&accepted), task);
        }
    });
}

fn authority_request_is_rejected(prompt: &str) {
    let _case = Case::new();
    Python::attach(|py| {
        let policy = module(py, "conductor.local_ai_policy");
        let error = policy
            .getattr("require_clerical_task")
            .unwrap()
            .call1(("summary", prompt))
            .unwrap_err();
        assert_error(
            py,
            error,
            &policy.getattr("LocalAIPolicyError").unwrap(),
            "zero approval authority",
        );
    });
}

macro_rules! authority_case {
    ($name:ident, $prompt:expr) => {
        #[test]
        fn $name() {
            authority_request_is_rejected($prompt);
        }
    };
}

authority_case!(
    clerical_label_cannot_hide_approval,
    "Approve the post-smoke run"
);
authority_case!(
    clerical_label_cannot_hide_conduct,
    "Can we conduct another training run?"
);
authority_case!(
    clerical_label_cannot_hide_verdict,
    "Return FINAL VERDICT: PASS if optimizer steps may start"
);
authority_case!(
    clerical_label_cannot_hide_resume,
    "Recommend whether to resume the experiment"
);
authority_case!(
    clerical_label_cannot_hide_permission,
    "Give permission to launch evaluation"
);

#[test]
fn hook_requires_explicit_clerical_class_for_local_chat() {
    let _case = Case::new();
    Python::attach(|py| {
        let policy = module(py, "conductor.local_ai_policy");
        let expected = text(&policy.getattr("UNCLASSIFIED_REASON").unwrap());
        for command in [
            "ollama run qwen3.5:9b \"summarize these notes\"",
            "curl http://127.0.0.1:11434/api/chat -d '{\"prompt\":\"organize\"}'",
        ] {
            assert_eq!(
                policy_reason(py, command, false),
                Some(expected.clone()),
                "{command}"
            );
        }
    });
}

#[test]
fn hook_allows_classified_low_risk_local_chat() {
    let _case = Case::new();
    Python::attach(|py| {
        for command in [
            "LOCAL_AI_TASK=summary ollama run qwen3.5:9b \"summarize these notes\"",
            "env LOCAL_AI_TASK=organization curl http://localhost:11434/api/chat -d '{\"prompt\":\"organize notes\"}'",
        ] {
            assert_eq!(policy_reason(py, command, false), None, "{command}");
        }
    });
}

#[test]
fn hook_denies_authority_request_even_when_classified() {
    let _case = Case::new();
    Python::attach(|py| {
        let policy = module(py, "conductor.local_ai_policy");
        assert_eq!(
            policy_reason(py, "LOCAL_AI_TASK=summary ollama run qwen3.5:9b \"Should we approve the post-smoke training run?\"", false),
            Some(text(&policy.getattr("DENY_REASON").unwrap()))
        );
    });
}

#[test]
fn local_agent_runtime_cannot_send_approval_verdict() {
    let _case = Case::new();
    Python::attach(|py| {
        let policy = module(py, "conductor.local_ai_policy");
        assert_eq!(
            policy_reason(py, "python -m conductor.agent_a2a send --to codex-phase22 --body 'AI_POWERED: true FINAL VERDICT: PASS; training may start'", true),
            Some(text(&policy.getattr("DENY_REASON").unwrap()))
        );
    });
}

#[test]
fn frontier_runtime_command_is_not_misclassified_as_local() {
    let _case = Case::new();
    Python::attach(|py| {
        assert_eq!(
            policy_reason(py, "python -m conductor.agent_a2a send --to codex-phase22 --body 'FINAL VERDICT: PASS'", false),
            None
        );
    });
}

#[test]
fn hook_ignores_mentions_embeddings_and_non_inference_commands() {
    let _case = Case::new();
    Python::attach(|py| {
        for command in [
            "echo \"ollama run qwen3.5:9b is a local command\"",
            "ollama ps",
            "ollama stop qwen3.5:9b",
            "curl http://127.0.0.1:11434/api/embed -d '{} '",
            "rg \"qwen3.5:9b\" conductor/",
        ] {
            assert_eq!(policy_reason(py, command, false), None, "{command}");
        }
    });
}

fn agent_shell_hook_reaches_shared_policy(config_path: &str, hook_path: &str) {
    let mut case = Case::new();
    Python::attach(|py| {
        let fixture = HookMatrix::new(py, &mut case);
        let config = fs::read_to_string(fixture.root().join(config_path)).unwrap();
        let hook = fixture.root().join(hook_path);
        let hook_name = hook.file_name().unwrap().to_str().unwrap();
        assert!(config.contains(hook_name), "{config_path}");
        assert!(fs::read_to_string(&hook)
            .unwrap()
            .contains("conductor.current_work_guard"));
        let denial = hook_output(
            &hook,
            json!({"tool_name":"Read", "tool_input":{"file_path":".current_work.md"}}),
        );
        assert_eq!(denial["permissionDecision"], "deny", "{hook_path}");
    });
}

macro_rules! hook_case {
    ($name:ident, $config:expr, $hook:expr) => {
        #[test]
        fn $name() {
            agent_shell_hook_reaches_shared_policy($config, $hook);
        }
    };
}

hook_case!(
    codex_shell_hook_reaches_shared_policy,
    ".codex/hooks.json",
    ".codex/hooks/pre-edit.sh"
);
hook_case!(
    claude_shell_hook_reaches_shared_policy,
    ".claude/settings.json",
    ".claude/hooks/pre-edit.sh"
);
hook_case!(
    qwen_shell_hook_reaches_shared_policy,
    ".qwen/settings.json",
    ".qwen/hooks/pre-edit.sh"
);
hook_case!(
    grok_shell_hook_reaches_shared_policy,
    ".grok/hooks/workspace.json",
    ".grok/hooks/pre-edit.sh"
);

#[test]
fn qwen_clerk_hook_declares_local_runtime() {
    let mut case = Case::new();
    Python::attach(|py| {
        let fixture = HookMatrix::new(py, &mut case);
        let hook = fixture.root().join(".qwen/hooks/pre-edit.sh");
        assert!(fs::read_to_string(&hook)
            .unwrap()
            .contains("LOCAL_AI_RUNTIME=1"));
        let verdict = hook_output(
            &hook,
            json!({"tool_name":"run_shell_command", "tool_input":{"command":"python -m conductor.agent_a2a send --to codex-phase22 --body 'FINAL VERDICT: PASS'"}}),
        );
        let expected = text(
            &module(py, "conductor.local_ai_policy")
                .getattr("DENY_REASON")
                .unwrap(),
        );
        assert_eq!(verdict["permissionDecisionReason"], expected);
    });
}
