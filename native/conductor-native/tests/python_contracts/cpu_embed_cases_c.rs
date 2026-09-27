//! Lock, client-facing metadata, and warming contracts.

use crate::cpu_embed_support::{
    api, canned_urlopen, contract, fail_route_attempts, json_to_py, mock_return, patch, py_to_json,
    response_json, route, test_client, EmbedCase,
};
use crate::support::{assert_error, module, path};
use pyo3::prelude::*;
use pyo3::types::PyDict;
use serde_json::json;

#[test]
fn startup_lock_serializes_and_releases_a_real_file_lock() {
    let case = EmbedCase::new();
    let lock_path = case.root().join("test.lock");
    Python::attach(|py| {
        let broker = api(py);
        let _path = patch(
            broker.as_any(),
            "_runtime_lock_path",
            &mock_return(py, &path(py, &lock_path)),
        );
        let mut entered = Vec::new();
        for _ in 0..2 {
            let guard = broker
                .getattr("_startup_lock")
                .unwrap()
                .call1(("probe",))
                .unwrap();
            guard.call_method0("__enter__").unwrap();
            entered.push(true);
            assert!(lock_path.is_file());
            guard
                .call_method1("__exit__", (py.None(), py.None(), py.None()))
                .unwrap();
        }
        assert_eq!(entered, [true, true]);
    });
}

#[test]
fn read_json_url_returns_parsed_object() {
    let _case = EmbedCase::new();
    Python::attach(|py| {
        let (_patch, _) = canned_urlopen(py, json!({"ok": true}));
        let kwargs = PyDict::new(py);
        kwargs.set_item("timeout", 1.0).unwrap();
        let body = api(py)
            .getattr("_read_json_url")
            .unwrap()
            .call(("https://embeddings.example/health",), Some(&kwargs))
            .unwrap();
        assert_eq!(py_to_json(&body), json!({"ok": true}));
    });
}

#[test]
fn read_json_url_rejects_non_object_response() {
    let _case = EmbedCase::new();
    Python::attach(|py| {
        let (_patch, _) = canned_urlopen(py, json!([1, 2]));
        let kwargs = PyDict::new(py);
        kwargs.set_item("timeout", 1.0).unwrap();
        let err = api(py)
            .getattr("_read_json_url")
            .unwrap()
            .call(("https://embeddings.example/health",), Some(&kwargs))
            .unwrap_err();
        assert_error(
            py,
            err,
            &api(py).getattr("CpuEmbedError").unwrap(),
            "is not an object",
        );
    });
}

fn validate_vectors<'py>(
    py: Python<'py>,
    vectors: &Bound<'py, pyo3::types::PyAny>,
    route: &Bound<'py, pyo3::types::PyAny>,
    count: i32,
) -> PyResult<Bound<'py, pyo3::types::PyAny>> {
    api(py)
        .getattr("_validate_route_vectors")?
        .call1((vectors, route, count))
}

#[test]
fn validate_route_vectors_rejects_count_mismatch() {
    let _case = EmbedCase::new();
    Python::attach(|py| {
        let selected = route(py, &json!({"dimension": 2}));
        let err =
            validate_vectors(py, &json_to_py(py, &json!([[0.0, 0.0]])), &selected, 2).unwrap_err();
        assert_error(
            py,
            err,
            &api(py).getattr("CpuEmbedError").unwrap(),
            "returned 1 vectors; expected 2",
        );
    });
}

#[test]
fn validate_route_vectors_rejects_dimension_mismatch() {
    let _case = EmbedCase::new();
    Python::attach(|py| {
        let selected = route(py, &json!({"dimension": 2}));
        let vectors = json_to_py(py, &json!([[0.0, 0.0], [0.0, 0.0, 0.0]]));
        let err = validate_vectors(py, &vectors, &selected, 2).unwrap_err();
        assert_error(
            py,
            err,
            &api(py).getattr("CpuEmbedError").unwrap(),
            "vector 1 dimension 3 != 2",
        );
    });
}

#[test]
fn validate_route_vectors_rejects_non_finite_values() {
    let _case = EmbedCase::new();
    Python::attach(|py| {
        let selected = route(py, &json!({"dimension": 2}));
        let vectors = vec![vec![0.0_f64, f64::NAN]];
        let err = api(py)
            .getattr("_validate_route_vectors")
            .unwrap()
            .call1((vectors, selected, 1))
            .unwrap_err();
        assert_error(
            py,
            err,
            &api(py).getattr("CpuEmbedError").unwrap(),
            "vector 0 contains non-finite values",
        );
    });
}

#[test]
fn validate_route_vectors_normalizes_only_for_l2_client_routes() {
    let _case = EmbedCase::new();
    Python::attach(|py| {
        let plain = route(py, &json!({"dimension": 2, "normalization": "none"}));
        let result = api(py)
            .getattr("_validate_route_vectors")
            .unwrap()
            .call1((vec![vec![3.0, 4.0]], plain, 1))
            .unwrap();
        assert_eq!(py_to_json(&result), json!([[3.0, 4.0]]));
        let normalized = route(py, &json!({"dimension": 2, "normalization": "l2-client"}));
        let result = api(py)
            .getattr("_validate_route_vectors")
            .unwrap()
            .call1((vec![vec![3.0, 4.0]], normalized, 1))
            .unwrap();
        let values: Vec<Vec<f64>> = result.extract().unwrap();
        assert_eq!(values.len(), 1);
        assert_eq!(values[0].len(), 2);
        assert!((values[0][0] - 0.6).abs() <= 1e-6);
        assert!((values[0][1] - 0.8).abs() <= 1e-6);
    });
}

#[test]
fn resolve_model_returns_primary_route_model() {
    let _case = EmbedCase::new();
    Python::attach(|py| {
        let selected = route(py, &json!({"model": "primary-model"}));
        let _pin = crate::cpu_embed_support::pin_route(py, &selected);
        let model: String = api(py)
            .getattr("resolve_model")
            .unwrap()
            .call0()
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(model, "primary-model");
    });
}

#[test]
fn resolve_model_falls_back_to_env_or_default_on_contract_error() {
    let mut case = EmbedCase::new();
    Python::attach(|py| {
        let broker = api(py);
        let _fail = fail_route_attempts(py, "no routes configured");
        let model: String = broker
            .getattr("resolve_model")
            .unwrap()
            .call0()
            .unwrap()
            .extract()
            .unwrap();
        let default: String = broker.getattr("DEFAULT_MODEL").unwrap().extract().unwrap();
        assert_eq!(model, default);
        case.set_env("CPU_EMBED_MODEL", "legacy-model");
        let model: String = broker
            .getattr("resolve_model")
            .unwrap()
            .call0()
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(model, "legacy-model");
    });
}

#[test]
fn models_route_lists_every_attempt_as_openai_style_model() {
    let _case = EmbedCase::new();
    Python::attach(|py| {
        let (client, _client_exit) = test_client(py, true);
        let response = client.call_method1("get", ("/v1/models",)).unwrap();
        assert_eq!(
            response
                .getattr("status_code")
                .unwrap()
                .extract::<i32>()
                .unwrap(),
            200
        );
        let body = response_json(&response);
        assert_eq!(body["object"], "list");
        let rows = body["data"].as_array().unwrap();
        assert!(rows.iter().any(|row| row["id"] == "qwen3-embed-cpu"));
        let first = rows[0].as_object().unwrap();
        let keys: std::collections::BTreeSet<_> = first.keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            ["id", "object", "owned_by", "route_id", "paid"]
                .into_iter()
                .collect()
        );
        assert_eq!(first["object"], "model");
    });
}

#[test]
fn models_route_propagates_contract_error_uncaught() {
    let _case = EmbedCase::new();
    Python::attach(|py| {
        let _fail = fail_route_attempts(py, "no routes configured");
        let (client, _client_exit) = test_client(py, true);
        let err = client.call_method1("get", ("/v1/models",)).unwrap_err();
        assert_error(
            py,
            err,
            &contract(py).getattr("EmbeddingContractError").unwrap(),
            "no routes configured",
        );
    });
}

#[test]
fn warm_reports_what_the_model_load_cost() {
    let _case = EmbedCase::new();
    Python::attach(|py| {
        let broker = api(py);
        let _service = patch(
            broker.as_any(),
            "ensure_service",
            &mock_return(py, &false.into_pyobject(py).unwrap()),
        );
        let reply = json_to_py(
            py,
            &json!({"model": "qwen3-embed-cpu", "data": [{"embedding": [0.1, 0.2, 0.3]}]}),
        );
        let post = mock_return(py, &reply);
        let _post = patch(broker.as_any(), "_post_json", &post);
        let result = broker.getattr("warm").unwrap().call0().unwrap();
        let report = py_to_json(&result);
        assert_eq!(report["ok"], true);
        assert_eq!(report["dimension"], 3);
        assert_eq!(report["model"], "qwen3-embed-cpu");
        assert!(result
            .get_item("seconds")
            .unwrap()
            .is_instance(&module(py, "builtins").getattr("float").unwrap())
            .unwrap());
        let args = post.getattr("call_args").unwrap().getattr("args").unwrap();
        let url: String = args.get_item(0).unwrap().extract().unwrap();
        let expected: String = broker
            .getattr("BROKER_EMBED_URL")
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(url, expected);
        let timeout: f64 = post
            .getattr("call_args")
            .unwrap()
            .getattr("kwargs")
            .unwrap()
            .get_item("timeout_s")
            .unwrap()
            .extract()
            .unwrap();
        let budget: f64 = contract(py)
            .getattr("EMBED_TIMEOUT_SECONDS")
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(timeout, budget);
    });
}

#[test]
fn warm_fails_loud_when_the_broker_returns_no_vector() {
    let _case = EmbedCase::new();
    Python::attach(|py| {
        let broker = api(py);
        let _service = patch(
            broker.as_any(),
            "ensure_service",
            &mock_return(py, &false.into_pyobject(py).unwrap()),
        );
        let empty_vector = mock_return(py, &json_to_py(py, &json!({"data": [{"embedding": []}]})));
        let first = patch(broker.as_any(), "_post_json", &empty_vector);
        let err = broker.getattr("warm").unwrap().call0().unwrap_err();
        assert_error(
            py,
            err,
            &broker.getattr("CpuEmbedError").unwrap(),
            "empty vector",
        );
        drop(first);
        let empty_data = mock_return(py, &json_to_py(py, &json!({"data": []})));
        let _second = patch(broker.as_any(), "_post_json", &empty_data);
        let err = broker.getattr("warm").unwrap().call0().unwrap_err();
        assert_error(
            py,
            err,
            &broker.getattr("CpuEmbedError").unwrap(),
            "expected one vector",
        );
    });
}

#[test]
fn broker_side_embed_calls_budget_for_a_cold_load() {
    let _case = EmbedCase::new();
    Python::attach(|py| {
        let broker = api(py);
        let signature = module(py, "inspect").getattr("signature").unwrap();
        let expected = contract(py).getattr("EMBED_TIMEOUT_SECONDS").unwrap();
        for name in [
            "ollama_embed",
            "openai_compatible_embed",
            "embed_with_routing",
        ] {
            let function = broker.getattr(name).unwrap();
            let actual = signature
                .call1((&function,))
                .unwrap()
                .getattr("parameters")
                .unwrap()
                .get_item("timeout_s")
                .unwrap()
                .getattr("default")
                .unwrap();
            assert!(
                actual.eq(&expected).unwrap(),
                "{name} timeout default changed"
            );
        }
    });
}
