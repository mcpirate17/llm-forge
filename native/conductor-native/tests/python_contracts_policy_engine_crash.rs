#![cfg(feature = "python-compat-tests")]
//! Rust-owned policy-engine crash reporting and backlog contracts.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::prelude::*;
use pyo3::types::{PyAny, PyBytes, PyDict};
use std::ffi::OsString;
use std::fs;
use std::path::Path;
use std::process::Command;
use support::{attr_text, module, path, text, AttrPatch, Case};

// Data for the Python traceback interpreter. The assertions and test driver live in Rust.
const RAISE_FIXTURE: &str =
    "def inner():\n    raise exc_type(message)\ndef outer():\n    inner()\n";

fn raised<'py>(py: Python<'py>, class: &str, message: &str) -> Bound<'py, PyAny> {
    let builtins = module(py, "builtins");
    let globals = PyDict::new(py);
    globals
        .set_item("exc_type", builtins.getattr(class).unwrap())
        .unwrap();
    globals.set_item("message", message).unwrap();
    let code = builtins
        .getattr("compile")
        .unwrap()
        .call1((RAISE_FIXTURE, "test_policy_engine_crash.py", "exec"))
        .unwrap();
    builtins
        .getattr("exec")
        .unwrap()
        .call1((code, &globals))
        .unwrap();
    globals
        .get_item("outer")
        .unwrap()
        .unwrap()
        .call0()
        .unwrap_err()
        .into_value(py)
        .into_bound(py)
        .into_any()
}

fn crash<'py>(py: Python<'py>, name: &str, exception: &Bound<'py, PyAny>) -> Bound<'py, PyAny> {
    module(py, "conductor.candidate_review.engine")
        .getattr("_crash_result")
        .unwrap()
        .call1((name, exception))
        .unwrap()
}

fn first_finding<'py>(result: &Bound<'py, PyAny>) -> Bound<'py, PyAny> {
    result.getattr("findings").unwrap().get_item(0).unwrap()
}

fn trace(finding: &Bound<'_, PyAny>) -> String {
    finding
        .getattr("evidence")
        .unwrap()
        .get_item("traceback")
        .unwrap()
        .extract()
        .unwrap()
}

#[test]
fn crashed_check_names_the_line_it_raised_on() {
    let _case = Case::new();
    Python::attach(|py| {
        let exception = raised(
            py,
            "TypeError",
            "Object of type bytes is not JSON serializable",
        );
        let result = crash(py, "equivalence-probe", &exception);
        assert_eq!(attr_text(&result, "status"), "error");
        let finding = first_finding(&result);
        assert_eq!(attr_text(&finding, "severity"), "critical");
        let message = attr_text(&finding, "message");
        assert!(message.contains("TypeError: Object of type bytes is not JSON serializable"));
        assert!(
            message.contains("test_policy_engine_crash.py:"),
            "raise site missing: {message}"
        );
        assert!(
            message.contains("in inner"),
            "deepest frame missing: {message}"
        );
    });
}

#[test]
fn frames_reach_receipt_even_though_message_holds_one_line() {
    let _case = Case::new();
    Python::attach(|py| {
        let finding = first_finding(&crash(py, "probe", &raised(py, "RuntimeError", "boom")));
        let traceback = trace(&finding);
        assert!(traceback.contains("RuntimeError: boom"));
        assert!(traceback.contains("in outer") && traceback.contains("in inner"));
        let cap: usize = module(py, "conductor.candidate_review.engine")
            .getattr("CRASH_TRACEBACK_CHARS")
            .unwrap()
            .extract()
            .unwrap();
        assert!(traceback.len() <= cap);
    });
}

#[test]
fn long_traceback_is_truncated_to_frames_naming_defect() {
    let _case = Case::new();
    Python::attach(|py| {
        let engine = module(py, "conductor.candidate_review.engine");
        let cap = 40usize.into_pyobject(py).unwrap().into_any();
        let _patch = AttrPatch::replace(engine.as_any(), "CRASH_TRACEBACK_CHARS", &cap);
        let finding = first_finding(&crash(py, "probe", &raised(py, "RuntimeError", "boom")));
        let traceback = trace(&finding);
        assert_eq!(traceback.len(), 40);
        assert!(traceback.ends_with("RuntimeError: boom\n"));
    });
}

#[test]
fn crash_finding_carries_no_path_or_line() {
    let _case = Case::new();
    Python::attach(|py| {
        let finding = first_finding(&crash(py, "probe", &raised(py, "RuntimeError", "x")));
        assert!(finding.getattr("path").unwrap().is_none());
        assert!(finding.getattr("line").unwrap().is_none());
    });
}

#[test]
fn unserializable_summary_is_reported_not_fatal() {
    let case = Case::new();
    let findings_dir = case.root().join("gate_findings");
    Python::attach(|py| {
        let ledger = module(py, "conductor.slop_ledger");
        let new_path = path(py, &findings_dir);
        let _path_patch = AttrPatch::replace(ledger.as_any(), "GATE_FINDINGS", &new_path);
        let stderr = module(py, "io")
            .getattr("StringIO")
            .unwrap()
            .call0()
            .unwrap();
        let _stderr_patch = AttrPatch::replace(module(py, "sys").as_any(), "stderr", &stderr);
        let incomplete = PyDict::new(py);
        incomplete
            .set_item("stderr_tail", PyBytes::new(py, b"killed mid-import"))
            .unwrap();
        let summary = PyDict::new(py);
        summary.set_item("incomplete", vec![incomplete]).unwrap();
        module(py, "conductor.candidate_review.equivalence_probe_check")
            .getattr("_record_for_backlog")
            .unwrap()
            .call1((summary, py.None()))
            .unwrap();
        assert!(text(&stderr.call_method0("getvalue").unwrap()).contains("not recorded"));
    });
    assert!(
        fs::read_dir(&findings_dir).unwrap().next().is_none(),
        "no .json or .part may survive"
    );
}

#[test]
fn crash_help_names_flag_gate_actually_accepts() {
    let _case = Case::new();
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../src/conductor/candidate_review/equivalence_probe_check.py");
    let content = fs::read_to_string(source).unwrap();
    assert!(content.contains("python -m conductor.slop_gate --module <module>"));
    assert!(!content.contains("--only"));
    let python = std::env::var_os("PYO3_PYTHON")
        .or_else(|| std::env::var_os("PYTHON"))
        .unwrap_or_else(|| OsString::from("python3"));
    let source_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../src");
    let mut search_path = vec![source_root];
    if let Some(inherited) = std::env::var_os("PYTHONPATH") {
        search_path.extend(std::env::split_paths(&inherited));
    }
    let output = Command::new(python)
        .args(["-m", "conductor.slop_gate", "--help"])
        .env("PYTHONPATH", std::env::join_paths(search_path).unwrap())
        .output()
        .expect("run slop_gate help");
    assert!(
        output.status.success(),
        "slop_gate help failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("--module"));
    assert!(!stdout.contains("--only"));
}
