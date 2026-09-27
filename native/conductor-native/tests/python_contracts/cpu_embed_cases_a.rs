//! Route choice, GPU sharing, and broker retry contracts.

use crate::cpu_embed_support::{
    api, attempt, canned_urlopen, contract, error, json_to_py, mock_raise, mock_return,
    paid_environment, patch, pin_route, py_to_json, response_json, route, test_client, EmbedCase,
};
use crate::support::{assert_error, module, path};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList, PyTuple};
use serde_json::json;
use std::fs;

fn call_embed<'py>(py: Python<'py>, texts: &[&str]) -> PyResult<Bound<'py, pyo3::types::PyAny>> {
    api(py)
        .getattr("embed_with_routing")?
        .call1((texts.to_vec(),))
}

#[test]
fn pin_options_are_bounded() {
    let mut case = EmbedCase::new();
    Python::attach(|py| {
        case.set_env("CPU_EMBED_NUM_GPU", "0");
        let options = api(py).getattr("pin_options").unwrap().call0().unwrap();
        assert_eq!(
            options
                .get_item("num_gpu")
                .unwrap()
                .extract::<i32>()
                .unwrap(),
            0
        );
        assert_eq!(
            options
                .get_item("num_ctx")
                .unwrap()
                .extract::<i32>()
                .unwrap(),
            2048
        );
        case.set_env("CPU_EMBED_NUM_GPU", "99");
        let options = api(py).getattr("pin_options").unwrap().call0().unwrap();
        assert_eq!(
            options
                .get_item("num_gpu")
                .unwrap()
                .extract::<i32>()
                .unwrap(),
            99
        );
        case.set_env("CPU_EMBED_NUM_GPU", "8");
        let err = api(py).getattr("pin_options").unwrap().call0().unwrap_err();
        assert_error(
            py,
            err,
            &api(py).getattr("CpuEmbedError").unwrap(),
            "must be 0 or 99",
        );
    });
}

#[test]
fn choose_num_gpu_returns_zero_when_nvidia_smi_unavailable() {
    let _case = EmbedCase::new();
    Python::attach(|py| {
        let missing = module(py, "builtins")
            .getattr("OSError")
            .unwrap()
            .call1(("no nvidia-smi",))
            .unwrap();
        let subprocess = api(py).getattr("subprocess").unwrap();
        let _patch = patch(&subprocess, "check_output", &mock_raise(py, &missing));
        assert_eq!(
            api(py)
                .getattr("choose_num_gpu")
                .unwrap()
                .call0()
                .unwrap()
                .extract::<i32>()
                .unwrap(),
            0
        );
    });
}

fn busy_threshold_case(delta: i32) {
    let _case = EmbedCase::new();
    Python::attach(|py| {
        let broker = api(py);
        let busy: i32 = broker.getattr("GPU_BUSY_MIB").unwrap().extract().unwrap();
        let csv = format!("python, {}\n", busy + delta);
        let subprocess = broker.getattr("subprocess").unwrap();
        let _patch = patch(
            &subprocess,
            "check_output",
            &mock_return(py, &csv.into_pyobject(py).unwrap()),
        );
        assert_eq!(
            broker
                .getattr("choose_num_gpu")
                .unwrap()
                .call0()
                .unwrap()
                .extract::<i32>()
                .unwrap(),
            0
        );
    });
}

#[test]
fn choose_num_gpu_returns_zero_at_busy_threshold() {
    busy_threshold_case(0);
}

#[test]
fn choose_num_gpu_returns_zero_above_busy_threshold() {
    busy_threshold_case(1);
}

#[test]
fn choose_num_gpu_returns_ninety_nine_when_gpu_free() {
    let _case = EmbedCase::new();
    Python::attach(|py| {
        let broker = api(py);
        let busy: i32 = broker.getattr("GPU_BUSY_MIB").unwrap().extract().unwrap();
        let csv = format!("python, {}\n", busy - 1);
        let subprocess = broker.getattr("subprocess").unwrap();
        let _patch = patch(
            &subprocess,
            "check_output",
            &mock_return(py, &csv.into_pyobject(py).unwrap()),
        );
        assert_eq!(
            broker
                .getattr("choose_num_gpu")
                .unwrap()
                .call0()
                .unwrap()
                .extract::<i32>()
                .unwrap(),
            99
        );
    });
}

#[test]
fn choose_num_gpu_ignores_skipped_holders() {
    let _case = EmbedCase::new();
    Python::attach(|py| {
        let broker = api(py);
        let busy: i32 = broker.getattr("GPU_BUSY_MIB").unwrap().extract().unwrap();
        let csv = format!("Xorg, {}\npython, {}\n", busy * 10, busy - 1);
        let subprocess = broker.getattr("subprocess").unwrap();
        let _patch = patch(
            &subprocess,
            "check_output",
            &mock_return(py, &csv.into_pyobject(py).unwrap()),
        );
        assert_eq!(
            broker
                .getattr("choose_num_gpu")
                .unwrap()
                .call0()
                .unwrap()
                .extract::<i32>()
                .unwrap(),
            99
        );
    });
}

#[test]
fn choose_num_gpu_skips_malformed_csv_rows() {
    let _case = EmbedCase::new();
    Python::attach(|py| {
        let broker = api(py);
        let busy: i32 = broker.getattr("GPU_BUSY_MIB").unwrap().extract().unwrap();
        let csv = format!("no-comma-row\npython, not-a-number\npython, {busy}\n");
        let subprocess = broker.getattr("subprocess").unwrap();
        let _patch = patch(
            &subprocess,
            "check_output",
            &mock_return(py, &csv.into_pyobject(py).unwrap()),
        );
        assert_eq!(
            broker
                .getattr("choose_num_gpu")
                .unwrap()
                .call0()
                .unwrap()
                .extract::<i32>()
                .unwrap(),
            0
        );
    });
}

#[test]
fn openai_route_forwards_pinned_ollama_payload() {
    let mut case = EmbedCase::new();
    case.set_env("CPU_EMBED_MODEL", "qwen3-embed-cpu");
    case.set_env("CPU_EMBED_NUM_GPU", "0");
    Python::attach(|py| {
        let mut first = vec![0.0_f64; 1024];
        let mut second = vec![0.0_f64; 1024];
        first[0] = 1.0;
        second[1] = 1.0;
        let (_patch, captured) = canned_urlopen(py, json!({"embeddings": [first, second]}));
        let (client, _client_exit) = test_client(py, true);
        let kwargs = PyDict::new(py);
        kwargs
            .set_item(
                "json",
                json_to_py(py, &json!({"model": "anything", "input": ["a", "b"]})),
            )
            .unwrap();
        let response = client
            .call_method("post", ("/v1/embeddings",), Some(&kwargs))
            .unwrap();
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
        assert_eq!(body["data"].as_array().unwrap().len(), 2);
        assert_eq!(body["workspace_embedding"]["route_id"], "local-qwen3");
        assert!(body["workspace_embedding"]["fingerprint"]
            .as_str()
            .unwrap()
            .starts_with("sha256:"));
        assert_eq!(body["workspace_embedding"]["paid"], false);
        let payload = captured.lock().unwrap();
        assert_eq!(payload["request"]["options"]["num_gpu"], 0);
        assert_eq!(payload["request"]["options"]["num_ctx"], 2048);
        assert_eq!(payload["request"]["keep_alive"], 0);
    });
}

#[test]
fn rejects_non_string_input() {
    let _case = EmbedCase::new();
    Python::attach(|py| {
        let (client, _client_exit) = test_client(py, true);
        let kwargs = PyDict::new(py);
        kwargs
            .set_item("json", json_to_py(py, &json!({"input": [1, 2]})))
            .unwrap();
        let response = client
            .call_method("post", ("/v1/embeddings",), Some(&kwargs))
            .unwrap();
        assert_eq!(
            response
                .getattr("status_code")
                .unwrap()
                .extract::<i32>()
                .unwrap(),
            400
        );
        assert!(response_json(&response).get("error").is_some());
    });
}

#[test]
fn rejects_malformed_embedding_vectors() {
    let mut case = EmbedCase::new();
    case.set_env("CPU_EMBED_NUM_GPU", "0");
    Python::attach(|py| {
        let (_patch, _) = canned_urlopen(py, json!({"embeddings": [[0.0, "bad"]]}));
        let kwargs = PyDict::new(py);
        kwargs.set_item("model", "test").unwrap();
        let err = api(py)
            .getattr("ollama_embed")
            .unwrap()
            .call((vec!["x"],), Some(&kwargs))
            .unwrap_err();
        assert_error(
            py,
            err,
            &api(py).getattr("CpuEmbedError").unwrap(),
            "non-numeric",
        );
    });
}

#[test]
fn unavailable_local_route_falls_back_to_enabled_paid_route() {
    let mut case = EmbedCase::new();
    paid_environment(&mut case, "1536");
    Python::attach(|py| {
        let broker = api(py);
        let _local = patch(
            broker.as_any(),
            "ollama_embed",
            &mock_raise(py, &error(py, "local unavailable")),
        );
        let _service = patch(
            broker.as_any(),
            "ensure_ollama_service",
            &mock_return(py, &false.into_pyobject(py).unwrap()),
        );
        let mut vector = vec![0.0_f64; 1536];
        vector[0] = 1.0;
        let _paid = patch(
            broker.as_any(),
            "openai_compatible_embed",
            &mock_return(py, &json_to_py(py, &json!([vector]))),
        );
        let result = call_embed(py, &["fallback canary"]).unwrap();
        let attempt = result.getattr("attempt").unwrap();
        let route = attempt.getattr("route").unwrap();
        assert_eq!(
            route
                .getattr("route_id")
                .unwrap()
                .extract::<String>()
                .unwrap(),
            "paid-openai-compatible"
        );
        assert!(attempt.getattr("reason").unwrap().is(contract(py)
            .getattr("SelectionReason")
            .unwrap()
            .getattr("AVAILABILITY_FALLBACK")
            .unwrap()));
        assert!(route.getattr("paid").unwrap().extract::<bool>().unwrap());
    });
}

#[test]
fn quality_failure_does_not_call_local_route() {
    let mut case = EmbedCase::new();
    paid_environment(&mut case, "1536");
    Python::attach(|py| {
        let contract_api = contract(py);
        let config = contract_api
            .getattr("load_routing_config")
            .unwrap()
            .call0()
            .unwrap();
        let options = PyDict::new(py);
        options.set_item("env", PyDict::new(py)).unwrap();
        let attempts = contract_api
            .getattr("route_attempts")
            .unwrap()
            .call((config,), Some(&options))
            .unwrap();
        let fingerprint: String = attempts
            .get_item(0)
            .unwrap()
            .getattr("route")
            .unwrap()
            .getattr("fingerprint")
            .unwrap()
            .extract()
            .unwrap();
        let quality_path = case.root().join("quality.json");
        fs::write(&quality_path, json!({"schema_version": 1, "routes": {"local-qwen3": {"status": "FAIL", "route_fingerprint": fingerprint}}}).to_string()).unwrap();
        let broker = api(py);
        let local = mock_raise(
            py,
            &module(py, "builtins")
                .getattr("AssertionError")
                .unwrap()
                .call1(("quality-failed local route was called",))
                .unwrap(),
        );
        let _local = patch(broker.as_any(), "ollama_embed", &local);
        let mut vector = vec![0.0_f64; 1536];
        vector[0] = 1.0;
        let _paid = patch(
            broker.as_any(),
            "openai_compatible_embed",
            &mock_return(py, &json_to_py(py, &json!([vector]))),
        );
        let kwargs = PyDict::new(py);
        let quality = contract_api
            .getattr("load_quality_receipt")
            .unwrap()
            .call1((path(py, &quality_path),))
            .unwrap();
        kwargs.set_item("quality", quality).unwrap();
        let result = broker
            .getattr("embed_with_routing")
            .unwrap()
            .call((vec!["quality canary"],), Some(&kwargs))
            .unwrap();
        assert!(result
            .getattr("attempt")
            .unwrap()
            .getattr("reason")
            .unwrap()
            .is(contract_api
                .getattr("SelectionReason")
                .unwrap()
                .getattr("QUALITY_FALLBACK")
                .unwrap()));
        assert_eq!(
            local
                .getattr("call_count")
                .unwrap()
                .extract::<i32>()
                .unwrap(),
            0
        );
    });
}

#[test]
fn ensure_service_rejects_running_broker_with_different_route() {
    let _case = EmbedCase::new();
    Python::attach(|py| {
        let broker = api(py);
        let fake = json_to_py(
            py,
            &json!({"ok": true, "route": {"fingerprint": format!("sha256:{}", "0".repeat(64))}}),
        );
        let _patch = patch(broker.as_any(), "_read_json_url", &mock_return(py, &fake));
        let err = broker
            .getattr("ensure_service")
            .unwrap()
            .call0()
            .unwrap_err();
        assert_error(
            py,
            err,
            &broker.getattr("CpuEmbedError").unwrap(),
            "does not match",
        );
    });
}

#[test]
fn embed_with_routing_skips_route_over_cost_cap() {
    let _case = EmbedCase::new();
    Python::attach(|py| {
        let selected = route(
            py,
            &json!({"route_id": "too-expensive", "paid": true, "cost_per_million_tokens": 1_000_000_000.0, "max_request_usd": 0.000001}),
        );
        let _pin = pin_route(py, &selected);
        let err = call_embed(py, &["hello"]).unwrap_err();
        for phrase in ["too-expensive", "exceeds", "0.000001"] {
            assert!(err.to_string().contains(phrase), "{err}");
        }
    });
}

#[test]
fn embed_with_routing_rechecks_gpu_and_retries_after_ollama_restart() {
    let _case = EmbedCase::new();
    Python::attach(|py| {
        let broker = api(py);
        let selected = route(py, &json!({}));
        let _pin = pin_route(py, &selected);
        let gpu = mock_return(py, &json_to_py(py, &json!(0)));
        let _gpu_patch = patch(broker.as_any(), "choose_num_gpu", &gpu);
        let restart = mock_return(py, &json_to_py(py, &json!(true)));
        let _restart_patch = patch(broker.as_any(), "ensure_ollama_service", &restart);
        let sequence = PyList::new(
            py,
            [
                error(py, "connection refused"),
                json_to_py(py, &json!([[1.0, 0.0]])),
            ],
        )
        .unwrap();
        let kwargs = PyDict::new(py);
        kwargs.set_item("side_effect", sequence).unwrap();
        let embed = module(py, "unittest.mock")
            .getattr("Mock")
            .unwrap()
            .call((), Some(&kwargs))
            .unwrap();
        let _embed_patch = patch(broker.as_any(), "ollama_embed", &embed);
        let result = call_embed(py, &["hello"]).unwrap();
        assert_eq!(
            py_to_json(&result.getattr("vectors").unwrap()),
            json!([[1.0, 0.0]])
        );
        assert_eq!(
            embed
                .getattr("call_count")
                .unwrap()
                .extract::<i32>()
                .unwrap(),
            2
        );
        assert_eq!(
            restart
                .getattr("call_count")
                .unwrap()
                .extract::<i32>()
                .unwrap(),
            1
        );
        assert_eq!(
            gpu.getattr("call_count").unwrap().extract::<i32>().unwrap(),
            2
        );
    });
}

#[test]
fn embed_with_routing_rejects_unsupported_protocol() {
    let _case = EmbedCase::new();
    Python::attach(|py| {
        let selected = route(
            py,
            &json!({"route_id": "carrier-pigeon-route", "protocol": "carrier-pigeon"}),
        );
        let _pin = pin_route(py, &selected);
        let err = call_embed(py, &["hello"]).unwrap_err();
        for phrase in [
            "carrier-pigeon-route",
            "carrier-pigeon",
            "unsupported protocol",
        ] {
            assert!(err.to_string().contains(phrase), "{err}");
        }
    });
}

#[test]
fn embed_with_routing_aggregates_failures_from_every_route() {
    let _case = EmbedCase::new();
    Python::attach(|py| {
        let first = route(
            py,
            &json!({"route_id": "route-a", "protocol": "carrier-pigeon"}),
        );
        let second = route(
            py,
            &json!({"route_id": "route-b", "protocol": "smoke-signal"}),
        );
        let attempts = PyTuple::new(py, [attempt(py, &first), attempt(py, &second)]).unwrap();
        let _pin = patch(
            api(py).as_any(),
            "route_attempts",
            &mock_return(py, attempts.as_any()),
        );
        let err = call_embed(py, &["hello"]).unwrap_err();
        assert!(err.to_string().contains("route-a"), "{err}");
        assert!(err.to_string().contains("route-b"), "{err}");
    });
}

#[test]
fn embed_with_routing_stops_after_first_failure_when_fingerprint_required() {
    let _case = EmbedCase::new();
    Python::attach(|py| {
        let first = route(
            py,
            &json!({"route_id": "route-a", "protocol": "carrier-pigeon"}),
        );
        let second = route(py, &json!({"route_id": "route-b"}));
        let attempts = PyTuple::new(py, [attempt(py, &first), attempt(py, &second)]).unwrap();
        let broker = api(py);
        let _pin = patch(
            broker.as_any(),
            "route_attempts",
            &mock_return(py, attempts.as_any()),
        );
        let local = mock_return(py, &json_to_py(py, &json!([[1.0, 0.0]])));
        let _local = patch(broker.as_any(), "ollama_embed", &local);
        let kwargs = PyDict::new(py);
        kwargs
            .set_item(
                "required_fingerprint",
                first.getattr("fingerprint").unwrap(),
            )
            .unwrap();
        let err = broker
            .getattr("embed_with_routing")
            .unwrap()
            .call((vec!["hello"],), Some(&kwargs))
            .unwrap_err();
        assert!(err.to_string().contains("route-a"), "{err}");
        assert!(!err.to_string().contains("route-b"), "{err}");
        assert_eq!(
            local
                .getattr("call_count")
                .unwrap()
                .extract::<i32>()
                .unwrap(),
            0
        );
    });
}

#[test]
fn paid_route_rejects_duplicate_response_indexes() {
    let mut case = EmbedCase::new();
    paid_environment(&mut case, "2");
    Python::attach(|py| {
        let contract_api = contract(py);
        let config = contract_api
            .getattr("load_routing_config")
            .unwrap()
            .call0()
            .unwrap();
        let environment = json_to_py(
            py,
            &json!({
                "WORKSPACE_EMBED_PAID_ENABLED": "1",
                "WORKSPACE_EMBED_PAID_BASE_URL": "https://embeddings.example/v1/embeddings",
                "WORKSPACE_EMBED_PAID_API_KEY": "secret",
                "WORKSPACE_EMBED_PAID_MODEL": "paid-model",
                "WORKSPACE_EMBED_PAID_MODEL_REVISION": "provider:2026-08-23",
                "WORKSPACE_EMBED_PAID_DIMENSION": "2",
                "WORKSPACE_EMBED_PAID_COST_PER_MILLION_TOKENS": "0.02"
            }),
        );
        let paid = config.getattr("routes").unwrap().get_item(1).unwrap();
        let resolved = paid.call_method1("resolve", (environment,)).unwrap();
        assert!(!resolved.is_none());
        let broker = api(py);
        let reply = json_to_py(
            py,
            &json!({"data": [
                {"index": 0, "embedding": [1.0, 0.0]},
                {"index": 0, "embedding": [0.0, 1.0]}
            ]}),
        );
        let _reply = patch(broker.as_any(), "_post_json", &mock_return(py, &reply));
        let kwargs = PyDict::new(py);
        kwargs.set_item("route", resolved).unwrap();
        let err = broker
            .getattr("openai_compatible_embed")
            .unwrap()
            .call((vec!["a", "b"],), Some(&kwargs))
            .unwrap_err();
        assert_error(
            py,
            err,
            &broker.getattr("CpuEmbedError").unwrap(),
            "invalid indexes",
        );
    });
}
