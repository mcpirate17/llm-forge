//! Transport, HTTP API, and vector validation contracts.

use crate::cpu_embed_support::{
    api, canned_urlopen, contract, fail_route_attempts, json_to_py, mock_return, paid_route, patch,
    py_to_json, refused_urlopen, response_json, test_client, EmbedCase,
};
use crate::support::assert_error;
use pyo3::prelude::*;
use pyo3::types::PyDict;
use serde_json::json;

fn assert_cpu_error(py: Python<'_>, error: PyErr, text: &str) {
    assert_error(py, error, &api(py).getattr("CpuEmbedError").unwrap(), text);
}

#[test]
fn post_json_returns_parsed_object_body() {
    let _case = EmbedCase::new();
    Python::attach(|py| {
        let (_patch, _) = canned_urlopen(py, json!({"ok": true, "value": 1}));
        let kwargs = PyDict::new(py);
        kwargs.set_item("timeout_s", 1.0).unwrap();
        let body = api(py)
            .getattr("_post_json")
            .unwrap()
            .call(
                (
                    "https://embeddings.example/v1/embeddings",
                    json_to_py(py, &json!({"input": ["x"]})),
                ),
                Some(&kwargs),
            )
            .unwrap();
        assert_eq!(py_to_json(&body), json!({"ok": true, "value": 1}));
    });
}

#[test]
fn post_json_rejects_non_http_scheme() {
    let _case = EmbedCase::new();
    Python::attach(|py| {
        let kwargs = PyDict::new(py);
        kwargs.set_item("timeout_s", 1.0).unwrap();
        let err = api(py)
            .getattr("_post_json")
            .unwrap()
            .call(("file:///etc/passwd", PyDict::new(py)), Some(&kwargs))
            .unwrap_err();
        assert_cpu_error(py, err, "must use http or https");
    });
}

#[test]
fn post_json_wraps_network_error() {
    let _case = EmbedCase::new();
    Python::attach(|py| {
        let _patch = refused_urlopen(py);
        let kwargs = PyDict::new(py);
        kwargs.set_item("timeout_s", 1.0).unwrap();
        let err = api(py)
            .getattr("_post_json")
            .unwrap()
            .call(
                ("https://embeddings.example/v1/embeddings", PyDict::new(py)),
                Some(&kwargs),
            )
            .unwrap_err();
        assert_cpu_error(py, err, "embedding request to");
    });
}

#[test]
fn post_json_rejects_non_object_response() {
    let _case = EmbedCase::new();
    Python::attach(|py| {
        let (_patch, _) = canned_urlopen(py, json!([1, 2, 3]));
        let kwargs = PyDict::new(py);
        kwargs.set_item("timeout_s", 1.0).unwrap();
        let err = api(py)
            .getattr("_post_json")
            .unwrap()
            .call(
                ("https://embeddings.example/v1/embeddings", PyDict::new(py)),
                Some(&kwargs),
            )
            .unwrap_err();
        assert_cpu_error(py, err, "is not an object");
    });
}

#[test]
fn health_route_reports_primary_route_metadata() {
    let mut case = EmbedCase::new();
    case.set_env("CPU_EMBED_NUM_GPU", "0");
    Python::attach(|py| {
        let (client, _client_exit) = test_client(py, true);
        let response = client.call_method1("get", ("/health",)).unwrap();
        assert_eq!(
            response
                .getattr("status_code")
                .unwrap()
                .extract::<i32>()
                .unwrap(),
            200
        );
        let body = response_json(&response);
        assert_eq!(body["ok"], true);
        assert_eq!(body["num_gpu"], 0);
        assert_eq!(body["route"]["route_id"], "local-qwen3");
        let primary: String = contract(py)
            .getattr("SelectionReason")
            .unwrap()
            .getattr("PRIMARY")
            .unwrap()
            .getattr("value")
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(body["selection_reason"], primary);
    });
}

#[test]
fn health_route_returns_503_when_routing_fails() {
    let _case = EmbedCase::new();
    Python::attach(|py| {
        let _route = fail_route_attempts(py, "no routes configured");
        let (client, _client_exit) = test_client(py, true);
        let response = client.call_method1("get", ("/health",)).unwrap();
        assert_eq!(
            response
                .getattr("status_code")
                .unwrap()
                .extract::<i32>()
                .unwrap(),
            503
        );
        let body = response_json(&response);
        assert_eq!(body["ok"], false);
        assert!(body["error"]
            .as_str()
            .unwrap()
            .contains("no routes configured"));
    });
}

#[test]
fn embed_with_routing_rejects_empty_texts() {
    let _case = EmbedCase::new();
    Python::attach(|py| {
        let err = api(py)
            .getattr("embed_with_routing")
            .unwrap()
            .call1((Vec::<String>::new(),))
            .unwrap_err();
        assert_cpu_error(py, err, "must not be empty");
    });
}

#[test]
fn embed_with_routing_wraps_contract_error() {
    let _case = EmbedCase::new();
    Python::attach(|py| {
        let _route = fail_route_attempts(py, "pinned index has no route");
        let err = api(py)
            .getattr("embed_with_routing")
            .unwrap()
            .call1((vec!["hello"],))
            .unwrap_err();
        assert_cpu_error(py, err, "pinned index has no route");
    });
}

#[test]
fn choose_num_gpu_rejects_non_integer_forced_value() {
    let mut case = EmbedCase::new();
    case.set_env("CPU_EMBED_NUM_GPU", "not-a-number");
    Python::attach(|py| {
        let err = api(py)
            .getattr("choose_num_gpu")
            .unwrap()
            .call0()
            .unwrap_err();
        assert_cpu_error(py, err, "must be 0 or 99");
    });
}

#[test]
fn pin_options_rejects_invalid_explicit_num_gpu() {
    let _case = EmbedCase::new();
    Python::attach(|py| {
        let err = api(py)
            .getattr("pin_options")
            .unwrap()
            .call1((7,))
            .unwrap_err();
        assert_cpu_error(py, err, "num_gpu must be 0 or 99");
    });
}

#[test]
fn ollama_embed_returns_empty_for_no_texts() {
    let _case = EmbedCase::new();
    Python::attach(|py| {
        let kwargs = PyDict::new(py);
        kwargs.set_item("model", "test").unwrap();
        let vectors = api(py)
            .getattr("ollama_embed")
            .unwrap()
            .call((Vec::<String>::new(),), Some(&kwargs))
            .unwrap();
        assert_eq!(py_to_json(&vectors), json!([]));
    });
}

#[test]
fn ollama_embed_wraps_network_error() {
    let _case = EmbedCase::new();
    Python::attach(|py| {
        let _patch = refused_urlopen(py);
        let kwargs = PyDict::new(py);
        kwargs.set_item("model", "test").unwrap();
        let err = api(py)
            .getattr("ollama_embed")
            .unwrap()
            .call((vec!["x"],), Some(&kwargs))
            .unwrap_err();
        assert_cpu_error(py, err, "ollama embed failed");
    });
}

fn reject_ollama_body(body: serde_json::Value, texts: &[&str], message: &str) {
    let mut case = EmbedCase::new();
    case.set_env("CPU_EMBED_NUM_GPU", "0");
    Python::attach(|py| {
        let (_patch, _) = canned_urlopen(py, body);
        let kwargs = PyDict::new(py);
        kwargs.set_item("model", "test").unwrap();
        let err = api(py)
            .getattr("ollama_embed")
            .unwrap()
            .call((texts.to_vec(),), Some(&kwargs))
            .unwrap_err();
        assert_cpu_error(py, err, message);
    });
}

#[test]
fn ollama_embed_rejects_wrong_vector_count() {
    reject_ollama_body(
        json!({"embeddings": [[1.0, 0.0]]}),
        &["a", "b"],
        "wrong vector count",
    );
}

#[test]
fn ollama_embed_rejects_empty_vector() {
    reject_ollama_body(json!({"embeddings": [[]]}), &["a"], "not a non-empty array");
}

#[test]
fn l2_normalize_rejects_zero_vector() {
    let _case = EmbedCase::new();
    Python::attach(|py| {
        let kwargs = PyDict::new(py);
        kwargs.set_item("route_id", "test-route").unwrap();
        let err = api(py)
            .getattr("_l2_normalize")
            .unwrap()
            .call((vec![0.0, 0.0],), Some(&kwargs))
            .unwrap_err();
        assert_cpu_error(py, err, "zero or non-finite");
    });
}

#[test]
fn openai_compatible_embed_requires_credential_env() {
    let _case = EmbedCase::new();
    Python::attach(|py| {
        let kwargs = PyDict::new(py);
        kwargs
            .set_item("route", paid_route(py, &json!({})))
            .unwrap();
        let err = api(py)
            .getattr("openai_compatible_embed")
            .unwrap()
            .call((vec!["a"],), Some(&kwargs))
            .unwrap_err();
        assert_cpu_error(py, err, "missing credential env");
    });
}

fn paid_response_case(body: serde_json::Value, texts: &[&str], message: &str) {
    let mut case = EmbedCase::new();
    case.set_env("TEST_PAID_API_KEY", "key");
    Python::attach(|py| {
        let broker = api(py);
        let _reply = patch(
            broker.as_any(),
            "_post_json",
            &mock_return(py, &json_to_py(py, &body)),
        );
        let kwargs = PyDict::new(py);
        kwargs
            .set_item("route", paid_route(py, &json!({})))
            .unwrap();
        let err = broker
            .getattr("openai_compatible_embed")
            .unwrap()
            .call((texts.to_vec(),), Some(&kwargs))
            .unwrap_err();
        assert_cpu_error(py, err, message);
    });
}

#[test]
fn openai_compatible_embed_rejects_non_list_data() {
    paid_response_case(json!({"data": "nope"}), &["a"], "lacks data");
}

#[test]
fn openai_compatible_embed_rejects_row_missing_embedding() {
    paid_response_case(
        json!({"data": [{"index": 0}]}),
        &["a"],
        "lacks an embedding",
    );
}

#[test]
fn openai_compatible_embed_rejects_invalid_index() {
    paid_response_case(
        json!({"data": [{"index": "zero", "embedding": [1.0]}]}),
        &["a"],
        "has invalid index",
    );
}

#[test]
fn openai_compatible_embed_rejects_non_numeric_value() {
    paid_response_case(
        json!({"data": [{"index": 0, "embedding": [1.0, "bad"]}]}),
        &["a"],
        "is not numeric",
    );
}

#[test]
fn openai_compatible_embed_returns_vectors_ordered_by_index() {
    let mut case = EmbedCase::new();
    case.set_env("TEST_PAID_API_KEY", "key");
    Python::attach(|py| {
        let broker = api(py);
        let reply = json!({"data": [
            {"index": 1, "embedding": [0.0, 1.0]},
            {"index": 0, "embedding": [1.0, 0.0]}
        ]});
        let _reply = patch(
            broker.as_any(),
            "_post_json",
            &mock_return(py, &json_to_py(py, &reply)),
        );
        let kwargs = PyDict::new(py);
        kwargs
            .set_item("route", paid_route(py, &json!({})))
            .unwrap();
        let vectors = broker
            .getattr("openai_compatible_embed")
            .unwrap()
            .call((vec!["a", "b"],), Some(&kwargs))
            .unwrap();
        assert_eq!(py_to_json(&vectors), json!([[1.0, 0.0], [0.0, 1.0]]));
    });
}
