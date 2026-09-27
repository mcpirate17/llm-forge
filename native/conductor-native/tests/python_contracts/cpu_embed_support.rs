//! Rust-owned fixtures for the shipped embedding broker API.

use crate::support::{module, AttrPatch, Case};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyBytes, PyCFunction, PyDict, PyTuple};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

pub struct EmbedCase(pub Case);

impl EmbedCase {
    pub fn new() -> Self {
        let mut case = Case::new();
        for key in [
            "CPU_EMBED_MODEL",
            "CPU_EMBED_NUM_GPU",
            "WORKSPACE_EMBED_QUALITY_RECEIPT",
            "WORKSPACE_EMBED_PAID_ENABLED",
            "WORKSPACE_EMBED_PAID_BASE_URL",
            "WORKSPACE_EMBED_PAID_API_KEY",
            "WORKSPACE_EMBED_PAID_MODEL",
            "WORKSPACE_EMBED_PAID_MODEL_REVISION",
            "WORKSPACE_EMBED_PAID_DIMENSION",
            "WORKSPACE_EMBED_PAID_COST_PER_MILLION_TOKENS",
            "TEST_PAID_API_KEY",
        ] {
            case.remove_env(key);
        }
        Self(case)
    }

    pub fn set_env(&mut self, key: &'static str, value: &str) {
        self.0.set_env(key, value);
    }

    pub fn root(&self) -> &std::path::Path {
        self.0.root()
    }
}

pub fn api(py: Python<'_>) -> Bound<'_, pyo3::types::PyModule> {
    module(py, "conductor.cpu_embed")
}

pub fn contract(py: Python<'_>) -> Bound<'_, pyo3::types::PyModule> {
    module(py, "conductor.embedding_contract")
}

pub fn json_to_py<'py>(py: Python<'py>, value: &Value) -> Bound<'py, PyAny> {
    module(py, "json")
        .getattr("loads")
        .unwrap()
        .call1((value.to_string(),))
        .unwrap()
}

pub fn py_to_json(value: &Bound<'_, PyAny>) -> Value {
    let py = value.py();
    let raw: String = module(py, "json")
        .getattr("dumps")
        .unwrap()
        .call1((value,))
        .unwrap()
        .extract()
        .unwrap();
    serde_json::from_str(&raw).unwrap()
}

pub fn route<'py>(py: Python<'py>, overrides: &Value) -> Bound<'py, PyAny> {
    let mut fields = json!({
        "route_id": "test-route",
        "backend_kind": "ollama-local",
        "protocol": "ollama-embed",
        "priority": 10,
        "normalization": "none",
        "tokenizer_policy": "backend-default",
        "truncation_policy": "unicode-prefix",
        "max_input_chars": 2000,
        "device_policy": "cpu",
        "paid": false,
        "endpoint": "http://127.0.0.1:11434/api/embed",
        "api_key_env": "",
        "model": "test-model",
        "model_revision": format!("sha256:{}", "a".repeat(64)),
        "dimension": 2,
        "cost_per_million_tokens": 0.0,
        "max_request_usd": 0.0
    });
    for (name, value) in overrides.as_object().expect("route overrides") {
        fields[name] = value.clone();
    }
    let kwargs = json_to_py(py, &fields);
    contract(py)
        .getattr("ResolvedRoute")
        .unwrap()
        .call((), Some(kwargs.cast::<PyDict>().unwrap()))
        .unwrap()
}

pub fn paid_route<'py>(py: Python<'py>, overrides: &Value) -> Bound<'py, PyAny> {
    let mut fields = json!({
        "route_id": "paid-route",
        "backend_kind": "paid",
        "protocol": "openai-embeddings",
        "paid": true,
        "endpoint": "https://embeddings.example/v1/embeddings",
        "api_key_env": "TEST_PAID_API_KEY"
    });
    for (name, value) in overrides.as_object().expect("paid overrides") {
        fields[name] = value.clone();
    }
    route(py, &fields)
}

pub fn attempt<'py>(py: Python<'py>, selected: &Bound<'py, PyAny>) -> Bound<'py, PyAny> {
    contract(py)
        .getattr("RouteAttempt")
        .unwrap()
        .call1((
            selected,
            contract(py)
                .getattr("SelectionReason")
                .unwrap()
                .getattr("PRIMARY")
                .unwrap(),
        ))
        .unwrap()
}

pub fn mock_return<'py>(py: Python<'py>, value: &Bound<'py, PyAny>) -> Bound<'py, PyAny> {
    let kwargs = PyDict::new(py);
    kwargs.set_item("return_value", value).unwrap();
    module(py, "unittest.mock")
        .getattr("Mock")
        .unwrap()
        .call((), Some(&kwargs))
        .unwrap()
}

pub fn mock_raise<'py>(py: Python<'py>, exception: &Bound<'py, PyAny>) -> Bound<'py, PyAny> {
    let kwargs = PyDict::new(py);
    kwargs.set_item("side_effect", exception).unwrap();
    module(py, "unittest.mock")
        .getattr("Mock")
        .unwrap()
        .call((), Some(&kwargs))
        .unwrap()
}

pub fn patch<'py>(
    object: &Bound<'py, PyAny>,
    name: &str,
    replacement: &Bound<'py, PyAny>,
) -> AttrPatch {
    AttrPatch::replace(object, name, replacement)
}

pub fn pin_route(py: Python<'_>, selected: &Bound<'_, PyAny>) -> AttrPatch {
    let one = PyTuple::new(py, [attempt(py, selected)]).unwrap();
    patch(
        api(py).as_any(),
        "route_attempts",
        &mock_return(py, one.as_any()),
    )
}

pub fn contract_error<'py>(py: Python<'py>, message: &str) -> Bound<'py, PyAny> {
    contract(py)
        .getattr("EmbeddingContractError")
        .unwrap()
        .call1((message,))
        .unwrap()
}

pub fn fail_route_attempts(py: Python<'_>, message: &str) -> AttrPatch {
    patch(
        api(py).as_any(),
        "route_attempts",
        &mock_raise(py, &contract_error(py, message)),
    )
}

pub fn error<'py>(py: Python<'py>, message: &str) -> Bound<'py, PyAny> {
    api(py)
        .getattr("CpuEmbedError")
        .unwrap()
        .call1((message,))
        .unwrap()
}

pub struct TestClientExit(Py<PyAny>);

impl Drop for TestClientExit {
    fn drop(&mut self) {
        Python::attach(|py| {
            self.0
                .bind(py)
                .call_method1("__exit__", (py.None(), py.None(), py.None()))
                .expect("close TestClient context");
        });
    }
}

pub fn test_client<'py>(
    py: Python<'py>,
    raise_server: bool,
) -> (Bound<'py, PyAny>, TestClientExit) {
    let app = api(py).getattr("build_app").unwrap().call0().unwrap();
    let opts = PyDict::new(py);
    opts.set_item("raise_server_exceptions", raise_server)
        .unwrap();
    let client = module(py, "starlette.testclient")
        .getattr("TestClient")
        .unwrap()
        .call((app,), Some(&opts))
        .unwrap();
    let entered = client.call_method0("__enter__").unwrap();
    (entered, TestClientExit(client.unbind()))
}

pub fn response_json(response: &Bound<'_, PyAny>) -> Value {
    py_to_json(&response.call_method0("json").unwrap())
}

/// The same context-manager response shape as `patched_urlopen`, with all
/// callbacks and assertions owned by Rust. Its bytes are fixture data.
pub fn canned_urlopen(py: Python<'_>, body: Value) -> (AttrPatch, Arc<Mutex<Value>>) {
    let bytes = serde_json::to_vec(&body).unwrap();
    let seen = Arc::new(Mutex::new(json!({})));
    let captured = Arc::clone(&seen);
    let callback =
        PyCFunction::new_closure(py, None, None, move |args, kwargs| -> PyResult<Py<PyAny>> {
            let py = args.py();
            let request = args.get_item(0)?;
            if let Ok(data) = request.getattr("data") {
                if !data.is_none() {
                    let raw: Vec<u8> = data.extract()?;
                    captured.lock().unwrap()["request"] = serde_json::from_slice(&raw).unwrap();
                }
            }
            let timeout = kwargs
                .and_then(|kwargs| kwargs.get_item("timeout").ok().flatten())
                .map(|value| value.extract::<f64>())
                .transpose()?
                .unwrap_or(0.0);
            captured.lock().unwrap()["timeout"] = json!(timeout);
            let stream = module(py, "io")
                .getattr("BytesIO")?
                .call1((PyBytes::new(py, &bytes),))?;
            Ok(stream.unbind())
        })
        .unwrap();
    let urllib = module(py, "urllib.request");
    let guard = patch(urllib.as_any(), "urlopen", callback.as_any());
    (guard, seen)
}

pub fn refused_urlopen(py: Python<'_>) -> AttrPatch {
    let reason = module(py, "urllib.error")
        .getattr("URLError")
        .unwrap()
        .call1(("connection refused",))
        .unwrap();
    patch(
        module(py, "urllib.request").as_any(),
        "urlopen",
        &mock_raise(py, &reason),
    )
}

pub fn paid_environment(case: &mut EmbedCase, dimension: &str) {
    for (name, value) in [
        ("WORKSPACE_EMBED_PAID_ENABLED", "1"),
        (
            "WORKSPACE_EMBED_PAID_BASE_URL",
            "https://embeddings.example/v1/embeddings",
        ),
        ("WORKSPACE_EMBED_PAID_API_KEY", "secret"),
        ("WORKSPACE_EMBED_PAID_MODEL", "paid-model"),
        ("WORKSPACE_EMBED_PAID_MODEL_REVISION", "provider:2026-08-23"),
        ("WORKSPACE_EMBED_PAID_COST_PER_MILLION_TOKENS", "0.02"),
    ] {
        case.set_env(name, value);
    }
    case.set_env("WORKSPACE_EMBED_PAID_DIMENSION", dimension);
}
