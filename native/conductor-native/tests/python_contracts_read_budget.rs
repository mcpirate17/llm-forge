#![cfg(feature = "python-compat-tests")]
//! Legacy read-budget entrypoints retain native accounting and CLI advice.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::prelude::*;
use pyo3::types::PyTuple;
use serde_json::{json, Value};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use support::{module, path, Case};

fn hook_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../src/tooling/hooks/agent")
        .canonicalize()
        .unwrap()
}

#[test]
fn python_binding_counts_and_tallies_in_native_code() {
    let case = Case::new();
    Python::attach(|py| {
        module(py, "sys")
            .getattr("path")
            .unwrap()
            .call_method1("insert", (0, hook_dir().to_str().unwrap()))
            .unwrap();
        let budget = module(py, "read_budget");
        let payload = module(py, "json")
            .call_method1(
                "loads",
                (json!({"a": "xx", "b": ["yyy", "z"]}).to_string(),),
            )
            .unwrap();
        assert!(budget
            .call_method1("response_chars", (payload,))
            .unwrap()
            .eq(6)
            .unwrap());
        for (tokens, expected) in [(5, [0, 5]), (7, [5, 12])] {
            let actual = budget
                .call_method1("tally", (path(py, case.root()), "session", tokens))
                .unwrap();
            assert!(actual.eq(PyTuple::new(py, expected).unwrap()).unwrap());
        }
    });
}

#[test]
fn legacy_cli_emits_native_advice() {
    let case = Case::new();
    let python: String = Python::attach(|py| {
        module(py, "sys")
            .getattr("executable")
            .unwrap()
            .extract()
            .unwrap()
    });
    let mut child = Command::new(python)
        .arg(hook_dir().join("read_budget.py"))
        .env("CRG_GATE_STATE_DIR", case.root())
        .env("READ_BUDGET_STEP_TOKENS", "50")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let payload = json!({"session_id": "s", "tool_response": "x".repeat(400)}).to_string();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(value["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap()
        .contains("READ BUDGET"));
}
