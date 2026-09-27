#![cfg(feature = "python-compat-tests")]
//! Rust-owned PyO3 contracts for provider-hook provisioning.

#[path = "python_contracts/agent_comm_support.rs"]
#[allow(dead_code)]
mod comm_support;
#[path = "python_contracts/harness_provisioning_support.rs"]
mod harness_support;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use comm_support::{bind_signature, json_value};
use harness_support::{config, hp, importable, pi, provider_path, signature_with_kwargs};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyCFunction, PyDict, PyList, PyTuple};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use support::{module, path, AttrPatch, Case};

fn executable(py: Python<'_>) -> std::path::PathBuf {
    let value: String = module(py, "sys")
        .getattr("executable")
        .unwrap()
        .extract()
        .unwrap();
    value.into()
}

#[test]
fn provider_bootstrap_is_idempotent_and_check_catches_drift() {
    for provider in ["codex", "qwen", "grok"] {
        let case = Case::new();
        fs::create_dir(case.root().join(".git")).unwrap();
        Python::attach(|py| {
            let _importable = importable(py);
            let python = executable(py);
            let conf = config(py, case.root(), provider, Some(&python));
            let init = pi(py);
            assert!(init
                .getattr("run")
                .unwrap()
                .call1((&conf,))
                .unwrap()
                .eq(0)
                .unwrap());
            assert!(!init
                .getattr("plan")
                .unwrap()
                .call1((&conf,))
                .unwrap()
                .getattr("changed")
                .unwrap()
                .is_truthy()
                .unwrap());
            let updates = PyDict::new(py);
            updates.set_item("check", true).unwrap();
            let options = PyDict::new(py);
            options.set_item("update", updates).unwrap();
            let checked = conf.call_method("model_copy", (), Some(&options)).unwrap();
            assert!(init
                .getattr("run")
                .unwrap()
                .call1((checked,))
                .unwrap()
                .eq(0)
                .unwrap());
            let location = provider_path(py, provider, case.root());
            let mut payload: Value = serde_json::from_slice(&fs::read(&location).unwrap()).unwrap();
            let event: String = module(py, "conductor.hook_installer")
                .getattr("PROVIDERS")
                .unwrap()
                .get_item(provider)
                .unwrap()
                .getattr("event")
                .unwrap()
                .extract()
                .unwrap();
            payload["hooks"][&event] = json!([]);
            fs::write(location, payload.to_string()).unwrap();
            let errors = hp(py)
                .getattr("check_provider")
                .unwrap()
                .call1((provider, path(py, &python), path(py, case.root())))
                .unwrap();
            assert!(errors.cast::<PyList>().is_ok());
            assert!(!errors.is_empty().unwrap());
            assert!(!case.root().join(".claude/settings.json").exists());
        });
    }
}

#[test]
fn codex_wires_explicit_protocol_root_and_interpreter_with_spaces() {
    let case = Case::new();
    Python::attach(|py| {
        let python = Path::new("/runtime with spaces/bin/python");
        let hooks = hp(py)
            .getattr("provider_hooks")
            .unwrap()
            .call1(("codex", path(py, python), path(py, case.root())))
            .unwrap();
        let command = hooks
            .get_item("PreToolUse")
            .unwrap()
            .get_item(0)
            .unwrap()
            .get_item("hooks")
            .unwrap()
            .get_item(0)
            .unwrap()
            .get_item("command")
            .unwrap();
        let split = module(py, "shlex")
            .getattr("split")
            .unwrap()
            .call1((command,))
            .unwrap();
        let expected = PyList::new(
            py,
            [
                python.to_str().unwrap(),
                "-m",
                "tooling.hooks.dispatch",
                "PreToolUse",
                "--project-dir",
                case.root().to_str().unwrap(),
                "--protocol",
                "codex",
            ],
        )
        .unwrap();
        assert!(split.eq(expected).unwrap());
    });
}

#[test]
fn grok_uses_first_prompt_and_preserves_foreign_hooks() {
    let case = Case::new();
    Python::attach(|py| {
        let location = provider_path(py, "grok", case.root());
        fs::create_dir_all(location.parent().unwrap()).unwrap();
        let foreign = json!({"hooks":[{"type":"command","command":"keep-me"}]});
        fs::write(
            &location,
            json!({"hooks":{"UserPromptSubmit":[foreign]}}).to_string(),
        )
        .unwrap();
        let conf = config(py, case.root(), "grok", None);
        let action = pi(py)
            .getattr("_provider_actions")
            .unwrap()
            .call1((conf,))
            .unwrap()
            .get_item(0)
            .unwrap();
        let after: String = action.getattr("after").unwrap().extract().unwrap();
        let payload: Value = serde_json::from_str(&after).unwrap();
        assert!(payload["hooks"]["UserPromptSubmit"]
            .as_array()
            .unwrap()
            .contains(&foreign));
        assert!(payload["hooks"].get("SessionStart").is_none());
        assert!(after.contains("--once-per-session"));
        let capabilities: String = hp(py)
            .getattr("CAPABILITIES")
            .unwrap()
            .get_item("grok")
            .unwrap()
            .extract()
            .unwrap();
        assert!(capabilities.contains("tool guards are not provisioned"));
    });
}

#[test]
fn check_only_imports_adapters_without_starting_services() {
    let case = Case::new();
    Python::attach(|py| {
        let location = provider_path(py, "qwen", case.root());
        fs::create_dir_all(location.parent().unwrap()).unwrap();
        let python = executable(py);
        let hooks = hp(py)
            .getattr("provider_hooks")
            .unwrap()
            .call1(("qwen", path(py, &python), path(py, case.root())))
            .unwrap();
        fs::write(&location, json!({"hooks":json_value(&hooks)}).to_string()).unwrap();
        let calls = PyList::empty(py);
        let recorded = calls.clone().unbind();
        let expected = signature_with_kwargs(py, "argv");
        let completed = module(py, "subprocess")
            .getattr("CompletedProcess")
            .unwrap()
            .unbind();
        let callback =
            PyCFunction::new_closure(py, None, None, move |args, kwargs| -> PyResult<Py<PyAny>> {
                let bound = bind_signature(&expected, args, kwargs)?;
                let argv = bound.getattr("arguments")?.get_item("argv")?;
                recorded.bind(args.py()).append(&argv)?;
                Ok(completed.bind(args.py()).call1((argv, 0, "", ""))?.unbind())
            })
            .unwrap();
        let subprocess = hp(py).getattr("subprocess").unwrap();
        let _patch = AttrPatch::replace(&subprocess, "run", callback.as_any());
        let errors = hp(py)
            .getattr("check_provider")
            .unwrap()
            .call1(("qwen", path(py, &python), path(py, case.root())))
            .unwrap();
        assert!(errors.eq(PyList::empty(py)).unwrap());
        assert_eq!(calls.len(), 1);
        let argv = calls.get_item(0).unwrap();
        assert!(argv.get_item(1).unwrap().eq("-c").unwrap());
        let import: String = argv.get_item(2).unwrap().extract().unwrap();
        assert!(import.starts_with("import conductor.a2a_session_start;"));
    });
}

#[test]
fn selected_interpreter_is_bound_in_native_commands() {
    let case = Case::new();
    let binary = case.root().join("tool directory/forge");
    let python = case.root().join("virtual environment/bin/python");
    Python::attach(|py| {
        let value: String = pi(py)
            .getattr("render_settings")
            .unwrap()
            .call1((py.None(), false, path(py, &binary), path(py, &python)))
            .unwrap()
            .extract()
            .unwrap();
        let settings: Value = serde_json::from_str(&value).unwrap();
        let hooks = settings["hooks"].as_object().unwrap();
        for (event, groups) in hooks {
            let command = groups[0]["hooks"][0]["command"].as_str().unwrap();
            let split: Vec<String> = module(py, "shlex")
                .getattr("split")
                .unwrap()
                .call1((command,))
                .unwrap()
                .extract()
                .unwrap();
            assert_eq!(
                split,
                [
                    "env",
                    &format!("CONDUCTOR_PYTHON={}", python.display()),
                    binary.to_str().unwrap(),
                    "hook",
                    event
                ]
            );
        }
    });
}

#[test]
fn provider_selection_defaults_to_claude_and_all_is_explicit() {
    let case = Case::new();
    Python::attach(|py| {
        let root = case.root().to_str().unwrap();
        let parse = pi(py).getattr("parse_args").unwrap();
        let defaults = parse
            .call1((vec![root],))
            .unwrap()
            .getattr("providers")
            .unwrap();
        assert!(defaults.eq(PyTuple::new(py, ["claude"]).unwrap()).unwrap());
        let selected = parse
            .call1((vec![root, "--provider", "all"],))
            .unwrap()
            .getattr("providers")
            .unwrap();
        let actual: BTreeSet<String> = selected
            .cast::<PyTuple>()
            .unwrap()
            .iter()
            .map(|item| item.extract().unwrap())
            .collect();
        let caps = hp(py).getattr("CAPABILITIES").unwrap();
        let expected: BTreeSet<String> = caps
            .cast::<PyDict>()
            .unwrap()
            .iter()
            .map(|(key, _)| key.extract().unwrap())
            .collect();
        assert_eq!(actual, expected);
    });
}
