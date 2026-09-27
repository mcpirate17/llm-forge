//! Rust fixtures and scoped Python patches for dispatch runner contracts.

use crate::comm_support::{bind_signature, py_json, signature};
use crate::support::{module, path, Case};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyBytes, PyCFunction, PyDict, PyModule};
use serde_json::{json, Value};
use std::path::Path;

pub const PAYLOAD: &str =
    r#"{"session_id": "t", "tool_name": "Bash", "tool_input": {"command": "echo hi"}}"#;

pub fn case() -> Case {
    let mut case = Case::new();
    for name in [
        "PROJECT_DIR",
        "CLAUDE_PROJECT_DIR",
        "FORGE_NATIVE_HOOKS",
        "FORGE_NATIVE_ANSWERS",
        "HOOK_DISPATCH_TRACE",
    ] {
        case.remove_env(name);
    }
    case
}

pub fn runner(py: Python<'_>) -> Bound<'_, PyModule> {
    module(py, "tooling.hooks.dispatch.runner")
}

pub fn adapters(py: Python<'_>) -> Bound<'_, PyModule> {
    module(py, "tooling.hooks.dispatch.adapters")
}

pub fn entry(py: Python<'_>) -> Bound<'_, PyModule> {
    module(py, "tooling.hooks.dispatch.__main__")
}

pub fn spec<'py>(
    py: Python<'py>,
    name: &str,
    adapter: Option<&str>,
    argv: &[&str],
    timeout: i32,
) -> Bound<'py, PyAny> {
    let kwargs = PyDict::new(py);
    kwargs.set_item("event", "PreToolUse").unwrap();
    kwargs.set_item("matcher", "Bash").unwrap();
    kwargs.set_item("timeout", timeout).unwrap();
    kwargs.set_item("legacy_command", "x").unwrap();
    if let Some(adapter) = adapter {
        kwargs.set_item("adapter", adapter).unwrap();
    }
    if !argv.is_empty() {
        kwargs
            .set_item("argv", pyo3::types::PyTuple::new(py, argv).unwrap())
            .unwrap();
    }
    module(py, "tooling.hooks.dispatch.registry")
        .getattr("HookSpec")
        .unwrap()
        .call((name,), Some(&kwargs))
        .unwrap()
}

pub fn context<'py>(
    py: Python<'py>,
    event: &str,
    payload: Value,
    root: &Path,
) -> Bound<'py, PyAny> {
    let json = module(py, "json");
    let raw: String = json
        .getattr("dumps")
        .unwrap()
        .call1((py_json(py, payload),))
        .unwrap()
        .extract()
        .unwrap();
    runner(py)
        .getattr("build_context")
        .unwrap()
        .call1((event, PyBytes::new(py, raw.as_bytes()), path(py, root)))
        .unwrap()
}

pub fn standard_context<'py>(py: Python<'py>, root: &Path) -> Bound<'py, PyAny> {
    runner(py)
        .getattr("build_context")
        .unwrap()
        .call1((
            "PreToolUse",
            PyBytes::new(py, PAYLOAD.as_bytes()),
            path(py, root),
        ))
        .unwrap()
}

pub fn payload<'py>(py: Python<'py>) -> Bound<'py, PyAny> {
    py_json(py, serde_json::from_str(PAYLOAD).unwrap())
}

pub fn output<'py>(py: Python<'py>, value: Value) -> Bound<'py, PyAny> {
    py_json(py, value)
}

pub fn outcome_names(value: &Bound<'_, PyAny>) -> Vec<String> {
    value
        .try_iter()
        .unwrap()
        .map(|item| item.unwrap().getattr("name").unwrap().extract().unwrap())
        .collect()
}

/// Match a Python `lambda ctx: ...` or `lambda event: ...` call contract.
pub fn callback<'py, F>(py: Python<'py>, argument: &str, body: F) -> Bound<'py, PyCFunction>
where
    F: Fn(Python<'_>, Py<PyAny>) -> PyResult<Py<PyAny>> + Send + Sync + 'static,
{
    let sig = signature(py, &[argument], &[]);
    let argument = argument.to_owned();
    PyCFunction::new_closure(py, None, None, move |args, kwargs| {
        let bound = bind_signature(&sig, args, kwargs)?;
        let value = bound
            .getattr("arguments")?
            .get_item(argument.as_str())?
            .unbind();
        body(args.py(), value)
    })
    .unwrap()
}

pub fn no_args<'py, F>(py: Python<'py>, body: F) -> Bound<'py, PyCFunction>
where
    F: Fn(Python<'_>) -> PyResult<Py<PyAny>> + Send + Sync + 'static,
{
    PyCFunction::new_closure(py, None, None, move |args, kwargs| {
        if !args.is_empty() || kwargs.is_some_and(|kw| !kw.is_empty()) {
            return Err(pyo3::exceptions::PyTypeError::new_err(
                "expected no arguments",
            ));
        }
        body(args.py())
    })
    .unwrap()
}

/// Python's `monkeypatch.setattr(..., raising=False)`, restored on drop.
pub struct OptionalPatch {
    target: Py<PyAny>,
    name: String,
    previous: Option<Py<PyAny>>,
}

impl OptionalPatch {
    pub fn new(target: &Bound<'_, PyAny>, name: &str, replacement: &Bound<'_, PyAny>) -> Self {
        let previous = target.getattr(name).ok().map(Bound::unbind);
        target.setattr(name, replacement).unwrap();
        Self {
            target: target.clone().unbind(),
            name: name.to_owned(),
            previous,
        }
    }
}

impl Drop for OptionalPatch {
    fn drop(&mut self) {
        Python::attach(|py| {
            if let Some(previous) = &self.previous {
                self.target
                    .bind(py)
                    .setattr(self.name.as_str(), previous.bind(py))
                    .unwrap();
            } else {
                self.target.bind(py).delattr(self.name.as_str()).unwrap();
            }
        });
    }
}

pub struct IoRestore {
    stdin: Py<PyAny>,
    stdout: Py<PyAny>,
}

impl IoRestore {
    pub fn new(py: Python<'_>) -> Self {
        let sys = module(py, "sys");
        Self {
            stdin: sys.getattr("stdin").unwrap().unbind(),
            stdout: sys.getattr("stdout").unwrap().unbind(),
        }
    }
}

impl Drop for IoRestore {
    fn drop(&mut self) {
        Python::attach(|py| {
            let sys = module(py, "sys");
            sys.setattr("stdin", self.stdin.bind(py)).unwrap();
            sys.setattr("stdout", self.stdout.bind(py)).unwrap();
        });
    }
}

pub fn deny() -> Value {
    json!({"hookSpecificOutput": {"permissionDecision": "deny", "permissionDecisionReason": "r"}})
}
