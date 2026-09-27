#![cfg(feature = "python-compat-tests")]
//! Rust-owned PyO3 contracts for the CRG workspace embedding provider.

#[path = "python_contracts/agent_comm_support.rs"]
#[allow(dead_code)]
mod comm_support;
#[path = "python_contracts/crg_server_support.rs"]
#[allow(dead_code)]
mod server_support;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use comm_support::{bind_signature, py_json, signature};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyCFunction, PyDict, PyList, PyModule};
use serde_json::{json, Value};
use server_support::{fake_crg, no_arg, no_service, signature_with_kwargs, version};
use sha2::{Digest, Sha256};
use std::fs;
use std::sync::{Arc, Mutex};
use support::{assert_error, module, AttrPatch, Case};

fn bridge(py: Python<'_>) -> Bound<'_, PyModule> {
    module(py, "conductor.crg_embedding_bridge")
}

fn bridge_error(py: Python<'_>, result: PyResult<Bound<'_, PyAny>>, part: &str) {
    assert_error(
        py,
        result.unwrap_err(),
        &bridge(py).getattr("CrgBridgeError").unwrap(),
        part,
    );
}

fn provider<'py>(_py: Python<'py>, bridge: &Bound<'py, PyModule>) -> Bound<'py, PyAny> {
    bridge
        .getattr("WorkspaceEmbeddingProvider")
        .unwrap()
        .call0()
        .unwrap()
}

fn batch<'py>(
    py: Python<'py>,
    bridge: &Bound<'py, PyModule>,
    vectors: Value,
    metadata: Value,
) -> Bound<'py, PyAny> {
    bridge
        .getattr("EmbeddingBatch")
        .unwrap()
        .call1((py_json(py, vectors), py_json(py, metadata)))
        .unwrap()
}

#[test]
fn source_sha256_hashes_or_rejects_missing_file() {
    let case = Case::new();
    let source = case.write("m.py", "X = 1\n");
    Python::attach(|py| {
        let bridge = bridge(py);
        let bare = module(py, "builtins")
            .getattr("object")
            .unwrap()
            .call0()
            .unwrap();
        bridge_error(
            py,
            bridge.getattr("_source_sha256").unwrap().call1((bare,)),
            "no source path",
        );
        let kwargs = PyDict::new(py);
        kwargs
            .set_item("__file__", source.to_str().unwrap())
            .unwrap();
        let fake = module(py, "types")
            .getattr("SimpleNamespace")
            .unwrap()
            .call((), Some(&kwargs))
            .unwrap();
        let actual: String = bridge
            .getattr("_source_sha256")
            .unwrap()
            .call1((fake,))
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(actual, format!("{:x}", Sha256::digest(b"X = 1\n")));
    });
}

#[test]
fn assert_supported_crg_validates_version_hashes_and_symbols() {
    let _case = Case::new();
    Python::attach(|py| {
        let bridge = bridge(py);
        let metadata = bridge
            .getattr("importlib")
            .unwrap()
            .getattr("metadata")
            .unwrap();
        let _wrong_version =
            AttrPatch::replace(&metadata, "version", version(py, "9.9.9").as_any());
        bridge_error(
            py,
            bridge.getattr("assert_supported_crg").unwrap().call0(),
            "version",
        );
        let expected_version: String = bridge
            .getattr("EXPECTED_CRG_VERSION")
            .unwrap()
            .extract()
            .unwrap();
        let _right_version = AttrPatch::replace(
            &metadata,
            "version",
            version(py, &expected_version).as_any(),
        );
        let (embeddings, main, _modules) = fake_crg(py);
        let hash_signature = signature(py, &["module"], &[]);
        let wrong_hash =
            PyCFunction::new_closure(py, None, None, move |args, kwargs| -> PyResult<String> {
                bind_signature(&hash_signature, args, kwargs)?;
                Ok("deadbeef".repeat(8))
            })
            .unwrap();
        let _hash = AttrPatch::replace(bridge.as_any(), "_source_sha256", wrong_hash.as_any());
        bridge_error(
            py,
            bridge.getattr("assert_supported_crg").unwrap().call0(),
            "source hash drift",
        );
        let hash_signature = signature(py, &["module"], &[]);
        let embeddings_ref = embeddings.clone().unbind();
        let main_ref = main.clone().unbind();
        let expected_embeddings: String = bridge
            .getattr("EXPECTED_EMBEDDINGS_SHA256")
            .unwrap()
            .extract()
            .unwrap();
        let expected_main: String = bridge
            .getattr("EXPECTED_MAIN_SHA256")
            .unwrap()
            .extract()
            .unwrap();
        let matching =
            PyCFunction::new_closure(py, None, None, move |args, kwargs| -> PyResult<String> {
                let bound = bind_signature(&hash_signature, args, kwargs)?;
                let target = bound.getattr("arguments")?.get_item("module")?;
                if target.is(embeddings_ref.bind(args.py())) {
                    Ok(expected_embeddings.clone())
                } else if target.is(main_ref.bind(args.py())) {
                    Ok(expected_main.clone())
                } else {
                    Err(pyo3::exceptions::PyAssertionError::new_err(
                        "unexpected CRG module",
                    ))
                }
            })
            .unwrap();
        let _matching = AttrPatch::replace(bridge.as_any(), "_source_sha256", matching.as_any());
        bridge_error(
            py,
            bridge.getattr("assert_supported_crg").unwrap().call0(),
            "lacks",
        );
        embeddings.add("get_provider", py.None()).unwrap();
        embeddings
            .add(
                "EmbeddingStore",
                module(py, "builtins").getattr("object").unwrap(),
            )
            .unwrap();
        embeddings
            .add(
                "EmbeddingProvider",
                module(py, "builtins").getattr("object").unwrap(),
            )
            .unwrap();
        bridge
            .getattr("assert_supported_crg")
            .unwrap()
            .call0()
            .unwrap();
    });
}

#[test]
fn install_bridge_replaces_get_provider() {
    let _case = Case::new();
    Python::attach(|py| {
        let bridge = bridge(py);
        let _supported =
            AttrPatch::replace(bridge.as_any(), "assert_supported_crg", no_arg(py).as_any());
        let (embeddings, _main, _modules) = fake_crg(py);
        bridge.getattr("install_bridge").unwrap().call0().unwrap();
        let get_provider = embeddings.getattr("get_provider").unwrap();
        assert!(!get_provider.is_none());
        let kwargs = PyDict::new(py);
        kwargs.set_item("provider", "cloud").unwrap();
        bridge_error(py, get_provider.call((), Some(&kwargs)), "disabled");
        let _service = no_service(py, &bridge);
        let actual = get_provider.call0().unwrap();
        assert!(actual
            .is_instance(&bridge.getattr("WorkspaceEmbeddingProvider").unwrap())
            .unwrap());
    });
}

#[test]
fn init_raises_when_requested_model_does_not_match_route() {
    let _case = Case::new();
    Python::attach(|py| {
        let bridge = bridge(py);
        let _service = no_service(py, &bridge);
        let kwargs = PyDict::new(py);
        kwargs
            .set_item("requested_model", "totally-different-model")
            .unwrap();
        bridge_error(
            py,
            bridge
                .getattr("WorkspaceEmbeddingProvider")
                .unwrap()
                .call((), Some(&kwargs)),
            "does not match selected",
        );
    });
}

#[test]
fn embed_validates_batch_size_and_response_integrity() {
    let _case = Case::new();
    Python::attach(|py| {
        let bridge = bridge(py);
        let _service = no_service(py, &bridge);
        let provider = provider(py, &bridge);
        let empty = provider
            .call_method1("embed", (Vec::<String>::new(),))
            .unwrap();
        assert!(empty.cast::<PyList>().is_ok());
        assert!(empty.eq(PyList::empty(py)).unwrap());
        let dimension: usize = provider.getattr("dimension").unwrap().extract().unwrap();
        let fingerprint: String = provider
            .getattr("_route")
            .unwrap()
            .getattr("fingerprint")
            .unwrap()
            .extract()
            .unwrap();
        let expected = signature_with_kwargs(py, "texts");
        let bridge_ref = bridge.clone().unbind();
        let wrong_fingerprint = PyCFunction::new_closure(py, None, None, move |args, kwargs| -> PyResult<Py<PyAny>> {
            let bound = bind_signature(&expected, args, kwargs)?;
            let texts: Vec<String> = bound.getattr("arguments")?.get_item("texts")?.extract()?;
            let vectors = json!(vec![vec![0.0; dimension]; texts.len()]);
            let metadata = json!({"fingerprint": format!("sha256:{}", "9".repeat(64)), "dimension":dimension});
            Ok(batch(args.py(), bridge_ref.bind(args.py()), vectors, metadata).unbind())
        }).unwrap();
        let _wrong = AttrPatch::replace(bridge.as_any(), "embed_batch", wrong_fingerprint.as_any());
        bridge_error(
            py,
            provider.call_method1("embed", (vec!["a"],)),
            "changed vector spaces",
        );
        let expected = signature_with_kwargs(py, "texts");
        let bridge_ref = bridge.clone().unbind();
        let wrong_dimension =
            PyCFunction::new_closure(py, None, None, move |args, kwargs| -> PyResult<Py<PyAny>> {
                let bound = bind_signature(&expected, args, kwargs)?;
                let texts: Vec<String> =
                    bound.getattr("arguments")?.get_item("texts")?.extract()?;
                let vectors = json!(vec![vec![0.0; dimension + 1]; texts.len()]);
                let metadata = json!({"fingerprint":fingerprint, "dimension":dimension+1});
                Ok(batch(args.py(), bridge_ref.bind(args.py()), vectors, metadata).unbind())
            })
            .unwrap();
        let _dimension =
            AttrPatch::replace(bridge.as_any(), "embed_batch", wrong_dimension.as_any());
        bridge_error(
            py,
            provider.call_method1("embed", (vec!["a"],)),
            "dimension changed",
        );
    });
}

#[test]
fn provider_name_binds_backend_and_query_contract() {
    let _case = Case::new();
    Python::attach(|py| {
        let bridge = bridge(py);
        let _service = no_service(py, &bridge);
        let provider = provider(py, &bridge);
        assert!(provider.getattr("dimension").unwrap().eq(1024).unwrap());
        let name: String = provider.getattr("name").unwrap().extract().unwrap();
        assert!(name.starts_with("workspace:"));
        assert!(!name.contains("qwen"));
    });
}

#[test]
fn provider_pins_broker_fingerprint() {
    let _case = Case::new();
    let calls = Arc::new(Mutex::new(Vec::<Value>::new()));
    Python::attach(|py| {
        let bridge = bridge(py);
        let _service = no_service(py, &bridge);
        let provider = provider(py, &bridge);
        let dimension: usize = provider.getattr("dimension").unwrap().extract().unwrap();
        let fingerprint: String = provider
            .getattr("_route")
            .unwrap()
            .getattr("fingerprint")
            .unwrap()
            .extract()
            .unwrap();
        let expected = signature_with_kwargs(py, "texts");
        let recorded = calls.clone();
        let bridge_ref = bridge.clone().unbind();
        let expected_fingerprint = fingerprint.clone();
        let fake = PyCFunction::new_closure(py, None, None, move |args, kwargs| -> PyResult<Py<PyAny>> {
            let bound = bind_signature(&expected, args, kwargs)?;
            let arguments = bound.getattr("arguments")?;
            let texts: Vec<String> = arguments.get_item("texts")?.extract()?;
            let keyword = arguments.get_item("kwargs")?;
            recorded.lock().unwrap().push(json!({
                "texts": texts,
                "required_fingerprint": keyword.get_item("required_fingerprint")?.extract::<String>()?,
                "purpose": keyword.get_item("purpose")?.extract::<String>()?,
            }));
            let mut vector = vec![0.0; dimension];
            vector[0] = 1.0;
            let vectors = json!(vec![vector; texts.len()]);
            let metadata = json!({"fingerprint":expected_fingerprint,"dimension":dimension,"selection_reason":"pinned-index"});
            Ok(batch(args.py(), bridge_ref.bind(args.py()), vectors, metadata).unbind())
        }).unwrap();
        let _patch = AttrPatch::replace(bridge.as_any(), "embed_batch", fake.as_any());
        assert_eq!(
            provider
                .call_method1("embed", (vec!["a", "b"],))
                .unwrap()
                .len()
                .unwrap(),
            2
        );
        assert_eq!(
            provider
                .call_method1("embed_query", ("find stale authorization",))
                .unwrap()
                .len()
                .unwrap(),
            1024
        );
        let observed = calls.lock().unwrap();
        assert_eq!(observed[0]["required_fingerprint"], fingerprint);
        assert_eq!(observed[0]["purpose"], "document");
        let instruct: String = bridge.getattr("QUERY_INSTRUCT").unwrap().extract().unwrap();
        assert!(observed[1]["texts"][0]
            .as_str()
            .unwrap()
            .starts_with(&instruct));
        assert_eq!(observed[1]["purpose"], "query");
    });
}

#[test]
fn trace_contains_no_raw_text() {
    let mut case = Case::new();
    let trace = case.root().join("trace.json");
    case.set_env("WORKSPACE_CRG_EMBED_TRACE_PATH", trace.to_str().unwrap());
    Python::attach(|py| {
        let bridge = bridge(py);
        assert!(bridge
            .getattr("TRACE_PATH_ENV")
            .unwrap()
            .eq("WORKSPACE_CRG_EMBED_TRACE_PATH")
            .unwrap());
        let _service = no_service(py, &bridge);
        let provider = provider(py, &bridge);
        // Keep this sentinel fragmented so this source remains a meaningful secret-scan fixture.
        let secret = ["RAW_", "SECRET_", "MUST_NOT_APPEAR"].concat();
        let dimension: usize = provider.getattr("dimension").unwrap().extract().unwrap();
        let fingerprint: String = provider
            .getattr("_route")
            .unwrap()
            .getattr("fingerprint")
            .unwrap()
            .extract()
            .unwrap();
        let expected = signature_with_kwargs(py, "texts");
        let bridge_ref = bridge.clone().unbind();
        let fake = PyCFunction::new_closure(py, None, None, move |args, kwargs| -> PyResult<Py<PyAny>> {
            let bound = bind_signature(&expected, args, kwargs)?;
            let texts: Vec<String> = bound.getattr("arguments")?.get_item("texts")?.extract()?;
            let mut vector = vec![0.0; dimension];
            vector[0] = 1.0;
            let vectors = json!(vec![vector; texts.len()]);
            let metadata = json!({"fingerprint":fingerprint,"dimension":dimension,"selection_reason":"pinned-index"});
            Ok(batch(args.py(), bridge_ref.bind(args.py()), vectors, metadata).unbind())
        }).unwrap();
        let _patch = AttrPatch::replace(bridge.as_any(), "embed_batch", fake.as_any());
        provider.call_method1("embed_query", (&secret,)).unwrap();
        let content = fs::read_to_string(&trace).unwrap();
        let payload: Value = serde_json::from_str(&content).unwrap();
        assert_eq!(payload["purpose"], "query");
        assert_eq!(payload["vector_count"], 1);
        assert!(!content.contains(&secret));
    });
}
