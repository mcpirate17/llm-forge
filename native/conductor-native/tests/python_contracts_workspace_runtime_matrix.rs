#![cfg(feature = "python-compat-tests")]
//! Public Python boundary and safety ordering for the native runtime matrix.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyList, PyModule, PyTuple};
use serde_json::{json, Value};
use std::ffi::CString;
use support::{attr_text, module, path, AttrPatch, Case};

fn py_json<'py>(py: Python<'py>, value: &Value) -> Bound<'py, PyAny> {
    let json_module = PyModule::import(py, "json").unwrap();
    json_module
        .getattr("loads")
        .unwrap()
        .call1((value.to_string(),))
        .unwrap()
}

fn eval_with<'py>(
    py: Python<'py>,
    expression: &str,
    globals: &Bound<'py, PyDict>,
) -> Bound<'py, PyAny> {
    let code = CString::new(expression).unwrap();
    py.eval(&code, Some(globals), None).unwrap()
}

fn status(cell: &Bound<'_, PyAny>) -> String {
    attr_text(cell, "status")
}

#[test]
fn status_token_and_graph_wrappers_match_native_policy() {
    let case = Case::new();
    Python::attach(|py| {
        let matrix = module(py, "conductor.workspace_runtime_matrix");
        let pass = matrix
            .getattr("ReceiptStatus")
            .unwrap()
            .getattr("PASS")
            .unwrap();
        let not_ready = matrix
            .getattr("ReceiptStatus")
            .unwrap()
            .getattr("NOT_READY")
            .unwrap();
        let cell_type = matrix.getattr("CellReceipt").unwrap();
        let good = cell_type.call1(("good", &pass, "ok")).unwrap();
        let waiting = cell_type.call1(("waiting", &not_ready, "pending")).unwrap();
        let cells = PyList::new(py, [&good, &waiting]).unwrap();
        let aggregate = matrix
            .getattr("aggregate_status")
            .unwrap()
            .call1((cells,))
            .unwrap();
        assert!(aggregate.is(&not_ready));

        let usage = concat!(
            "{\"usage\":{\"input_tokens\":11,\"cached_input_tokens\":9,\"output_tokens\":7}}\n",
            "ordinary launcher output\n",
            "{\"type\":\"result\",\"usage\":{\"input_tokens\":21,\"output_tokens\":4,\"total_tokens\":25}}"
        );
        let tokens: i64 = matrix
            .getattr("extract_reported_tokens")
            .unwrap()
            .call1((usage,))
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(tokens, 25);

        let graph = json!({"provider":"workspace:provider-fingerprint",
            "backend_fingerprint":"sha256:backend-fingerprint","stored_provider":"workspace:provider-fingerprint",
            "model":"local-model","dimension":1024,"paid":false,"search_mode":"hybrid",
            "result_count":2,"node_count":3,"live_non_file_node_count":2,"embedded_node_count":2,
            "missing_embedding_count":0,"mixed_provider_live_count":0,"orphan_embedding_count":0,
            "expected_result_found":true,"query_trace":{"provider_name":"workspace:provider-fingerprint",
                "backend_fingerprint":"sha256:backend-fingerprint","purpose":"query","vector_count":1,
                "broker_calls":1,"dimension":1024,"paid":false}});
        let graph_path = case.write("graph.json", &graph.to_string());
        let result = matrix
            .getattr("load_graph_evidence")
            .unwrap()
            .call1((path(py, &graph_path),))
            .unwrap();
        assert_eq!(status(&result), "PASS");
        let evidence = result.getattr("evidence").unwrap();
        assert_eq!(
            evidence
                .get_item("source_sha256")
                .unwrap()
                .extract::<String>()
                .unwrap()
                .len(),
            64
        );
    });
}

#[test]
fn clerk_payload_and_adjudication_preserve_public_shapes() {
    let _case = Case::new();
    Python::attach(|py| {
        let matrix = module(py, "conductor.workspace_runtime_matrix");
        let schema = matrix.getattr("_clerk_schema").unwrap().call0().unwrap();
        let payload = matrix
            .getattr("_clerk_payload")
            .unwrap()
            .call1((schema,))
            .unwrap();
        let options = payload.get_item("options").unwrap();
        assert_eq!(
            options
                .get_item("num_ctx")
                .unwrap()
                .extract::<i64>()
                .unwrap(),
            2048
        );
        assert_eq!(
            options
                .get_item("num_gpu")
                .unwrap()
                .extract::<i64>()
                .unwrap(),
            99
        );
        assert_eq!(
            options
                .get_item("num_predict")
                .unwrap()
                .extract::<i64>()
                .unwrap(),
            32
        );
        assert!(!payload
            .get_item("think")
            .unwrap()
            .extract::<bool>()
            .unwrap());

        let preflight = matrix
            .getattr("ClerkGpuPreflight")
            .unwrap()
            .call1((
                true,
                PyTuple::empty(py),
                PyTuple::empty(py),
                PyTuple::empty(py),
            ))
            .unwrap();
        let response = py_json(
            py,
            &json!({"model":"qwen3.5:9b","message":{
            "content":"{\"status\":\"PASS\",\"cells\":5}","thinking":""},
            "done":true,"done_reason":"stop","prompt_eval_count":20,"eval_count":9}),
        );
        let attempt = matrix
            .getattr("ClerkAttempt")
            .unwrap()
            .call1((
                response,
                "NAME ID SIZE PROCESSOR\nqwen3.5:9b id 6.6GB 100% GPU",
                "NAME ID SIZE PROCESSOR",
                0,
                "",
            ))
            .unwrap();
        let result = matrix
            .getattr("_adjudicate_clerk_attempt")
            .unwrap()
            .call1((preflight, attempt))
            .unwrap();
        let result = result.cast::<PyTuple>().unwrap();
        assert!(result.get_item(0).unwrap().extract::<bool>().unwrap());
        let evidence = result.get_item(1).unwrap();
        assert!(evidence
            .get_item("schema_valid")
            .unwrap()
            .extract::<bool>()
            .unwrap());
        assert!(evidence
            .get_item("unloaded")
            .unwrap()
            .extract::<bool>()
            .unwrap());
    });
}

#[test]
fn blocked_gpu_preflight_never_invokes_clerk_http() {
    let case = Case::new();
    Python::attach(|py| {
        let matrix = module(py, "conductor.workspace_runtime_matrix");
        let blocked = matrix
            .getattr("ClerkGpuPreflight")
            .unwrap()
            .call1((
                false,
                ("claim-nm-f6",),
                PyTuple::empty(py),
                PyTuple::empty(py),
            ))
            .unwrap();
        let globals = PyDict::new(py);
        globals.set_item("blocked", blocked).unwrap();
        let preflight = eval_with(py, "lambda *args, **kwargs: blocked", &globals);
        let forbidden_http = eval_with(py, "lambda *args, **kwargs: 1 / 0", &globals);
        let _patch_preflight =
            AttrPatch::replace(matrix.as_any(), "clerk_gpu_preflight", &preflight);
        let _patch_http = AttrPatch::replace(matrix.as_any(), "_http_json", &forbidden_http);
        let result = matrix
            .getattr("run_clerk_canary")
            .unwrap()
            .call1((path(py, case.root()),))
            .unwrap();
        assert_eq!(status(&result), "NOT_READY");
        let evidence = result.getattr("evidence").unwrap();
        let preflight = evidence.get_item("preflight").unwrap();
        let blocked_ids: Vec<String> = preflight
            .get_item("blocking_claim_ids")
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(blocked_ids, vec!["claim-nm-f6"]);
    });
}
