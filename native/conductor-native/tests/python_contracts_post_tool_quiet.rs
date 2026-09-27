#![cfg(feature = "python-compat-tests")]
//! Rust-owned contracts for the legacy PostToolUse quiet Python binding.

#[path = "python_contracts/agent_comm_support.rs"]
#[allow(dead_code)]
mod comm_support;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use comm_support::{buffer_text, capture, py_json};
use pyo3::prelude::*;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use support::{module, path, AttrPatch, Case};

fn fixture(name: &str) -> Value {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../src/tooling/hooks/claude/fixtures/test_post_tool_quiet");
    serde_json::from_str(&fs::read_to_string(root.join(name)).unwrap()).unwrap()
}

fn output<'py>(py: Python<'py>, name: &str) -> Bound<'py, pyo3::types::PyAny> {
    module(py, "tooling.hooks.claude.post_tool_quiet")
        .getattr("hook_output")
        .unwrap()
        .call1((py_json(py, fixture(name)),))
        .unwrap()
}

#[test]
fn python_binding_returns_native_read_resume_marker() {
    let _case = Case::new();
    Python::attach(|py| {
        let result = output(py, "read_over_cap.json");
        let content: String = result
            .get_item("hookSpecificOutput")
            .unwrap()
            .get_item("updatedToolOutput")
            .unwrap()
            .get_item("file")
            .unwrap()
            .get_item("content")
            .unwrap()
            .extract()
            .unwrap();
        assert!(content.contains("Read(offset=61, limit=...)"));
    });
}

#[test]
fn python_binding_reports_unrecognized_shape() {
    let _case = Case::new();
    Python::attach(|py| {
        let (stderr, _capture) = capture(py, "stderr");
        let result = output(py, "malformed_shape.json");
        let expected = py_json(
            py,
            serde_json::json!({"hookSpecificOutput": {"hookEventName": "PostToolUse"}}),
        );
        assert!(result.eq(expected).unwrap());
        assert!(buffer_text(&stderr).contains("unrecognized tool_response shape"));
    });
}

#[test]
fn shared_bash_configuration_is_used() {
    let case = Case::new();
    let save_dir: PathBuf = case.mkdir("saved");
    Python::attach(|py| {
        let bash = module(py, "_bash_quiet");
        let _save = AttrPatch::replace(bash.as_any(), "SAVE_DIR", &path(py, &save_dir));
        let result = output(py, "grep_over_cap.json");
        let detail: String = result
            .get_item("hookSpecificOutput")
            .unwrap()
            .get_item("updatedToolOutput")
            .unwrap()
            .extract()
            .unwrap();
        assert!(detail.contains("full output:"));
        assert_eq!(fs::read_dir(&save_dir).unwrap().count(), 1);
    });
}
