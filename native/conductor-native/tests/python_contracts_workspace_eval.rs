#![cfg(feature = "python-compat-tests")]
//! Workspace evaluation contracts, preserving offline and receipt-backed outcomes.

#[path = "python_contracts/hook_matrix_fixture.rs"]
mod hook_matrix_fixture;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;
#[path = "python_contracts/workspace_source_fixture.rs"]
mod workspace_source_fixture;

use hook_matrix_fixture::HookMatrix;
use pyo3::prelude::*;
use pyo3::types::PyDict;
use serde_json::{json, Value};
use std::fs;
use support::{attr_text, module, path, text, Case};

fn evaluate_with_receipt<'py>(
    py: Python<'py>,
    eval: &Bound<'py, pyo3::types::PyModule>,
    receipt: &std::path::Path,
    offline: bool,
) -> Bound<'py, pyo3::types::PyAny> {
    let kwargs = PyDict::new(py);
    kwargs
        .set_item("runtime_receipt", path(py, receipt))
        .unwrap();
    if offline {
        kwargs.set_item("live", false).unwrap();
    }
    eval.getattr("evaluate")
        .unwrap()
        .call((), Some(&kwargs))
        .unwrap()
}

fn offline_result<'py>(
    py: Python<'py>,
    eval: &Bound<'py, pyo3::types::PyModule>,
    repo: &std::path::Path,
) -> Bound<'py, pyo3::types::PyAny> {
    let kwargs = PyDict::new(py);
    kwargs.set_item("live", false).unwrap();
    eval.getattr("evaluate")
        .unwrap()
        .call((path(py, repo),), Some(&kwargs))
        .unwrap()
}

#[test]
fn offline_eval_is_honestly_not_ready() {
    let mut case = Case::new();
    Python::attach(|py| {
        let hooks = HookMatrix::new(py, &mut case);
        workspace_source_fixture::install(py, hooks.root());
        let eval = module(py, "conductor.workspace_eval");
        let matrix = module(py, "conductor.workspace_runtime_matrix");
        let result = offline_result(py, &eval, hooks.root());
        assert_eq!(
            attr_text(&result, "status"),
            attr_text(
                &matrix
                    .getattr("ReceiptStatus")
                    .unwrap()
                    .getattr("NOT_READY")
                    .unwrap(),
                "value"
            )
        );
        assert!(!result
            .getattr("is_valid")
            .unwrap()
            .extract::<bool>()
            .unwrap());
        assert!(result.getattr("score").unwrap().extract::<f64>().unwrap() < 100.0);
        let diagnostics = result.getattr("diagnostics").unwrap();
        assert!(
            diagnostics
                .try_iter()
                .unwrap()
                .any(|item| { text(&item.unwrap()).contains("embedding-canary") }),
            "offline result must identify the embedding canary"
        );
    });
}

#[test]
fn complete_runtime_receipt_is_required_for_pass() {
    let case = Case::new();
    Python::attach(|py| {
        let eval = module(py, "conductor.workspace_eval");
        let required = eval.getattr("REQUIRED_CELL_IDS").unwrap();
        let mut cell_ids: Vec<String> = required
            .try_iter()
            .unwrap()
            .map(|item| text(&item.unwrap()))
            .collect();
        cell_ids.sort();
        let cells: Vec<Value> = cell_ids
            .iter()
            .map(|cell_id| {
                json!({"cell_id":cell_id,"status":"PASS",
                "detail":"fixture","required":true,"evidence":{}})
            })
            .collect();
        let receipt = case.write(
            "receipt.json",
            &json!({"schema_version":1,"status":"PASS","cells":cells,
                "provenance":{"fixture":true}})
            .to_string(),
        );
        let result = evaluate_with_receipt(py, &eval, &receipt, true);
        assert_eq!(attr_text(&result, "status"), "PASS");
        assert!(result
            .getattr("is_valid")
            .unwrap()
            .extract::<bool>()
            .unwrap());
        assert_eq!(
            result.getattr("score").unwrap().extract::<f64>().unwrap(),
            100.0
        );
    });
}

#[test]
fn missing_or_nonpass_runtime_cell_fails_closed() {
    let case = Case::new();
    let receipt = case.write(
        "receipt.json",
        &json!({"schema_version":1,"status":"PASS","cells":[
            {"cell_id":"active-state-live-claims","status":"NOT_READY"}
        ]})
        .to_string(),
    );
    Python::attach(|py| {
        let eval = module(py, "conductor.workspace_eval");
        let result = evaluate_with_receipt(py, &eval, &receipt, false);
        assert_eq!(attr_text(&result, "status"), "FAIL-CLOSED");
        assert!(!result
            .getattr("is_valid")
            .unwrap()
            .extract::<bool>()
            .unwrap());
    });
}

#[test]
fn write_receipt_preserves_status() {
    let mut case = Case::new();
    Python::attach(|py| {
        let hooks = HookMatrix::new(py, &mut case);
        workspace_source_fixture::install(py, hooks.root());
        let eval = module(py, "conductor.workspace_eval");
        let result = offline_result(py, &eval, hooks.root());
        let kwargs = PyDict::new(py);
        kwargs
            .set_item("receipts_dir", path(py, case.root()))
            .unwrap();
        let written = eval
            .getattr("write_receipt")
            .unwrap()
            .call((&result,), Some(&kwargs))
            .unwrap();
        let receipt_path: std::path::PathBuf = text(&written).into();
        let payload: Value = serde_json::from_slice(&fs::read(receipt_path).unwrap()).unwrap();
        assert_eq!(payload["status"], attr_text(&result, "status"));
        assert!(payload["metrics"]["provenance"]["oracle"]
            .as_str()
            .unwrap()
            .ends_with("workspace_runtime_matrix"));
    });
}
