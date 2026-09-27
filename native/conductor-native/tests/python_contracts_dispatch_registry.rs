#![cfg(feature = "python-compat-tests")]
//! Dispatcher registry invariants, with Rust-owned cases and assertions.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyFrozenSet, PyTuple};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use support::{module, path, Case};

const LIVE_COMMANDS: &[&str] = &[
    "$CLAUDE_PROJECT_DIR/.agent_hooks/crg_gate.py mark",
    "env GOVERNANCE_OWNER=\"${GOVERNANCE_OWNER:-claude}\" $CLAUDE_PROJECT_DIR/.agent_hooks/crg_gate.py verify-bash",
    "$CLAUDE_PROJECT_DIR/.claude/hooks/pre-bash.sh",
    "$CLAUDE_PROJECT_DIR/.claude/hooks/pre-edit.sh",
    "$CLAUDE_PROJECT_DIR/.claude/hooks/pre-read-skeleton.sh",
    "env GOVERNANCE_OWNER=\"${GOVERNANCE_OWNER:-claude}\" $CLAUDE_PROJECT_DIR/.agent_hooks/crg_gate.py verify",
    "$CLAUDE_PROJECT_DIR/.agent_hooks/crg_graph_refresh.py",
    "$CLAUDE_PROJECT_DIR/.claude/hooks/post-edit.sh",
    "$CLAUDE_PROJECT_DIR/.agent_hooks/read_budget.py",
    "$CLAUDE_PROJECT_DIR/.claude/hooks/obsidian_sync.py post-edit",
    "$CLAUDE_PROJECT_DIR/.claude/hooks/post-bash-graph.sh",
    "$CLAUDE_PROJECT_DIR/.claude/hooks/post-bash-quiet.sh",
    "python3 -m conductor.context_telemetry",
    "$CLAUDE_PROJECT_DIR/.claude/hooks/obsidian_sync.py session-end",
    "$CLAUDE_PROJECT_DIR/.claude/hooks/session-start.sh",
    "$CLAUDE_PROJECT_DIR/.claude/hooks/session-handoff.sh",
];

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn registry(py: Python<'_>) -> Bound<'_, pyo3::types::PyModule> {
    module(py, "tooling.hooks.dispatch.registry")
}

fn executable(py: Python<'_>, file: &Path) -> bool {
    let os = module(py, "os");
    os.getattr("access")
        .unwrap()
        .call1((path(py, file), os.getattr("X_OK").unwrap()))
        .unwrap()
        .extract()
        .unwrap()
}

#[test]
fn settings_template_is_generated_from_the_registry() {
    let _case = Case::new();
    let template =
        fs::read_to_string(repo().join("src/tooling/hooks/claude/settings.dispatcher.json"))
            .unwrap();
    Python::attach(|py| {
        let expected = module(py, "json")
            .call_method1("loads", (template,))
            .unwrap();
        assert!(expected
            .eq(registry(py).call_method0("settings_block").unwrap())
            .unwrap());
    });
}

#[test]
fn dispatcher_recognition_handles_shell_quoting_and_rejects_extra_commands() {
    let _case = Case::new();
    let cases = [
        ("env 'CONDUCTOR_PYTHON=/runtime with spaces/python' '/tools with spaces/forge' hook SessionStart", Some("SessionStart")),
        ("CONDUCTOR_PYTHON=/runtime/python /tools/forge hook SessionStart", Some("SessionStart")),
        ("env X=1 /tools/forge hook SessionStart; echo bypass", None),
        ("env X=1 '/tools/forge hook SessionStart", None),
        ("env X=1 /tools/forge other SessionStart", None),
    ];
    Python::attach(|py| {
        for (command, expected) in cases {
            let actual = registry(py)
                .call_method1("resolve_dispatcher", (command,))
                .unwrap();
            match expected {
                Some(event) => assert!(actual.eq(event).unwrap(), "{command}"),
                None => assert!(actual.is_none(), "{command}"),
            }
        }
    });
}

#[test]
fn settings_block_wires_every_event_once_to_the_launcher() {
    let _case = Case::new();
    Python::attach(|py| {
        let registry = registry(py);
        let block = registry
            .call_method0("settings_block")
            .unwrap()
            .get_item("hooks")
            .unwrap();
        let block = block.cast::<PyDict>().unwrap();
        let keys = PyTuple::new(py, block.keys().iter()).unwrap();
        assert!(keys.eq(registry.getattr("EVENTS").unwrap()).unwrap());
        let launcher: String = registry.getattr("LAUNCHER").unwrap().extract().unwrap();
        for (event, groups) in block.iter() {
            assert_eq!(groups.len().unwrap(), 1);
            let hooks = groups.get_item(0).unwrap().get_item("hooks").unwrap();
            assert_eq!(hooks.len().unwrap(), 1);
            let hook = hooks.get_item(0).unwrap();
            let name: String = event.extract().unwrap();
            let command = hook.get_item("command").unwrap();
            assert!(command
                .eq(format!("$CLAUDE_PROJECT_DIR/{launcher} {name}"))
                .unwrap());
            let timeout = registry
                .call_method1("hooks_for", (&event,))
                .unwrap()
                .try_iter()
                .unwrap()
                .map(|spec| {
                    spec.unwrap()
                        .getattr("timeout")
                        .unwrap()
                        .extract::<i64>()
                        .unwrap()
                })
                .max()
                .unwrap();
            assert!(hook.get_item("timeout").unwrap().eq(timeout + 1).unwrap());
            assert!(registry
                .call_method1("resolve_dispatcher", (command,))
                .unwrap()
                .eq(event)
                .unwrap());
        }
    });
}

#[test]
fn launcher_is_tracked_and_executable() {
    let _case = Case::new();
    Python::attach(|py| {
        let launcher: String = registry(py).getattr("LAUNCHER").unwrap().extract().unwrap();
        let launcher = repo().join(launcher);
        assert!(launcher.is_file());
        assert!(launcher.metadata().unwrap().len() > 0);
        assert!(executable(py, &launcher));
        assert!(fs::read_to_string(launcher)
            .unwrap()
            .starts_with("#!/usr/bin/env python3"));
    });
}

#[test]
fn every_spec_has_a_runnable_body() {
    let _case = Case::new();
    Python::attach(|py| {
        let registry = registry(py);
        let adapters = module(py, "tooling.hooks.dispatch.adapters");
        let events = registry.getattr("EVENTS").unwrap();
        for spec in registry.getattr("HOOKS").unwrap().try_iter().unwrap() {
            let spec = spec.unwrap();
            assert!(events.contains(spec.getattr("event").unwrap()).unwrap());
            let adapter = spec.getattr("adapter").unwrap();
            let argv = spec.getattr("argv").unwrap();
            assert_ne!(adapter.is_truthy().unwrap(), argv.is_truthy().unwrap());
            if adapter.is_truthy().unwrap() {
                let name: String = adapter.extract().unwrap();
                assert!(adapters.getattr(name.as_str()).unwrap().is_callable());
            } else {
                let body: String = argv.get_item(0).unwrap().extract().unwrap();
                let body = repo().join("src").join(body);
                assert!(body.is_file());
                assert!(body.metadata().unwrap().len() > 0);
                assert!(executable(py, &body) || body.extension().is_some_and(|ext| ext == "py"));
            }
        }
    });
}

#[test]
fn names_are_unique() {
    let _case = Case::new();
    Python::attach(|py| {
        let names: Vec<String> = registry(py)
            .getattr("HOOKS")
            .unwrap()
            .try_iter()
            .unwrap()
            .map(|spec| spec.unwrap().getattr("name").unwrap().extract().unwrap())
            .collect();
        assert_eq!(names.len(), names.iter().collect::<HashSet<_>>().len());
    });
}

#[test]
fn every_live_command_resolves_to_a_registered_spec() {
    let _case = Case::new();
    Python::attach(|py| {
        let registry = registry(py);
        for command in LIVE_COMMANDS {
            assert!(
                !registry
                    .call_method1("resolve_legacy", (command,))
                    .unwrap()
                    .is_none(),
                "{command}"
            );
        }
        assert!(registry
            .call_method1(
                "resolve_legacy",
                ("$CLAUDE_PROJECT_DIR/.claude/hooks/nope.sh",)
            )
            .unwrap()
            .is_none());
    });
}

#[test]
fn natively_served_is_empty_by_default() {
    let mut case = Case::new();
    case.remove_env("FORGE_NATIVE_HOOKS");
    Python::attach(|py| {
        assert!(registry(py)
            .call_method0("natively_served")
            .unwrap()
            .eq(PyFrozenSet::empty(py).unwrap())
            .unwrap());
    });
}

#[test]
fn natively_served_parses_and_trims_the_env_var() {
    let mut case = Case::new();
    case.set_env("FORGE_NATIVE_HOOKS", " pre_bash ,, bash_write_targets");
    Python::attach(|py| {
        let expected = PyFrozenSet::new(py, ["pre_bash", "bash_write_targets"]).unwrap();
        assert!(registry(py)
            .call_method0("natively_served")
            .unwrap()
            .eq(expected)
            .unwrap());
    });
}

fn assert_empty_answers() {
    Python::attach(|py| {
        assert!(registry(py)
            .call_method0("native_answers")
            .unwrap()
            .eq(PyDict::new(py))
            .unwrap());
    });
}

#[test]
fn native_answers_is_empty_by_default() {
    let mut case = Case::new();
    case.remove_env("FORGE_NATIVE_ANSWERS");
    assert_empty_answers();
}

#[test]
fn native_answers_is_empty_when_the_env_var_is_blank() {
    let mut case = Case::new();
    case.set_env("FORGE_NATIVE_ANSWERS", "   ");
    assert_empty_answers();
}

#[test]
fn native_answers_parses_the_json_object() {
    let mut case = Case::new();
    let payload = r#"{"pre_bash":{"hookSpecificOutput":{"permissionDecision":"deny"}}}"#;
    case.set_env("FORGE_NATIVE_ANSWERS", payload);
    Python::attach(|py| {
        let expected = module(py, "json")
            .call_method1("loads", (payload,))
            .unwrap();
        assert!(registry(py)
            .call_method0("native_answers")
            .unwrap()
            .eq(expected)
            .unwrap());
    });
}

fn assert_invalid_answers(payload: &str, message: &str) {
    let mut case = Case::new();
    case.set_env("FORGE_NATIVE_ANSWERS", payload);
    Python::attach(|py| {
        let error = registry(py).call_method0("native_answers").unwrap_err();
        assert!(error.is_instance_of::<PyValueError>(py));
        assert!(error.to_string().contains(message));
    });
}

#[test]
fn native_answers_rejects_malformed_json() {
    assert_invalid_answers("{not json", "not valid JSON");
}

#[test]
fn native_answers_rejects_a_non_object_json_value() {
    assert_invalid_answers("[1, 2, 3]", "JSON object");
}

#[test]
fn matchers() {
    let _case = Case::new();
    Python::attach(|py| {
        let hooks: Vec<_> = registry(py)
            .getattr("HOOKS")
            .unwrap()
            .try_iter()
            .unwrap()
            .map(Result::unwrap)
            .collect();
        let find = |name: &str| {
            hooks
                .iter()
                .find(|spec| spec.getattr("name").unwrap().eq(name).unwrap())
                .unwrap()
        };
        let matches = |spec: &Bound<'_, PyAny>, tool: &str| {
            spec.call_method1("matches", (tool,))
                .unwrap()
                .is_truthy()
                .unwrap()
        };
        let mark = find("crg_gate_mark");
        assert!(matches(mark, "mcp__code-review-graph__locate_tool"));
        assert!(matches(mark, "mcp__code_review_graph__locate_tool"));
        assert!(!matches(mark, "Bash"));
        let edit = find("crg_gate_verify");
        assert!(matches(edit, "Edit"));
        assert!(matches(edit, "NotebookEdit"));
        assert!(!matches(edit, "Read"));
        for spec in &hooks {
            let matcher: String = spec.getattr("matcher").unwrap().extract().unwrap();
            if matcher.is_empty() || matcher == ".*" {
                assert!(matches(spec, "anything"));
            }
        }
    });
}
