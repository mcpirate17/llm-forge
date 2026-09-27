#![cfg(feature = "python-compat-tests")]
//! Offline clerk canary contracts: bounded request, schema, and unload.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyModule};
use serde_json::{json, Value};
use std::ffi::CString;
use support::{attr_text, module, path, AttrPatch, Case};

const MOCKS: &str = r#"
import subprocess
request = {}
ps_outputs = iter([resident, 'NAME ID SIZE PROCESSOR'])
def preflight(*_args, **_kwargs):
    return ready
def fake_http(_url, *, payload, timeout):
    request['payload'] = payload
    request['timeout'] = timeout
    return response
def fake_ps():
    return next(ps_outputs)
def fake_run(*args, **kwargs):
    request['stop'] = args[0]
    return subprocess.CompletedProcess([], 0, '', '')
"#;

fn py_json<'py>(py: Python<'py>, value: &Value) -> Bound<'py, PyAny> {
    PyModule::import(py, "json")
        .unwrap()
        .getattr("loads")
        .unwrap()
        .call1((value.to_string(),))
        .unwrap()
}

fn exercise(content: &str, status: &str, expected_schema: bool) {
    let case = Case::new();
    Python::attach(|py| {
        let matrix = module(py, "conductor.workspace_runtime_matrix");
        let ready = matrix
            .getattr("ClerkGpuPreflight")
            .unwrap()
            .call1((true, (), (), ()))
            .unwrap();
        let response = py_json(
            py,
            &json!({"model":"qwen3.5:9b",
            "message":{"content":content,"thinking":""},"done":true,
            "done_reason":"stop","prompt_eval_count":20,
            "eval_count":if expected_schema {9} else {32}}),
        );
        let globals = PyDict::new(py);
        globals.set_item("ready", &ready).unwrap();
        globals.set_item("response", &response).unwrap();
        globals
            .set_item(
                "resident",
                "NAME ID SIZE PROCESSOR\nqwen3.5:9b id 6.6GB 100% GPU",
            )
            .unwrap();
        py.run(&CString::new(MOCKS).unwrap(), Some(&globals), None)
            .unwrap();
        let mut patches = Vec::new();
        for (target, name) in [
            ("clerk_gpu_preflight", "preflight"),
            ("_http_json", "fake_http"),
            ("_ollama_ps", "fake_ps"),
            ("_run", "fake_run"),
        ] {
            let callback = globals.get_item(name).unwrap().unwrap();
            patches.push(AttrPatch::replace(matrix.as_any(), target, &callback));
        }
        let cell = matrix
            .getattr("run_clerk_canary")
            .unwrap()
            .call1((path(py, case.root()),))
            .unwrap();
        assert_eq!(attr_text(&cell, "status"), status);
        let request = globals.get_item("request").unwrap().unwrap();
        let argv: Vec<String> = request.get_item("stop").unwrap().extract().unwrap();
        assert_eq!(argv, ["ollama", "stop", "qwen3.5:9b"]);
        let saved: Value = serde_json::from_str(
            &std::fs::read_to_string(case.root().join("local_clerk.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(saved["schema_valid"], expected_schema);
        assert_eq!(saved["unloaded"], true);
        if expected_schema {
            let payload = request.get_item("payload").unwrap();
            assert!(!payload
                .get_item("think")
                .unwrap()
                .extract::<bool>()
                .unwrap());
            let options = payload.get_item("options").unwrap();
            for (key, value) in [
                ("num_ctx", 2048),
                ("num_gpu", 99),
                ("num_predict", 32),
                ("presence_penalty", 0),
                ("seed", 0),
                ("temperature", 0),
            ] {
                assert_eq!(
                    options.get_item(key).unwrap().extract::<i64>().unwrap(),
                    value
                );
            }
            let messages = payload.get_item("messages").unwrap();
            let system: String = messages
                .get_item(0)
                .unwrap()
                .get_item("content")
                .unwrap()
                .extract()
                .unwrap();
            let prompt: String = messages
                .get_item(1)
                .unwrap()
                .get_item("content")
                .unwrap()
                .extract()
                .unwrap();
            assert!(system.contains("zero authority"));
            assert!(prompt.contains("\"additionalProperties\":false"));
        } else {
            assert!(!cell
                .getattr("evidence")
                .unwrap()
                .get_item("schema_valid")
                .unwrap()
                .extract::<bool>()
                .unwrap());
            assert!(cell
                .getattr("evidence")
                .unwrap()
                .get_item("unloaded")
                .unwrap()
                .extract::<bool>()
                .unwrap());
        }
        drop(patches);
    });
}

#[test]
fn valid_clerk_canary_uses_bounded_schema_and_unloads() {
    exercise("{\"status\":\"PASS\",\"cells\":5}", "PASS", true);
}

#[test]
fn invalid_clerk_schema_fails_closed_and_unloads() {
    exercise("not-json", "FAIL-CLOSED", false);
}
