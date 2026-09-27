#![cfg(feature = "python-compat-tests")]
//! Rust assertions for status, graph, launcher, and GPU preflight contracts.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyList, PyModule};
use serde_json::{json, Value};
use std::ffi::CString;
use support::{assert_error, attr_bool, attr_text, module, path, text, AttrPatch, Case};

fn py_json<'py>(py: Python<'py>, value: &Value) -> Bound<'py, PyAny> {
    PyModule::import(py, "json")
        .unwrap()
        .getattr("loads")
        .unwrap()
        .call1((value.to_string(),))
        .unwrap()
}

fn eval<'py>(py: Python<'py>, source: &str, globals: &Bound<'py, PyDict>) -> Bound<'py, PyAny> {
    py.eval(&CString::new(source).unwrap(), Some(globals), None)
        .unwrap()
}

fn cell<'py>(cell_type: &Bound<'py, PyAny>, status: &Bound<'py, PyAny>) -> Bound<'py, PyAny> {
    cell_type.call1(("cell", status, "test")).unwrap()
}

fn graph(mixed: i64, mode: &str, results: i64, dimension: i64, paid: bool) -> Value {
    json!({"provider":"workspace:provider-fingerprint",
        "backend_fingerprint":"sha256:backend-fingerprint",
        "stored_provider":"workspace:provider-fingerprint", "model":"model",
        "dimension":dimension,"paid":paid,"search_mode":mode,"result_count":results,
        "node_count":39013,"live_non_file_node_count":35000,"embedded_node_count":35000,
        "missing_embedding_count":0,"mixed_provider_live_count":mixed,
        "orphan_embedding_count":0,"expected_result_found":true,
        "query_trace":{"provider_name":"workspace:provider-fingerprint",
            "backend_fingerprint":"sha256:backend-fingerprint","purpose":"query",
            "vector_count":1,"broker_calls":1,"dimension":dimension,"paid":paid}})
}

#[test]
fn required_and_optional_status_precedence() {
    let _case = Case::new();
    Python::attach(|py| {
        let matrix = module(py, "conductor.workspace_runtime_matrix");
        let statuses = matrix.getattr("ReceiptStatus").unwrap();
        let cell_type = matrix.getattr("CellReceipt").unwrap();
        let pass = statuses.getattr("PASS").unwrap();
        let waiting = statuses.getattr("NOT_READY").unwrap();
        let failed = statuses.getattr("FAIL_CLOSED").unwrap();
        for (values, expected) in [
            (vec![cell(&cell_type, &pass)], &pass),
            (
                vec![cell(&cell_type, &pass), cell(&cell_type, &waiting)],
                &waiting,
            ),
            (
                vec![cell(&cell_type, &waiting), cell(&cell_type, &failed)],
                &failed,
            ),
        ] {
            let result = matrix
                .getattr("aggregate_status")
                .unwrap()
                .call1((PyList::new(py, values).unwrap(),))
                .unwrap();
            assert!(result.is(expected));
        }
        let kwargs = PyDict::new(py);
        kwargs.set_item("required", false).unwrap();
        let optional = cell_type
            .call(("optional", &failed, "ignored"), Some(&kwargs))
            .unwrap();
        let result = matrix
            .getattr("aggregate_status")
            .unwrap()
            .call1((PyList::new(py, [cell(&cell_type, &pass), optional]).unwrap(),))
            .unwrap();
        assert!(result.is(&pass));
    });
}

#[test]
fn graph_requires_semantic_result_and_rejects_mixed_provider_rows() {
    let case = Case::new();
    Python::attach(|py| {
        let matrix = module(py, "conductor.workspace_runtime_matrix");
        let evidence = case.root().join("graph.json");
        for (payload, expected) in [
            (graph(0, "keyword", 0, 1024, false), "FAIL-CLOSED"),
            (graph(0, "hybrid", 2, 1536, true), "PASS"),
            (graph(1, "hybrid", 1, 1024, false), "FAIL-CLOSED"),
        ] {
            std::fs::write(&evidence, payload.to_string()).unwrap();
            let result = matrix
                .getattr("load_graph_evidence")
                .unwrap()
                .call1((path(py, &evidence),))
                .unwrap();
            assert_eq!(attr_text(&result, "status"), expected, "payload: {payload}");
        }
        let missing = matrix
            .getattr("load_graph_evidence")
            .unwrap()
            .call1((path(py, &case.root().join("missing.json")),))
            .unwrap();
        assert_eq!(attr_text(&missing, "status"), "NOT_READY");
    });
}

#[test]
fn grok_command_and_launcher_specs_remain_bounded() {
    let mut case = Case::new();
    case.remove_env("GROK_INSPECT_COMMAND");
    Python::attach(|py| {
        let matrix = module(py, "conductor.workspace_runtime_matrix");
        let command = matrix.getattr("_grok_inspect_argv").unwrap();
        assert_eq!(
            command.call0().unwrap().extract::<Vec<String>>().unwrap(),
            ["grok", "inspect", "--json"]
        );
        case.set_env(
            "GROK_INSPECT_COMMAND",
            "python -m conductor.grok_inspect_stub",
        );
        assert_eq!(
            command.call0().unwrap().extract::<Vec<String>>().unwrap(),
            ["python", "-m", "conductor.grok_inspect_stub"]
        );
        let specs: Vec<Py<PyAny>> = matrix
            .getattr("launcher_specs")
            .unwrap()
            .call0()
            .unwrap()
            .extract()
            .unwrap();
        let names: Vec<String> = specs
            .iter()
            .map(|spec| spec.bind(py).getattr("name").unwrap().extract().unwrap())
            .collect();
        let required: Vec<String> = matrix
            .getattr("REQUIRED_LAUNCHERS")
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(names, required);
        assert_eq!(
            specs.len(),
            matrix
                .getattr("MAX_LAUNCHER_CALLS")
                .unwrap()
                .extract::<usize>()
                .unwrap()
        );
        for spec in &specs {
            let name: String = spec.bind(py).getattr("name").unwrap().extract().unwrap();
            let argv: Vec<String> = spec.bind(py).getattr("argv").unwrap().extract().unwrap();
            match name.as_str() {
                "codex" => {
                    assert!(!argv.contains(&"--ask-for-approval".to_string()));
                    assert!(argv.contains(&"--dangerously-bypass-hook-trust".to_string()));
                }
                "claude" | "glm" => assert!(argv.contains(&"--verbose".to_string())),
                _ => {}
            }
        }
    });
}

#[test]
fn ollama_rows_and_token_metrics_fail_closed() {
    let _case = Case::new();
    Python::attach(|py| {
        let matrix = module(py, "conductor.workspace_runtime_matrix");
        let rows = matrix.getattr("_ollama_model_rows").unwrap();
        assert!(rows
            .call1(("NAME ID SIZE PROCESSOR",))
            .unwrap()
            .extract::<Vec<String>>()
            .unwrap()
            .is_empty());
        assert_eq!(
            rows.call1(("NAME ID SIZE PROCESSOR\nqwen3.5:9b id 6.6GB 100% GPU",))
                .unwrap()
                .extract::<Vec<String>>()
                .unwrap(),
            ["qwen3.5:9b id 6.6GB 100% GPU"]
        );
        let count = matrix.getattr("_nonnegative_int").unwrap();
        assert_eq!(
            count
                .call1((py_json(py, &json!({"tokens":4})), "tokens"))
                .unwrap()
                .extract::<i64>()
                .unwrap(),
            4
        );
        assert_eq!(
            count
                .call1((py_json(py, &json!({"tokens":null})), "tokens"))
                .unwrap()
                .extract::<i64>()
                .unwrap(),
            -1
        );
        let error = rows.call1(("connection failed",)).unwrap_err();
        assert_error(
            py,
            error,
            py.get_type::<pyo3::exceptions::PyRuntimeError>().as_any(),
            "unexpected ollama ps",
        );
    });
}

#[test]
fn gpu_preflight_blocks_active_novel_claims_and_filters_desktop_process() {
    let _case = Case::new();
    Python::attach(|py| {
        let matrix = module(py, "conductor.workspace_runtime_matrix");
        let globals = PyDict::new(py);
        let types = PyModule::import(py, "types").unwrap();
        let claim_type = types.getattr("SimpleNamespace").unwrap();
        let active = eval(py, "lambda _now: True", &globals);
        let claims = PyList::empty(py);
        for (id, owner, justification, filename) in [
            (
                "claim-nm-f6",
                "codex-nm-f6-head",
                "AVO throughput candidate",
                "research/tools/nm_f6_candidate.py",
            ),
            (
                "claim-cuda-path",
                "codex",
                "partitioned output head implementation",
                "research/tools/_nm_f6_partitioned_head_cuda.cu",
            ),
        ] {
            let kwargs = PyDict::new(py);
            kwargs.set_item("claim_id", id).unwrap();
            kwargs.set_item("owner", owner).unwrap();
            kwargs.set_item("justification", justification).unwrap();
            kwargs.set_item("paths", (filename,)).unwrap();
            kwargs.set_item("active", &active).unwrap();
            claims
                .append(claim_type.call((), Some(&kwargs)).unwrap())
                .unwrap();
        }
        globals.set_item("claims", &claims).unwrap();
        let load = eval(py, "lambda _repo: (claims, 'digest')", &globals);
        let no_processes = eval(py, "lambda: ()", &globals);
        let ps = eval(py, "lambda: 'NAME ID SIZE PROCESSOR'", &globals);
        let _claims = AttrPatch::replace(matrix.as_any(), "load_claims", &load);
        let _processes =
            AttrPatch::replace(matrix.as_any(), "_gpu_compute_processes", &no_processes);
        let _ps = AttrPatch::replace(matrix.as_any(), "_ollama_ps", &ps);
        let result = matrix
            .getattr("clerk_gpu_preflight")
            .unwrap()
            .call0()
            .unwrap();
        assert!(!attr_bool(&result, "ready"));
        assert_eq!(
            result
                .getattr("blocking_claim_ids")
                .unwrap()
                .extract::<Vec<String>>()
                .unwrap(),
            ["claim-cuda-path", "claim-nm-f6"]
        );

        let empty_claims = eval(py, "lambda _repo: ((), 'digest')", &globals);
        let _empty = AttrPatch::replace(matrix.as_any(), "load_claims", &empty_claims);
        let process_type = matrix.getattr("GpuComputeProcess").unwrap();
        let compute = process_type.call1((42, "python", 4096)).unwrap();
        let desktop = process_type
            .call1((43, "gnome-remote-desktop", 512))
            .unwrap();
        globals.set_item("processes", (compute, desktop)).unwrap();
        let process_callback = eval(py, "lambda: processes", &globals);
        let _compute =
            AttrPatch::replace(matrix.as_any(), "_gpu_compute_processes", &process_callback);
        let loaded = eval(
            py,
            "lambda: 'NAME ID SIZE PROCESSOR\\nother-model id 1GB 100% GPU'",
            &globals,
        );
        let _loaded = AttrPatch::replace(matrix.as_any(), "_ollama_ps", &loaded);
        let result = matrix
            .getattr("clerk_gpu_preflight")
            .unwrap()
            .call0()
            .unwrap();
        assert!(!attr_bool(&result, "ready"));
        let processes = result.getattr("blocking_processes").unwrap();
        assert_eq!(processes.len().unwrap(), 1);
        assert_eq!(
            processes
                .get_item(0)
                .unwrap()
                .getattr("pid")
                .unwrap()
                .extract::<i64>()
                .unwrap(),
            42
        );
        assert_eq!(
            result
                .getattr("loaded_models")
                .unwrap()
                .extract::<Vec<String>>()
                .unwrap(),
            ["other-model id 1GB 100% GPU"]
        );
        assert_eq!(text(&result.getattr("blocking_claim_ids").unwrap()), "()");
    });
}
