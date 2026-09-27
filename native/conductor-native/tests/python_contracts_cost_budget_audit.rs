#![cfg(feature = "python-compat-tests")]
//! Rust-owned contracts for the Python cost-budget gate adapter.

#[path = "python_contracts/agent_comm_support.rs"]
#[allow(dead_code)]
mod comm_support;
#[path = "python_contracts/cost_contract_support.rs"]
#[allow(dead_code)]
mod fixture;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use fixture::{audit_payload, completed, equal, metric, patch_constant, patch_kwargs_result};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyCFunction, PyDict, PyList};
use serde_json::{json, Value};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::sync::{Arc, Mutex};
use support::{assert_error, module, path, AttrPatch, Case};

fn budget<'py>(py: Python<'py>) -> Bound<'py, PyAny> {
    module(py, "conductor.cost_budget_audit").into_any()
}

fn phase_result(
    py: Python<'_>,
    case: &Case,
    payload: Option<Value>,
    code: i32,
    stdout: Option<&str>,
    stderr: &str,
) -> Py<PyAny> {
    phase_result_with_args(
        py,
        case,
        payload,
        code,
        stdout,
        stderr,
        &["forge", "ledger", "audit"],
    )
}

fn phase_result_with_args(
    py: Python<'_>,
    case: &Case,
    payload: Option<Value>,
    code: i32,
    stdout: Option<&str>,
    stderr: &str,
    args: &[&str],
) -> Py<PyAny> {
    let subject = budget(py);
    let binary = case.write("forge", "#!/bin/sh\nexit 0\n");
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).unwrap();
    let binary_path = path(py, &binary);
    let _resolver = patch_constant(
        py,
        &subject,
        "resolve_forge_binary",
        &binary_path,
        &["root"],
    );
    let output = stdout
        .map(str::to_owned)
        .unwrap_or_else(|| payload.unwrap().to_string());
    let process = completed(py, &output, code, stderr, args);
    let _runner = patch_kwargs_result(py, &subject, "run_forge_ledger_audit", &process);
    subject
        .getattr("phase")
        .unwrap()
        .call1((path(py, case.root()),))
        .unwrap()
        .unbind()
}

fn phase_detail(result: &Bound<'_, PyAny>) -> String {
    result.getattr("detail").unwrap().extract().unwrap()
}

fn phase_ok(result: &Bound<'_, PyAny>) -> bool {
    result.getattr("ok").unwrap().extract().unwrap()
}

#[test]
fn test_pass_status_is_ok() {
    let case = Case::new();
    Python::attach(|py| {
        let result = phase_result(py, &case, Some(audit_payload("PASS", "PASS")), 0, None, "");
        let result = result.bind(py);
        assert!(phase_ok(result));
        assert!(result
            .getattr("name")
            .unwrap()
            .eq("cost-budget-audit")
            .unwrap());
        assert!(phase_detail(result).contains("PASS"));
    });
}

#[test]
fn test_ratchet_held_is_ok_but_not_pass() {
    let case = Case::new();
    Python::attach(|py| {
        let result = phase_result(
            py,
            &case,
            Some(audit_payload("RATCHET_HELD", "RATCHET_HELD")),
            0,
            None,
            "",
        );
        let result = result.bind(py);
        assert!(phase_ok(result));
        let status = result
            .getattr("evidence")
            .unwrap()
            .get_item("status")
            .unwrap();
        assert!(status.eq("RATCHET_HELD").unwrap());
        assert!(!status.eq("PASS").unwrap());
    });
}

#[test]
fn test_regression_status_is_not_ok() {
    let case = Case::new();
    Python::attach(|py| {
        let mut payload = audit_payload("REGRESSION", "RATCHET_HELD");
        payload["metrics"]["tokens_per_landed_pr"] =
            metric("REGRESSION", Some(500_000.0), Some(150_000.0));
        let result = phase_result(py, &case, Some(payload), 0, None, "");
        let result = result.bind(py);
        assert!(!phase_ok(result));
        assert!(phase_detail(result).contains("REGRESSION"));
        assert!(result
            .getattr("evidence")
            .unwrap()
            .get_item("metrics")
            .unwrap()
            .get_item("tokens_per_landed_pr")
            .unwrap()
            .get_item("status")
            .unwrap()
            .eq("REGRESSION")
            .unwrap());
    });
}

#[test]
fn test_no_baseline_status_is_ok() {
    let case = Case::new();
    Python::attach(|py| {
        let result = phase_result(
            py,
            &case,
            Some(audit_payload("NO_BASELINE", "NO_BASELINE")),
            0,
            None,
            "",
        );
        let result = result.bind(py);
        assert!(phase_ok(result));
        assert!(phase_detail(result).contains("NO_BASELINE"));
    });
}

#[test]
fn test_no_data_exit_code_is_ok_with_status_in_detail() {
    let case = Case::new();
    Python::attach(|py| {
        let result = phase_result(py, &case, Some(json!({})), 3, None, "window has zero rows");
        let result = result.bind(py);
        assert!(phase_ok(result));
        assert!(phase_detail(result).contains("NO_DATA"));
        let evidence = result.getattr("evidence").unwrap();
        assert!(evidence.get_item("status").unwrap().eq("NO_DATA").unwrap());
        assert!(phase_detail(result).contains("window has zero rows"));
        assert!(evidence
            .get_item("message")
            .unwrap()
            .eq("window has zero rows")
            .unwrap());
    });
}

#[test]
fn test_no_data_falls_back_to_stdout_when_stderr_is_empty() {
    let case = Case::new();
    Python::attach(|py| {
        let result = phase_result_with_args(
            py,
            &case,
            None,
            3,
            Some("stdout says zero rows too"),
            "",
            &["forge"],
        );
        let result = result.bind(py);
        assert!(phase_ok(result));
        assert!(result
            .getattr("evidence")
            .unwrap()
            .get_item("message")
            .unwrap()
            .eq("stdout says zero rows too")
            .unwrap());
    });
}

#[test]
fn test_single_regression_metric_makes_phase_not_ok() {
    let case = Case::new();
    Python::attach(|py| {
        let mut payload = audit_payload("REGRESSION", "PASS");
        payload["metrics"]["resend_bytes_per_session"] =
            metric("REGRESSION", Some(20_000_000.0), Some(8_800_000.0));
        let result = phase_result(py, &case, Some(payload), 0, None, "");
        assert!(!phase_ok(result.bind(py)));
    });
}

#[test]
fn test_five_metric_payload_with_no_baseline_metric_is_ok() {
    let case = Case::new();
    Python::attach(|py| {
        let mut payload = audit_payload("NO_BASELINE", "RATCHET_HELD");
        payload["metrics"]["cheap_tier_rework_rate"] = metric("NO_BASELINE", None, None);
        assert_eq!(payload["metrics"].as_object().unwrap().len(), 5);
        let result = phase_result(py, &case, Some(payload), 0, None, "");
        let result = result.bind(py);
        assert!(phase_ok(result));
        assert!(phase_detail(result).contains("cheap_tier_rework_rate=NO_BASELINE"));
        let metric = result
            .getattr("evidence")
            .unwrap()
            .get_item("metrics")
            .unwrap()
            .get_item("cheap_tier_rework_rate")
            .unwrap();
        assert!(metric.get_item("value").unwrap().is_none());
        assert!(metric.get_item("baseline").unwrap().is_none());
    });
}

#[test]
fn test_missing_forge_binary_raises() {
    let case = Case::new();
    Python::attach(|py| {
        let subject = budget(py);
        let _patch = patch_constant(
            py,
            &subject,
            "resolve_forge_binary",
            py.None().bind(py),
            &["root"],
        );
        let error = subject
            .getattr("phase")
            .unwrap()
            .call1((path(py, case.root()),))
            .unwrap_err();
        assert_error(
            py,
            error,
            &subject.getattr("CostBudgetAuditError").unwrap(),
            "no forge binary",
        );
    });
}

#[test]
fn test_malformed_json_raises() {
    let case = Case::new();
    Python::attach(|py| {
        let subject = budget(py);
        let binary = case.write("forge", "#!/bin/sh\nexit 0\n");
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).unwrap();
        let _resolver = patch_constant(
            py,
            &subject,
            "resolve_forge_binary",
            &path(py, &binary),
            &["root"],
        );
        let completed = completed(py, "not json", 0, "", &["forge"]);
        let _runner = patch_kwargs_result(py, &subject, "run_forge_ledger_audit", &completed);
        let error = subject
            .getattr("phase")
            .unwrap()
            .call1((path(py, case.root()),))
            .unwrap_err();
        assert_error(
            py,
            error,
            &subject.getattr("CostBudgetAuditError").unwrap(),
            "no JSON",
        );
    });
}

#[test]
fn test_run_forge_ledger_audit_builds_the_expected_command() {
    let case = Case::new();
    Python::attach(|py| {
        let captured = Arc::new(Mutex::new(None::<Py<PyAny>>));
        let record = Arc::clone(&captured);
        let stub = PyCFunction::new_closure(py, None, None, move |args, kwargs| {
            let named = kwargs.and_then(|kw| kw.get_item("command").ok().flatten());
            let command = match (args.len(), named) {
                (1, None) => args.get_item(0)?,
                (0, Some(value)) => value,
                (0, None) => return Err(pyo3::exceptions::PyTypeError::new_err("missing command")),
                (1, Some(_)) => {
                    return Err(pyo3::exceptions::PyTypeError::new_err(
                        "multiple values for command",
                    ))
                }
                _ => {
                    return Err(pyo3::exceptions::PyTypeError::new_err(
                        "too many positional arguments",
                    ))
                }
            };
            *record.lock().unwrap() = Some(command.unbind());
            Ok::<_, PyErr>(
                completed(
                    args.py(),
                    &audit_payload("PASS", "PASS").to_string(),
                    0,
                    "",
                    &["forge", "ledger", "audit"],
                )
                .unbind(),
            )
        })
        .unwrap();
        let subprocess = py.import("subprocess").unwrap();
        let _patch = AttrPatch::replace(subprocess.as_any(), "run", stub.as_any());
        let subject = budget(py);
        let kwargs = PyDict::new(py);
        kwargs
            .set_item(
                "forge_binary",
                path(py, std::path::Path::new("/usr/local/bin/forge")),
            )
            .unwrap();
        kwargs
            .set_item("baseline", path(py, &case.root().join("baseline.json")))
            .unwrap();
        kwargs
            .set_item("ledger_root", path(py, &case.root().join("ledger")))
            .unwrap();
        kwargs.set_item("window_days", 14).unwrap();
        kwargs.set_item("record", true).unwrap();
        subject
            .getattr("run_forge_ledger_audit")
            .unwrap()
            .call((), Some(&kwargs))
            .unwrap();
        let command = captured.lock().unwrap().as_ref().unwrap().clone_ref(py);
        let command = command.bind(py).cast::<PyList>().unwrap();
        let first = command.get_slice(0, 3);
        equal(
            first.as_any(),
            PyList::new(py, ["/usr/local/bin/forge", "ledger", "audit"])
                .unwrap()
                .as_any(),
        );
        for token in ["--record", "--window-days", "14", "--ledger-root"] {
            assert!(command.contains(token).unwrap());
        }
    });
}

#[test]
fn test_default_baseline_path_is_ledger_relative() {
    let case = Case::new();
    Python::attach(|py| {
        let subject = budget(py);
        let actual = subject
            .getattr("default_baseline_path")
            .unwrap()
            .call1((path(py, case.root()),))
            .unwrap();
        equal(
            &actual,
            &path(py, &case.root().join("ledger/cost_budget_baseline.json")),
        );
    });
}
