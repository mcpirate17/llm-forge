#![cfg(feature = "python-compat-tests")]
//! Rust assertions for provider-neutral embedding route contracts.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::exceptions::PyTypeError;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyModule};
use pyo3::PyTypeInfo;
use serde_json::{json, Value};
use support::{assert_error, attr_bool, attr_text, module, path, Case};

const ROUTE_TOML_FIELDS: &str = "\
backend_kind = \"local\"\n\
protocol = \"ollama-embed\"\n\
priority = 1\n\
normalization = \"l2-client\"\n\
tokenizer_policy = \"backend-default\"\n\
truncation_policy = \"unicode-prefix\"\n\
max_input_chars = 100\n\
device_policy = \"cpu\"\n\
paid = false\n";

fn py_json<'py>(py: Python<'py>, value: &Value) -> Bound<'py, PyAny> {
    PyModule::import(py, "json")
        .unwrap()
        .getattr("loads")
        .unwrap()
        .call1((value.to_string(),))
        .unwrap()
}

fn kwargs<'py>(py: Python<'py>, value: &Value) -> Bound<'py, PyDict> {
    py_json(py, value).cast::<PyDict>().unwrap().clone()
}

fn spec<'py>(
    py: Python<'py>,
    contract: &Bound<'py, PyModule>,
    overrides: Value,
) -> Bound<'py, PyAny> {
    let mut fields = json!({
        "route_id":"r", "backend_kind":"local", "protocol":"ollama-embed",
        "priority":1, "normalization":"l2-client", "tokenizer_policy":"backend-default",
        "truncation_policy":"unicode-prefix", "max_input_chars":100,
        "device_policy":"cpu", "paid":false, "endpoint":"http://127.0.0.1:9/embed",
        "model":"m", "model_revision":"m:1", "dimension":8
    });
    for (key, value) in overrides.as_object().unwrap() {
        fields
            .as_object_mut()
            .unwrap()
            .insert(key.clone(), value.clone());
    }
    contract
        .getattr("RouteSpec")
        .unwrap()
        .call((), Some(&kwargs(py, &fields)))
        .unwrap()
}

fn contract_error(
    py: Python<'_>,
    contract: &Bound<'_, PyModule>,
    result: PyResult<Bound<'_, PyAny>>,
    message: &str,
) {
    assert_error(
        py,
        result.expect_err("expected contract error"),
        &contract.getattr("EmbeddingContractError").unwrap(),
        message,
    );
}

fn routing<'py>(contract: &Bound<'py, PyModule>) -> Bound<'py, PyAny> {
    contract
        .getattr("load_routing_config")
        .unwrap()
        .call0()
        .unwrap()
}

fn attempts<'py>(
    py: Python<'py>,
    contract: &Bound<'py, PyModule>,
    options: Value,
) -> PyResult<Bound<'py, PyAny>> {
    contract
        .getattr("route_attempts")
        .unwrap()
        .call((routing(contract),), Some(&kwargs(py, &options)))
}

fn local_fingerprint(py: Python<'_>, contract: &Bound<'_, PyModule>) -> String {
    attr_text(
        &attempts(py, contract, json!({"env":{}}))
            .unwrap()
            .get_item(0)
            .unwrap()
            .getattr("route")
            .unwrap(),
        "fingerprint",
    )
}

fn paid_env() -> Value {
    json!({
        "WORKSPACE_EMBED_PAID_ENABLED":"1",
        "WORKSPACE_EMBED_PAID_BASE_URL":"https://embeddings.example/v1/embeddings",
        "WORKSPACE_EMBED_PAID_API_KEY":"secret",
        "WORKSPACE_EMBED_PAID_MODEL":"paid-model",
        "WORKSPACE_EMBED_PAID_MODEL_REVISION":"provider:2026-08-23",
        "WORKSPACE_EMBED_PAID_DIMENSION":"1536",
        "WORKSPACE_EMBED_PAID_COST_PER_MILLION_TOKENS":"0.02"
    })
}

fn receipt(case: &Case, name: &str, content: Value) -> std::path::PathBuf {
    case.write(name, &content.to_string())
}

fn quality<'py>(
    py: Python<'py>,
    contract: &Bound<'py, PyModule>,
    file: &std::path::Path,
) -> PyResult<Bound<'py, PyAny>> {
    contract
        .getattr("load_quality_receipt")
        .unwrap()
        .call1((path(py, file),))
}

#[test]
fn resolve_enforces_required_fields_dimension_and_cost() {
    let _case = Case::new();
    Python::attach(|py| {
        let contract = module(py, "conductor.embedding_contract");
        let empty = kwargs(py, &json!({}));
        let missing = spec(py, &contract, json!({"endpoint":"","model":""}));
        contract_error(
            py,
            &contract,
            missing.call_method1("resolve", (&empty,)),
            "missing required fields",
        );
        let paid_missing = spec(py, &contract, json!({"paid":true,"endpoint":"","model":""}));
        assert!(paid_missing
            .call_method1("resolve", (&empty,))
            .unwrap()
            .is_none());
        let key = spec(py, &contract, json!({"paid":true,"api_key_env":"X_KEY"}));
        assert!(key.call_method1("resolve", (&empty,)).unwrap().is_none());
        let dimension = spec(py, &contract, json!({"dimension":0,"dimension_env":"DIM"}));
        for (value, message) in [("abc", "must be an integer"), ("0", "must be positive")] {
            contract_error(
                py,
                &contract,
                dimension.call_method1("resolve", (kwargs(py, &json!({"DIM":value})),)),
                message,
            );
        }
        let cost = spec(
            py,
            &contract,
            json!({"paid":true,"endpoint":"https://e.example/v1","cost_per_million_tokens_env":"COST"}),
        );
        assert!(cost.call_method1("resolve", (&empty,)).unwrap().is_none());
        for (value, message) in [("free", "cost must be numeric"), ("-1", "non-negative")] {
            contract_error(
                py,
                &contract,
                cost.call_method1("resolve", (kwargs(py, &json!({"COST":value})),)),
                message,
            );
        }
    });
}

#[test]
fn resolve_dimension_boundary_zero_raises() {
    let _case = Case::new();
    Python::attach(|py| {
        let contract = module(py, "conductor.embedding_contract");
        let route = spec(py, &contract, json!({"dimension":0,"dimension_env":"DIM"}));
        contract_error(
            py,
            &contract,
            route.call_method1("resolve", (kwargs(py, &json!({"DIM":"0"})),)),
            "must be positive",
        );
    });
}

#[test]
fn resolve_dimension_boundary_one_resolves() {
    let _case = Case::new();
    Python::attach(|py| {
        let contract = module(py, "conductor.embedding_contract");
        let route = spec(py, &contract, json!({"dimension":0,"dimension_env":"DIM"}));
        assert!(!route
            .call_method1("resolve", (kwargs(py, &json!({"DIM":"1"})),))
            .unwrap()
            .is_none());
    });
}

#[test]
fn by_id_maps_route_id_to_spec() {
    let _case = Case::new();
    Python::attach(|py| {
        let contract = module(py, "conductor.embedding_contract");
        let mapping = routing(&contract).call_method0("by_id").unwrap();
        assert_eq!(
            attr_text(&mapping.get_item("local-qwen3").unwrap(), "route_id"),
            "local-qwen3"
        );
    });
}

#[test]
fn validate_endpoint_rejects_bad_scheme_and_non_https_paid() {
    let _case = Case::new();
    Python::attach(|py| {
        let contract = module(py, "conductor.embedding_contract");
        let validate = contract.getattr("_validate_endpoint").unwrap();
        contract_error(
            py,
            &contract,
            validate.call(
                ("r", "not-a-url"),
                Some(&kwargs(py, &json!({"paid":false}))),
            ),
            "absolute HTTP",
        );
        contract_error(
            py,
            &contract,
            validate.call(
                ("r", "http://e.example/v1"),
                Some(&kwargs(py, &json!({"paid":true}))),
            ),
            "must use HTTPS",
        );
    });
}

#[test]
fn load_routing_config_rejects_malformed_variants() {
    let case = Case::new();
    Python::attach(|py| {
        let contract = module(py, "conductor.embedding_contract");
        let load = contract.getattr("load_routing_config").unwrap();
        let missing = case.root().join("missing.toml");
        contract_error(
            py,
            &contract,
            load.call1((path(py, &missing),)),
            "unreadable",
        );
        let version: i64 = contract
            .getattr("CONTRACT_VERSION")
            .unwrap()
            .extract()
            .unwrap();
        let variants = [
            ("unsupported.toml", "schema_version = 99\n".to_owned(), "unsupported"),
            ("no_policy.toml", format!("schema_version = {version}\n"), "require"),
            ("bad_route.toml", format!("schema_version = {version}\n[policy]\nprimary_route = \"r\"\nfallback_routes = []\n[[route]]\nroute_id = \"r\"\n"), "invalid"),
            ("dup_ids.toml", format!("schema_version = {version}\n[policy]\nprimary_route = \"dup\"\nfallback_routes = []\n{block}{block}", block=format!("[[route]]\nroute_id = \"dup\"\n{ROUTE_TOML_FIELDS}")), "unique"),
            ("missing_ref.toml", format!("schema_version = {version}\n[policy]\nprimary_route = \"ghost\"\nfallback_routes = []\n[[route]]\nroute_id = \"real\"\n{ROUTE_TOML_FIELDS}"), "missing routes"),
        ];
        for (name, content, message) in variants {
            let file = case.write(name, &content);
            contract_error(py, &contract, load.call1((path(py, &file),)), message);
        }
    });
}

#[test]
fn load_quality_receipt_rejects_malformed_variants() {
    let case = Case::new();
    Python::attach(|py| {
        let contract = module(py, "conductor.embedding_contract");
        let bad_json = case.write("bad.json", "not json");
        contract_error(
            py,
            &contract,
            quality(py, &contract, &bad_json),
            "unreadable",
        );
        let version: i64 = contract
            .getattr("QUALITY_SCHEMA_VERSION")
            .unwrap()
            .extract()
            .unwrap();
        let fp = format!("sha256:{}", "0".repeat(64));
        let variants = [
            (
                "bad_schema.json",
                json!({"schema_version":999}),
                "schema is invalid",
            ),
            (
                "no_routes.json",
                json!({"schema_version":version}),
                "routes are missing",
            ),
            (
                "bad_result_type.json",
                json!({"schema_version":version,"routes":{"r":"not-a-dict"}}),
                "route result is invalid",
            ),
            (
                "bad_status.json",
                json!({"schema_version":version,"routes":{"r":{"status":"NOT_A_STATUS","route_fingerprint":fp}}}),
                "is invalid",
            ),
            (
                "bad_fingerprint.json",
                json!({"schema_version":version,"routes":{"r":{"status":"FAIL","route_fingerprint":"not-sha256"}}}),
                "invalid route fingerprint",
            ),
            (
                "fail_above_threshold.json",
                json!({"schema_version":version,"routes":{"r":{"status":"FAIL","route_fingerprint":fp,"score":0.9,"threshold":0.8}}}),
                "claims FAIL at/above threshold",
            ),
        ];
        for (name, content, message) in variants {
            let file = receipt(&case, name, content);
            contract_error(py, &contract, quality(py, &contract, &file), message);
        }
    });
}

#[test]
fn optional_float_rejects_non_numeric_and_bool() {
    let _case = Case::new();
    Python::attach(|py| {
        let contract = module(py, "conductor.embedding_contract");
        let optional = contract.getattr("_optional_float").unwrap();
        assert!(optional.call1((py.None(),)).unwrap().is_none());
        assert_eq!(optional.call1((3,)).unwrap().extract::<f64>().unwrap(), 3.0);
        for invalid in [json!("nope"), json!(true)] {
            let error = optional
                .call1((py_json(py, &invalid),))
                .expect_err("numeric type error");
            assert_error(
                py,
                error,
                PyTypeError::type_object(py).as_any(),
                "expected numeric",
            );
        }
    });
}

#[test]
fn route_attempts_raises_when_fingerprint_matches_no_route() {
    let _case = Case::new();
    Python::attach(|py| {
        let contract = module(py, "conductor.embedding_contract");
        contract_error(
            py,
            &contract,
            attempts(
                py,
                &contract,
                json!({"required_fingerprint":format!("sha256:{}", "f".repeat(64)), "env":{}}),
            ),
            "no configured embedding route matches",
        );
    });
}

#[test]
fn default_route_is_local_and_provider_neutral() {
    let _case = Case::new();
    Python::attach(|py| {
        let contract = module(py, "conductor.embedding_contract");
        let attempts = attempts(py, &contract, json!({"env":{}})).unwrap();
        assert_eq!(attempts.len().unwrap(), 1);
        let route = attempts.get_item(0).unwrap().getattr("route").unwrap();
        assert_eq!(attr_text(&route, "route_id"), "local-qwen3");
        assert_eq!(attr_text(&route, "protocol"), "ollama-embed");
        assert!(!attr_bool(&route, "paid"));
        assert!(attr_text(&route, "fingerprint").starts_with("sha256:"));
        assert!(!route
            .call_method0("public_metadata")
            .unwrap()
            .contains("api_key")
            .unwrap());
    });
}

#[test]
fn quality_failure_selects_explicit_paid_fallback() {
    let case = Case::new();
    Python::attach(|py| {
        let contract = module(py, "conductor.embedding_contract");
        let file = receipt(
            &case,
            "quality.json",
            json!({"schema_version":1,"routes":{"local-qwen3":{
            "status":"FAIL","route_fingerprint":local_fingerprint(py, &contract),"score":0.61,
            "threshold":0.75,"fixture_sha256":"a".repeat(64)}}}),
        );
        let quality = quality(py, &contract, &file).unwrap();
        let options = PyDict::new(py);
        options.set_item("quality", quality).unwrap();
        options.set_item("env", py_json(py, &paid_env())).unwrap();
        let attempts = contract
            .getattr("route_attempts")
            .unwrap()
            .call((routing(&contract),), Some(&options))
            .unwrap();
        assert_eq!(attempts.len().unwrap(), 1);
        let first = attempts.get_item(0).unwrap();
        let route = first.getattr("route").unwrap();
        assert_eq!(attr_text(&route, "route_id"), "paid-openai-compatible");
        assert!(first.getattr("reason").unwrap().is(contract
            .getattr("SelectionReason")
            .unwrap()
            .getattr("QUALITY_FALLBACK")
            .unwrap()));
        assert!(attr_bool(&route, "paid"));
    });
}

#[test]
fn paid_route_is_disabled_without_explicit_configuration() {
    let case = Case::new();
    Python::attach(|py| {
        let contract = module(py, "conductor.embedding_contract");
        let file = receipt(
            &case,
            "quality.json",
            json!({"schema_version":1,"routes":{"local-qwen3":{
            "status":"FAIL","route_fingerprint":local_fingerprint(py, &contract)}}}),
        );
        let options = PyDict::new(py);
        options
            .set_item("quality", quality(py, &contract, &file).unwrap())
            .unwrap();
        options.set_item("env", PyDict::new(py)).unwrap();
        contract_error(
            py,
            &contract,
            contract
                .getattr("route_attempts")
                .unwrap()
                .call((routing(&contract),), Some(&options)),
            "no approved",
        );
    });
}

#[test]
fn pinned_index_never_switches_vector_spaces() {
    let _case = Case::new();
    Python::attach(|py| {
        let contract = module(py, "conductor.embedding_contract");
        let options = kwargs(
            py,
            &json!({"required_fingerprint":local_fingerprint(py, &contract),"env":paid_env()}),
        );
        let attempts = contract
            .getattr("route_attempts")
            .unwrap()
            .call((routing(&contract),), Some(&options))
            .unwrap();
        assert_eq!(attempts.len().unwrap(), 1);
        let first = attempts.get_item(0).unwrap();
        assert_eq!(
            attr_text(&first.getattr("route").unwrap(), "route_id"),
            "local-qwen3"
        );
        assert!(first.getattr("reason").unwrap().is(contract
            .getattr("SelectionReason")
            .unwrap()
            .getattr("PINNED_INDEX")
            .unwrap()));
    });
}

#[test]
fn rejects_non_loopback_local_endpoint() {
    let case = Case::new();
    Python::attach(|py| {
        let contract = module(py, "conductor.embedding_contract");
        let content = format!("schema_version = 1\n[policy]\nprimary_route = \"bad\"\nfallback_routes = []\n[[route]]\nroute_id = \"bad\"\n{ROUTE_TOML_FIELDS}endpoint = \"http://example.com/embed\"\nmodel = \"x\"\nmodel_revision = \"x:1\"\ndimension = 2\n");
        let file = case.write("routes.toml", content.trim());
        let config = contract
            .getattr("load_routing_config")
            .unwrap()
            .call1((path(py, &file),))
            .unwrap();
        contract_error(
            py,
            &contract,
            contract
                .getattr("route_attempts")
                .unwrap()
                .call((config,), Some(&kwargs(py, &json!({"env":{}})))),
            "loopback",
        );
    });
}

#[test]
fn quality_receipt_cannot_claim_pass_below_threshold() {
    let case = Case::new();
    Python::attach(|py| {
        let contract = module(py, "conductor.embedding_contract");
        let file = receipt(
            &case,
            "quality.json",
            json!({"schema_version":1,"routes":{"local-qwen3":{
            "status":"PASS","route_fingerprint":local_fingerprint(py, &contract),"score":0.5,"threshold":0.8}}}),
        );
        contract_error(
            py,
            &contract,
            quality(py, &contract, &file),
            "below threshold",
        );
    });
}

#[test]
fn quality_receipt_is_bound_to_exact_route_fingerprint() {
    let case = Case::new();
    Python::attach(|py| {
        let contract = module(py, "conductor.embedding_contract");
        let file = receipt(
            &case,
            "quality.json",
            json!({"schema_version":1,"routes":{"local-qwen3":{
            "status":"FAIL","route_fingerprint":format!("sha256:{}", "0".repeat(64))}}}),
        );
        let options = PyDict::new(py);
        options
            .set_item("quality", quality(py, &contract, &file).unwrap())
            .unwrap();
        options.set_item("env", py_json(py, &paid_env())).unwrap();
        contract_error(
            py,
            &contract,
            contract
                .getattr("route_attempts")
                .unwrap()
                .call((routing(&contract),), Some(&options)),
            "is bound to",
        );
    });
}
