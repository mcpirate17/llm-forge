#![cfg(feature = "python-compat-tests")]
//! Receipt reconciliation contracts, including preserved expensive evidence.

#[path = "python_contracts/hook_matrix_fixture.rs"]
mod hook_matrix_fixture;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;
#[path = "python_contracts/workspace_source_fixture.rs"]
mod workspace_source_fixture;

use hook_matrix_fixture::HookMatrix;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict};
use serde_json::{json, Value};
use std::ffi::CString;
use support::{attr_text, module, path, AttrPatch, Case};

fn receipt(cell: &str, companion: Option<&str>) -> Value {
    let mut cells = vec![json!({"cell_id":cell,"status":"FAIL-CLOSED",
        "detail":"old failure","required":true,"evidence":{}})];
    if let Some(name) = companion {
        cells.push(json!({"cell_id":name,"status":"PASS",
            "detail":"preserve me","required":true,"evidence":{"tokens":99}}));
    }
    json!({"status":"FAIL-CLOSED","cells":cells,"provenance":{}})
}

fn graph() -> Value {
    json!({"provider":"workspace:provider-fingerprint",
        "backend_fingerprint":"sha256:backend-fingerprint",
        "stored_provider":"workspace:provider-fingerprint","model":"paid-model",
        "dimension":1536,"paid":true,"search_mode":"hybrid","result_count":1,
        "node_count":2,"live_non_file_node_count":1,"embedded_node_count":1,
        "missing_embedding_count":0,"mixed_provider_live_count":0,
        "orphan_embedding_count":0,"expected_result_found":true,
        "query_trace":{"provider_name":"workspace:provider-fingerprint",
            "backend_fingerprint":"sha256:backend-fingerprint","purpose":"query",
            "vector_count":1,"broker_calls":1,"dimension":1536,"paid":true}})
}

fn assert_preserved(payload: &Bound<'_, PyAny>) {
    assert_eq!(
        payload
            .get_item("status")
            .unwrap()
            .extract::<String>()
            .unwrap(),
        "PASS"
    );
    let cells = payload.get_item("cells").unwrap();
    assert_eq!(
        cells
            .get_item(0)
            .unwrap()
            .get_item("status")
            .unwrap()
            .extract::<String>()
            .unwrap(),
        "PASS"
    );
    let expensive = cells.get_item(1).unwrap().get_item("evidence").unwrap();
    assert_eq!(
        expensive
            .get_item("tokens")
            .unwrap()
            .extract::<i64>()
            .unwrap(),
        99
    );
}

#[test]
fn hook_program_controls_pass_in_generated_foreign_repository() {
    let mut case = Case::new();
    Python::attach(|py| {
        let hooks = HookMatrix::new(py, &mut case);
        workspace_source_fixture::install(py, hooks.root());
        let matrix = module(py, "conductor.workspace_runtime_matrix");
        let cell = matrix
            .getattr("check_hook_programs")
            .unwrap()
            .call1((path(py, hooks.root()),))
            .unwrap();
        assert_eq!(
            attr_text(&cell, "status"),
            "PASS",
            "{}",
            attr_text(&cell, "detail")
        );
    });
}

#[test]
fn launcher_reconciliation_uses_preserved_terminal_usage() {
    let mut case = Case::new();
    let launchers = case.mkdir("launchers");
    let names = ["codex", "claude", "glm", "qwen", "grok"];
    for (index, name) in names.iter().enumerate() {
        let index = (index + 1) as i64;
        std::fs::write(
            launchers.join(format!("{name}.log")),
            json!({"type":"result","usage":{"input_tokens":index*10,
                "cached_input_tokens":index*7,"output_tokens":index}})
            .to_string(),
        )
        .unwrap();
        std::fs::write(
            launchers.join(format!("{name}.json")),
            json!({"launcher":name,"status":"PASS"}).to_string(),
        )
        .unwrap();
    }
    case.write(
        "receipt.json",
        &receipt("launcher-real-smokes", None).to_string(),
    );
    Python::attach(|py| {
        let hooks = HookMatrix::new(py, &mut case);
        workspace_source_fixture::install(py, hooks.root());
        let matrix = module(py, "conductor.workspace_runtime_matrix");
        let kwargs = PyDict::new(py);
        kwargs.set_item("repo", path(py, hooks.root())).unwrap();
        let payload = matrix
            .getattr("reconcile_receipt")
            .unwrap()
            .call((path(py, case.root()),), Some(&kwargs))
            .unwrap();
        assert_eq!(
            payload
                .get_item("status")
                .unwrap()
                .extract::<String>()
                .unwrap(),
            "PASS"
        );
        let tokens: i64 = payload
            .get_item("cells")
            .unwrap()
            .get_item(0)
            .unwrap()
            .get_item("evidence")
            .unwrap()
            .get_item("reported_tokens")
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(tokens, 165);
        assert!(case.root().join("receipt.pre_reconcile.json").is_file());
    });
}

#[test]
fn graph_reconciliation_preserves_other_cells_and_archives_old_receipt() {
    let case = Case::new();
    case.write(
        "receipt.json",
        &receipt("graph-semantic-runtime", Some("expensive-cell")).to_string(),
    );
    let graph = case.write("graph.json", &graph().to_string());
    Python::attach(|py| {
        let matrix = module(py, "conductor.workspace_runtime_matrix");
        let payload = matrix
            .getattr("reconcile_graph_evidence")
            .unwrap()
            .call1((path(py, case.root()), path(py, &graph)))
            .unwrap();
        assert_preserved(&payload);
        assert!(case
            .root()
            .join("receipt.pre_graph_reconcile.json")
            .is_file());
    });
}

#[test]
fn clerk_reconciliation_preserves_expensive_cells_and_archives_old_receipt() {
    let case = Case::new();
    case.write(
        "receipt.json",
        &receipt("local-clerk-canary", Some("expensive-launchers")).to_string(),
    );
    Python::attach(|py| {
        let matrix = module(py, "conductor.workspace_runtime_matrix");
        let globals = PyDict::new(py);
        globals.set_item("matrix", &matrix).unwrap();
        py.run(
            &CString::new(concat!(
                "def fake_canary(_output, **_kwargs):\n",
                "    return matrix.CellReceipt('local-clerk-canary', matrix.ReceiptStatus.PASS,\n",
                "                              'fixed', evidence={'schema_valid': True})\n",
            ))
            .unwrap(),
            Some(&globals),
            None,
        )
        .unwrap();
        let fake = globals.get_item("fake_canary").unwrap().unwrap();
        let _patch = AttrPatch::replace(matrix.as_any(), "run_clerk_canary", &fake);
        let payload = matrix
            .getattr("reconcile_clerk_evidence")
            .unwrap()
            .call1((path(py, case.root()),))
            .unwrap();
        assert_preserved(&payload);
        assert!(case
            .root()
            .join("receipt.pre_clerk_reconcile.json")
            .is_file());
    });
}
