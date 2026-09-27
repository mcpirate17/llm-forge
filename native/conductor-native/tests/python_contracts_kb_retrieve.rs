#![cfg(feature = "python-compat-tests")]
//! Rust-owned fixtures and assertions for the KB retrieval Python boundary.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::exceptions::{PyTimeoutError, PyTypeError};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyCFunction, PyDict, PyModule, PyTuple};
use serde_json::{json, Value};
use std::fs;
use std::sync::{Arc, Mutex};
use support::{assert_error, attr_text, module, path, text, AttrPatch, Case};

fn py_json<'py>(py: Python<'py>, value: &Value) -> Bound<'py, PyAny> {
    PyModule::import(py, "json")
        .unwrap()
        .getattr("loads")
        .unwrap()
        .call1((value.to_string(),))
        .unwrap()
}

fn json_value(value: &Bound<'_, PyAny>) -> Value {
    let py = value.py();
    let encoded: String = PyModule::import(py, "json")
        .unwrap()
        .getattr("dumps")
        .unwrap()
        .call1((value,))
        .unwrap()
        .extract()
        .unwrap();
    serde_json::from_str(&encoded).unwrap()
}

fn kwargs<'py>(py: Python<'py>, pairs: &[(&str, Bound<'py, PyAny>)]) -> Bound<'py, PyDict> {
    let out = PyDict::new(py);
    for (key, value) in pairs {
        out.set_item(key, value).unwrap();
    }
    out
}

fn index_payload(gpu: i64, fingerprint: &str) -> Value {
    json!({"schema_version":2,"embedding":{"fingerprint":fingerprint,"dimension":1,
        "paid":false,"num_gpu":gpu,"num_ctx":2048},
        "cards":[{"name":"x","path":"x","text":"x","vector":[1.0]}]})
}

fn fingerprint() -> String {
    format!("sha256:{}", "a".repeat(64))
}

fn error(py: Python<'_>, kb: &Bound<'_, PyModule>, result: PyResult<Bound<'_, PyAny>>, part: &str) {
    assert_error(
        py,
        result.expect_err("expected retrieval error"),
        &kb.getattr("RetrieveError").unwrap(),
        part,
    );
}

fn fixed_embed<'py>(
    py: Python<'py>,
    vector: Vec<f64>,
    calls: Option<Arc<Mutex<Vec<String>>>>,
) -> Bound<'py, PyCFunction> {
    PyCFunction::new_closure(
        py,
        None,
        None,
        move |args: &Bound<'_, PyTuple>, _kw: Option<&Bound<'_, PyDict>>| -> PyResult<Vec<f64>> {
            if let Some(calls) = &calls {
                calls
                    .lock()
                    .unwrap()
                    .push(args.get_item(0)?.extract::<String>()?);
            }
            Ok(vector.clone())
        },
    )
    .unwrap()
}

fn capture<'py>(py: Python<'py>, stream: &str) -> (Bound<'py, PyAny>, AttrPatch) {
    let buffer = PyModule::import(py, "io")
        .unwrap()
        .getattr("StringIO")
        .unwrap()
        .call0()
        .unwrap();
    let patch = AttrPatch::replace(
        PyModule::import(py, "sys").unwrap().as_any(),
        stream,
        &buffer,
    );
    (buffer, patch)
}

#[test]
fn query_ranks_instruct_query_against_document_vectors() {
    let _case = Case::new();
    Python::attach(|py| {
        let kb = module(py, "conductor.kb_retrieve");
        let calls = Arc::new(Mutex::new(Vec::new()));
        let embed = fixed_embed(py, vec![1.0, 0.0, 0.0], Some(Arc::clone(&calls)));
        let index = py_json(
            py,
            &json!({"schema_version":1,"model":"qwen3-embed-cpu","num_gpu":0,
            "cards":[{"name":"kb_hw.md","path":"hw","text":"throughput rules","vector":[0.0,1.0,0.0]},
                     {"name":"kb_comp.md","path":"comp","text":"EAGER_REQUIRED stays","vector":[1.0,0.0,0.0]}]}),
        );
        let kw = kwargs(
            py,
            &[
                ("top_k", 1i32.into_pyobject(py).unwrap().into_any()),
                ("embedder", embed.into_any()),
            ],
        );
        let hits = kb
            .getattr("query_index")
            .unwrap()
            .call(("What is EAGER_REQUIRED?", index), Some(&kw))
            .unwrap();
        assert_eq!(attr_text(&hits.get_item(0).unwrap(), "name"), "kb_comp.md");
        assert!(calls.lock().unwrap()[0].starts_with(&text(&kb.getattr("QUERY_INSTRUCT").unwrap())));
    });
}

#[test]
fn index_load_validation_and_guest_gpu() {
    let case = Case::new();
    Python::attach(|py| {
        let kb = module(py, "conductor.kb_retrieve");
        let file = case.root().join("idx.json");
        for (gpu, valid, message) in [(8, false, "num_gpu"), (99, true, "")] {
            fs::write(&file, index_payload(gpu, &fingerprint()).to_string()).unwrap();
            let loaded = kb.getattr("load_index").unwrap().call1((path(py, &file),));
            if valid {
                assert_eq!(json_value(&loaded.unwrap())["embedding"]["num_gpu"], 99);
            } else {
                error(py, &kb, loaded, message);
            }
        }
        fs::write(&file, index_payload(0, "wrong").to_string()).unwrap();
        error(
            py,
            &kb,
            kb.getattr("load_index").unwrap().call1((path(py, &file),)),
            "fingerprint",
        );
        let mut payload = index_payload(0, &fingerprint());
        payload["schema_version"] = json!(999);
        fs::write(&file, payload.to_string()).unwrap();
        error(
            py,
            &kb,
            kb.getattr("load_index").unwrap().call1((path(py, &file),)),
            "unsupported index schema",
        );
        payload["schema_version"] = json!(2);
        payload["cards"][0]["vector"] = json!([1.0, 2.0]);
        fs::write(&file, payload.to_string()).unwrap();
        error(
            py,
            &kb,
            kb.getattr("load_index").unwrap().call1((path(py, &file),)),
            "dimension does not match",
        );
    });
}

#[test]
fn embedding_metadata_validation_and_zero_vector() {
    let _case = Case::new();
    Python::attach(|py| {
        let kb = module(py, "conductor.kb_retrieve");
        error(
            py,
            &kb,
            kb.getattr("_l2_normalize")
                .unwrap()
                .call1((vec![0.0, 0.0, 0.0],)),
            "zero vector",
        );
        for (meta, message) in [
            (
                json!({"fingerprint":"not-sha256"}),
                "fingerprint is invalid",
            ),
            (
                json!({"fingerprint":fingerprint(),"dimension":0}),
                "dimension is invalid",
            ),
            (
                json!({"fingerprint":fingerprint(),"dimension":1}),
                "paid flag is invalid",
            ),
            (
                json!({"fingerprint":fingerprint(),"dimension":1,"paid":false,"num_gpu":0,"num_ctx":4096}),
                "num_ctx",
            ),
        ] {
            error(
                py,
                &kb,
                kb.getattr("assert_embedding_meta")
                    .unwrap()
                    .call1((py_json(py, &json!({"embedding":meta})),)),
                message,
            );
        }
    });
}

#[test]
fn cards_globs_order_and_missing_directory() {
    let case = Case::new();
    Python::attach(|py| {
        let kb = module(py, "conductor.kb_retrieve");
        case.write("readme.md", "ignored");
        error(
            py,
            &kb,
            kb.getattr("load_cards")
                .unwrap()
                .call1((path(py, case.root()),)),
            "no card files",
        );
        case.write("kb_b.md", "second");
        case.write("kb_a.md", "first");
        let cards = json_value(
            &kb.getattr("load_cards")
                .unwrap()
                .call1((path(py, case.root()),))
                .unwrap(),
        );
        assert_eq!(
            cards
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v["name"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec!["kb_a.md", "kb_b.md"]
        );
        assert_eq!(cards[0]["text"], "first");
        error(
            py,
            &kb,
            kb.getattr("load_cards")
                .unwrap()
                .call1((path(py, &case.root().join("does-not-exist")),)),
            "notes dir missing",
        );
    });
}

#[test]
fn build_save_load_and_atomic_cleanup() {
    let case = Case::new();
    Python::attach(|py| {
        let kb = module(py, "conductor.kb_retrieve");
        let cards = py_json(
            py,
            &json!([{"name":"kb_a.md","path":"a","text":"alpha"},{"name":"kb_b.md","path":"b","text":"beta"}]),
        );
        let embed = PyCFunction::new_closure(
            py,
            None,
            None,
            |args: &Bound<'_, PyTuple>, _kw: Option<&Bound<'_, PyDict>>| -> PyResult<Vec<f64>> {
                Ok(vec![
                    args.get_item(0)?.extract::<String>()?.len() as f64,
                    0.0,
                ])
            },
        )
        .unwrap();
        let kw = kwargs(py, &[("embedder", embed.into_any())]);
        let index = kb
            .getattr("build_index")
            .unwrap()
            .call((cards,), Some(&kw))
            .unwrap();
        let value = json_value(&index);
        assert_eq!(value["schema_version"], 2);
        assert_eq!(value["cards"][0]["vector"], json!([5.0, 0.0]));
        assert_eq!(value["cards"][1]["vector"], json!([4.0, 0.0]));
        assert_eq!(value["embedding"]["dimension"], 2);
        assert_eq!(value["embedding"]["paid"], false);
        let dest = case.root().join("nested/idx.json");
        assert_eq!(
            text(
                &kb.getattr("save_index")
                    .unwrap()
                    .call1((&index, path(py, &dest)))
                    .unwrap()
            ),
            dest.display().to_string()
        );
        assert_eq!(
            json_value(
                &kb.getattr("load_index")
                    .unwrap()
                    .call1((path(py, &dest),))
                    .unwrap()
            ),
            value
        );
        assert_eq!(fs::read_dir(dest.parent().unwrap()).unwrap().count(), 1);
        let bad = PyDict::new(py);
        let set = PyModule::import(py, "builtins")
            .unwrap()
            .getattr("set")
            .unwrap()
            .call1((vec![1, 2, 3],))
            .unwrap();
        bad.set_item("cards", set).unwrap();
        let err = kb
            .getattr("save_index")
            .unwrap()
            .call1((bad, path(py, &dest)))
            .expect_err("set cannot serialize");
        assert!(err.matches(py, &py.get_type::<PyTypeError>()).unwrap());
        assert_eq!(fs::read_dir(dest.parent().unwrap()).unwrap().count(), 1);
    });
}

#[test]
fn injected_query_ranking_and_both_invalid_top_k_values() {
    let _case = Case::new();
    Python::attach(|py| {
        let kb = module(py, "conductor.kb_retrieve");
        let index = py_json(
            py,
            &json!({"embedding":{"fingerprint":fingerprint()},"cards":[
            {"name":"kb_low.md","path":"low","text":"low","vector":[0.0,1.0]},
            {"name":"kb_high.md","path":"high","text":"high","vector":[1.0,0.0]}]}),
        );
        let embed = fixed_embed(py, vec![1.0, 0.0], None);
        for top_k in [0i32, -1] {
            let kw = kwargs(
                py,
                &[
                    ("top_k", top_k.into_pyobject(py).unwrap().into_any()),
                    ("embedder", embed.clone().into_any()),
                ],
            );
            error(
                py,
                &kb,
                kb.getattr("query_index")
                    .unwrap()
                    .call(("q", &index), Some(&kw)),
                "top_k must be",
            );
        }
        let kw = kwargs(
            py,
            &[
                ("top_k", 1i32.into_pyobject(py).unwrap().into_any()),
                ("embedder", embed.into_any()),
            ],
        );
        let hits = kb
            .getattr("query_index")
            .unwrap()
            .call(("q", index), Some(&kw))
            .unwrap();
        assert_eq!(hits.len().unwrap(), 1);
        assert_eq!(attr_text(&hits.get_item(0).unwrap(), "name"), "kb_high.md");
    });
}

#[test]
fn native_score_dimension_error_and_cancellation_parity() {
    let _case = Case::new();
    Python::attach(|py| {
        let kb = module(py, "conductor.kb_retrieve");
        let cards = json!([{"name":"c3","path":"p","text":"t","vector":[1.0,2.0]}]);
        let index = py_json(py, &json!({"embedding":{},"cards":cards}));
        let kw = kwargs(
            py,
            &[
                ("top_k", 1i32.into_pyobject(py).unwrap().into_any()),
                (
                    "embedder",
                    fixed_embed(py, vec![1.0, 2.0, 3.0], None).into_any(),
                ),
            ],
        );
        error(
            py,
            &kb,
            kb.getattr("query_index")
                .unwrap()
                .call(("find", index), Some(&kw)),
            "vector dim mismatch for c3: 2 != 3",
        );
        // Compensated summation is exercised with cancellation, not just easy vectors.
        for vector in [vec![1e16, 1.0, -1e16, 2.0], vec![1e-12, 1e12, -1e12, 3.0]] {
            let native = kb
                .getattr("_l2_normalize")
                .unwrap()
                .call1((vector.clone(),))
                .unwrap();
            let expected = PyModule::import(py, "builtins")
                .unwrap()
                .getattr("sum")
                .unwrap();
            let squares = vector.iter().map(|v| v * v).collect::<Vec<_>>();
            let norm: f64 = expected
                .call1((squares,))
                .unwrap()
                .extract::<f64>()
                .unwrap()
                .sqrt();
            assert_eq!(
                native.extract::<Vec<f64>>().unwrap(),
                vector.iter().map(|v| v / norm).collect::<Vec<_>>()
            );
        }
    });
}

#[test]
fn native_scoring_matches_python_builtin_sum_bit_for_bit() {
    let _case = Case::new();
    Python::attach(|py| {
        let kb = module(py, "conductor.kb_retrieve");
        let rng = PyModule::import(py, "random")
            .unwrap()
            .getattr("Random")
            .unwrap()
            .call1((20260905,))
            .unwrap();
        let sum = PyModule::import(py, "builtins")
            .unwrap()
            .getattr("sum")
            .unwrap();
        for _ in 0..32 {
            let dim: usize = rng
                .call_method1("choice", (vec![8, 64, 1024],))
                .unwrap()
                .extract()
                .unwrap();
            let mut query = Vec::with_capacity(dim);
            let mut vector = Vec::with_capacity(dim);
            for _ in 0..dim {
                query.push(
                    rng.call_method1("uniform", (-1.0, 1.0))
                        .unwrap()
                        .extract::<f64>()
                        .unwrap(),
                );
            }
            for _ in 0..dim {
                vector.push(
                    rng.call_method1("uniform", (-1000.0, 1000.0))
                        .unwrap()
                        .extract::<f64>()
                        .unwrap(),
                );
            }
            let products = query
                .iter()
                .zip(&vector)
                .map(|(a, b)| a * b)
                .collect::<Vec<_>>();
            let expected: f64 = sum.call1((products,)).unwrap().extract().unwrap();
            let index = py_json(
                py,
                &json!({"embedding":{},"cards":[{"name":"c0","path":"n/c0.md","text":"t0","vector":vector}]}),
            );
            let kw = kwargs(
                py,
                &[
                    ("top_k", 1i32.into_pyobject(py).unwrap().into_any()),
                    ("embedder", fixed_embed(py, query, None).into_any()),
                ],
            );
            let hits = kb
                .getattr("query_index")
                .unwrap()
                .call(("find", index), Some(&kw))
                .unwrap();
            let hit = hits.get_item(0).unwrap();
            let score: f64 = hit.getattr("score").unwrap().extract().unwrap();
            assert_eq!(score.to_bits(), expected.to_bits());
            assert_eq!(
                (
                    attr_text(&hit, "name"),
                    attr_text(&hit, "path"),
                    attr_text(&hit, "text")
                ),
                ("c0".to_owned(), "n/c0.md".to_owned(), "t0".to_owned())
            );
        }
    });
}

#[test]
fn native_normalize_matches_python_builtin_sum_for_fixed_seed_samples() {
    let _case = Case::new();
    Python::attach(|py| {
        let kb = module(py, "conductor.kb_retrieve");
        let rng = PyModule::import(py, "random")
            .unwrap()
            .getattr("Random")
            .unwrap()
            .call1((20260907,))
            .unwrap();
        let sum = PyModule::import(py, "builtins")
            .unwrap()
            .getattr("sum")
            .unwrap();
        for _ in 0..32 {
            let dim: usize = rng
                .call_method1("choice", (vec![4, 64, 512],))
                .unwrap()
                .extract()
                .unwrap();
            let vector = (0..dim)
                .map(|_| {
                    rng.call_method1("uniform", (-1e6, 1e6))
                        .unwrap()
                        .extract::<f64>()
                        .unwrap()
                })
                .collect::<Vec<_>>();
            let norm = sum
                .call1((vector.iter().map(|x| x * x).collect::<Vec<_>>(),))
                .unwrap()
                .extract::<f64>()
                .unwrap()
                .sqrt();
            let expected = vector.iter().map(|x| x / norm).collect::<Vec<_>>();
            let actual: Vec<f64> = kb
                .getattr("_l2_normalize")
                .unwrap()
                .call1((vector,))
                .unwrap()
                .extract()
                .unwrap();
            assert_eq!(
                actual.iter().map(|x| x.to_bits()).collect::<Vec<_>>(),
                expected.iter().map(|x| x.to_bits()).collect::<Vec<_>>()
            );
        }
    });
}

#[test]
fn configured_notes_root_and_environment_precedence() {
    let mut case = Case::new();
    Python::attach(|py| {
        let kb = module(py, "conductor.kb_retrieve");
        let _cwd = case.chdir(".");
        case.write(
            "pyproject.toml",
            "[tool.conductor]\nnotes_root = \"cards\"\n",
        );
        case.write("cards/kb_one.md", "# one\n");
        assert_eq!(
            text(&kb.getattr("default_notes_dir").unwrap().call0().unwrap()),
            case.root().join("cards").display().to_string()
        );
        assert_eq!(
            json_value(&kb.getattr("load_cards").unwrap().call0().unwrap())[0]["name"],
            "kb_one.md"
        );
        case.write(
            "pyproject.toml",
            "[tool.conductor]\nnotes_root = \"research/notes\"\n",
        );
        assert_eq!(
            text(&kb.getattr("default_notes_dir").unwrap().call0().unwrap()),
            case.root().join("research/notes").display().to_string()
        );
        case.set_env("CONDUCTOR_NOTES_ROOT", "envnotes");
        assert_eq!(
            text(&kb.getattr("default_notes_dir").unwrap().call0().unwrap()),
            case.root().join("envnotes").display().to_string()
        );
    });
}

#[test]
fn all_embed_entrypoints_have_cold_load_budget() {
    let _case = Case::new();
    Python::attach(|py| {
        let kb = module(py, "conductor.kb_retrieve");
        let contract = module(py, "conductor.embedding_contract");
        let timeout: f64 = contract
            .getattr("EMBED_TIMEOUT_SECONDS")
            .unwrap()
            .extract()
            .unwrap();
        assert!(timeout >= 300.0);
        let inspect = PyModule::import(py, "inspect").unwrap();
        for name in ["embed_batch", "embed_texts", "embed_text"] {
            let sig = inspect
                .getattr("signature")
                .unwrap()
                .call1((kb.getattr(name).unwrap(),))
                .unwrap();
            let default: f64 = sig
                .getattr("parameters")
                .unwrap()
                .get_item("timeout_s")
                .unwrap()
                .getattr("default")
                .unwrap()
                .extract()
                .unwrap();
            assert_eq!(default, timeout, "{name}");
        }
    });
}

#[test]
fn broker_timeout_reports_budget_and_malformed_json_stays_distinct() {
    let _case = Case::new();
    Python::attach(|py| {
        let kb = module(py, "conductor.kb_retrieve");
        let urlopen = kb.getattr("urllib").unwrap().getattr("request").unwrap();
        let timeout = PyCFunction::new_closure(
            py,
            None,
            None,
            |_args: &Bound<'_, PyTuple>, _kw: Option<&Bound<'_, PyDict>>| -> PyResult<()> {
                Err(PyTimeoutError::new_err("timed out"))
            },
        )
        .unwrap();
        let _patch = AttrPatch::replace(&urlopen, "urlopen", timeout.as_any());
        let kw = kwargs(
            py,
            &[
                ("purpose", "document".into_pyobject(py).unwrap().into_any()),
                ("timeout_s", 300.0f64.into_pyobject(py).unwrap().into_any()),
            ],
        );
        let err = kb
            .getattr("embed_batch")
            .unwrap()
            .call((vec!["one", "two"],), Some(&kw))
            .expect_err("timeout");
        assert_error(
            py,
            err.clone_ref(py),
            &kb.getattr("RetrieveError").unwrap(),
            "300s",
        );
        assert!(err.to_string().contains("2 input(s)"));
        assert!(err.to_string().contains("conductor.cpu_embed warm"));
    });
}

#[test]
fn embed_payload_pins_context_and_guest_gpu() {
    let _case = Case::new();
    Python::attach(|py| {
        let kb = module(py, "conductor.kb_retrieve");
        let request_json = Arc::new(Mutex::new(Value::Null));
        let observed = Arc::clone(&request_json);
        let response = json!({"data":[{"index":0,"embedding":[3.0,4.0]}],
            "workspace_embedding":{"fingerprint":format!("sha256:{}","b".repeat(64)),
            "dimension":2,"paid":false,"num_gpu":99,"num_ctx":2048}})
        .to_string();
        let fake = PyCFunction::new_closure(
            py,
            None,
            None,
            move |args: &Bound<'_, PyTuple>,
                  _kw: Option<&Bound<'_, PyDict>>|
                  -> PyResult<Py<PyAny>> {
                let request = args.get_item(0)?;
                let body = request.getattr("data")?;
                let parsed = PyModule::import(args.py(), "json")?
                    .getattr("loads")?
                    .call1((body,))?;
                *observed.lock().unwrap() = json_value(&parsed);
                Ok(PyModule::import(args.py(), "io")?
                    .getattr("BytesIO")?
                    .call1((response.as_bytes(),))?
                    .unbind())
            },
        )
        .unwrap();
        let urlopen = kb.getattr("urllib").unwrap().getattr("request").unwrap();
        let _patch = AttrPatch::replace(&urlopen, "urlopen", fake.as_any());
        let vector: Vec<f64> = kb
            .getattr("embed_text")
            .unwrap()
            .call1(("hello",))
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(vector, vec![0.6, 0.8]);
        assert_eq!(request_json.lock().unwrap()["input"], json!(["hello"]));
        assert_eq!(
            request_json.lock().unwrap()["workspace_purpose"],
            "document"
        );
    });
}

#[test]
fn broker_auto_starts_once_after_connection_refused() {
    let _case = Case::new();
    Python::attach(|py| {
        let kb = module(py, "conductor.kb_retrieve");
        let cpu = module(py, "conductor.cpu_embed");
        let attempts = Arc::new(Mutex::new(0usize));
        let observed = Arc::clone(&attempts);
        let response = json!({"data":[{"index":0,"embedding":[1.0,0.0]}],
            "workspace_embedding":{"fingerprint":format!("sha256:{}","c".repeat(64)),
            "dimension":2,"paid":false,"num_gpu":0,"num_ctx":2048}})
        .to_string();
        let fake = PyCFunction::new_closure(
            py,
            None,
            None,
            move |args: &Bound<'_, PyTuple>,
                  _kw: Option<&Bound<'_, PyDict>>|
                  -> PyResult<Py<PyAny>> {
                let mut count = observed.lock().unwrap();
                *count += 1;
                if *count == 1 {
                    return Err(PyErr::from_value(
                        PyModule::import(args.py(), "urllib.error")?
                            .getattr("URLError")?
                            .call1(("connection refused",))?,
                    ));
                }
                Ok(PyModule::import(args.py(), "io")?
                    .getattr("BytesIO")?
                    .call1((response.as_bytes(),))?
                    .unbind())
            },
        )
        .unwrap();
        let starts = Arc::new(Mutex::new(0usize));
        let seen = Arc::clone(&starts);
        let ensure = PyCFunction::new_closure(
            py,
            None,
            None,
            move |_args: &Bound<'_, PyTuple>, _kw: Option<&Bound<'_, PyDict>>| -> PyResult<bool> {
                *seen.lock().unwrap() += 1;
                Ok(true)
            },
        )
        .unwrap();
        let urlopen = kb.getattr("urllib").unwrap().getattr("request").unwrap();
        let _patches = (
            AttrPatch::replace(&urlopen, "urlopen", fake.as_any()),
            AttrPatch::replace(cpu.as_any(), "ensure_service", ensure.as_any()),
        );
        let vector: Vec<f64> = kb
            .getattr("embed_text")
            .unwrap()
            .call1(("hello",))
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(vector, vec![1.0, 0.0]);
        assert_eq!(*starts.lock().unwrap(), 1);
        assert_eq!(*attempts.lock().unwrap(), 2);
    });
}

#[test]
fn malformed_json_is_not_reported_as_a_timeout() {
    let _case = Case::new();
    Python::attach(|py| {
        let kb = module(py, "conductor.kb_retrieve");
        let fake = PyCFunction::new_closure(
            py,
            None,
            None,
            move |args: &Bound<'_, PyTuple>,
                  _kw: Option<&Bound<'_, PyDict>>|
                  -> PyResult<Py<PyAny>> {
                Ok(PyModule::import(args.py(), "io")?
                    .getattr("BytesIO")?
                    .call1((b"{not json".as_slice(),))?
                    .unbind())
            },
        )
        .unwrap();
        let urlopen = kb.getattr("urllib").unwrap().getattr("request").unwrap();
        let _patch = AttrPatch::replace(&urlopen, "urlopen", fake.as_any());
        let kw = kwargs(
            py,
            &[("purpose", "document".into_pyobject(py).unwrap().into_any())],
        );
        let err = kb
            .getattr("embed_batch")
            .unwrap()
            .call((vec!["one"],), Some(&kw))
            .expect_err("bad JSON");
        assert_error(
            py,
            err.clone_ref(py),
            &kb.getattr("RetrieveError").unwrap(),
            "malformed JSON",
        );
        assert!(!err.to_string().contains("cpu_embed warm"));
    });
}

#[test]
fn main_index_builds_saves_and_reports_count() {
    let case = Case::new();
    Python::attach(|py| {
        let kb = module(py, "conductor.kb_retrieve");
        let cards = json!([{"name":"a"},{"name":"b"}]);
        let index = json!({"cards":cards});
        let calls = Arc::new(Mutex::new(Vec::<(&'static str, Value)>::new()));
        let seen = Arc::clone(&calls);
        let cards_fixture = cards.clone();
        let load = PyCFunction::new_closure(
            py,
            None,
            None,
            move |args: &Bound<'_, PyTuple>,
                  _kw: Option<&Bound<'_, PyDict>>|
                  -> PyResult<Py<PyAny>> {
                seen.lock().unwrap().push(("load", Value::Null));
                Ok(py_json(args.py(), &cards_fixture).unbind())
            },
        )
        .unwrap();
        let seen = Arc::clone(&calls);
        let index_fixture = index.clone();
        let build = PyCFunction::new_closure(
            py,
            None,
            None,
            move |args: &Bound<'_, PyTuple>,
                  _kw: Option<&Bound<'_, PyDict>>|
                  -> PyResult<Py<PyAny>> {
                seen.lock()
                    .unwrap()
                    .push(("build", json_value(&args.get_item(0)?)));
                Ok(py_json(args.py(), &index_fixture).unbind())
            },
        )
        .unwrap();
        let seen = Arc::clone(&calls);
        let dest = case.root().join("kb_card_index.json");
        let save = PyCFunction::new_closure(
            py,
            None,
            None,
            move |args: &Bound<'_, PyTuple>,
                  _kw: Option<&Bound<'_, PyDict>>|
                  -> PyResult<Py<PyAny>> {
                let received = json_value(&args.get_item(0)?);
                seen.lock().unwrap().push(("save", received.clone()));
                fs::write(&dest, received.to_string()).unwrap();
                Ok(path(args.py(), &dest).unbind())
            },
        )
        .unwrap();
        let _patches = (
            AttrPatch::replace(kb.as_any(), "load_cards", load.as_any()),
            AttrPatch::replace(kb.as_any(), "build_index", build.as_any()),
            AttrPatch::replace(kb.as_any(), "save_index", save.as_any()),
        );
        let (stdout, _stream_patch) = capture(py, "stdout");
        assert_eq!(
            kb.getattr("main")
                .unwrap()
                .call1((vec!["index"],))
                .unwrap()
                .extract::<i32>()
                .unwrap(),
            0
        );
        assert_eq!(
            calls.lock().unwrap().as_slice(),
            [("load", Value::Null), ("build", cards), ("save", index)]
        );
        assert_eq!(
            json_value(
                &PyModule::import(py, "json")
                    .unwrap()
                    .getattr("loads")
                    .unwrap()
                    .call1((text(&stdout.getattr("getvalue").unwrap().call0().unwrap()),))
                    .unwrap()
            ),
            json!({"index":case.root().join("kb_card_index.json").display().to_string(),"cards":2})
        );
    });
}

#[test]
fn main_query_routes_args_and_prints_rounded_hits() {
    let case = Case::new();
    Python::attach(|py| {
        let kb = module(py, "conductor.kb_retrieve");
        let calls = Arc::new(Mutex::new(Vec::<Value>::new()));
        let seen = Arc::clone(&calls);
        let load = PyCFunction::new_closure(
            py,
            None,
            None,
            move |args: &Bound<'_, PyTuple>,
                  _kw: Option<&Bound<'_, PyDict>>|
                  -> PyResult<Py<PyAny>> {
                seen.lock()
                    .unwrap()
                    .push(json!({"load":text(&args.get_item(0)?)}));
                Ok(py_json(args.py(), &json!({"cards":[]})).unbind())
            },
        )
        .unwrap();
        let seen = Arc::clone(&calls);
        let scored = kb.getattr("ScoredCard").unwrap().unbind();
        let query = PyCFunction::new_closure(py,None,None,move |args: &Bound<'_,PyTuple>,kw: Option<&Bound<'_,PyDict>>| -> PyResult<Py<PyAny>> {
            let kw = kw.unwrap();
            seen.lock().unwrap().push(json!({"query":text(&args.get_item(0)?),"index":json_value(&args.get_item(1)?),"top_k":kw.get_item("top_k")?.unwrap().extract::<i32>()?}));
            let hits = PyModule::import(args.py(),"builtins")?.getattr("list")?.call0()?;
            hits.call_method1("append",(scored.bind(args.py()).call1(("alpha","notes/alpha.md",0.98765,"x"))?,))?;
            hits.call_method1("append",(scored.bind(args.py()).call1(("beta","notes/beta.md",0.5,"y"))?,))?;
            Ok(hits.unbind())
        }).unwrap();
        let _patches = (
            AttrPatch::replace(kb.as_any(), "load_index", load.as_any()),
            AttrPatch::replace(kb.as_any(), "query_index", query.as_any()),
        );
        let (stdout, _stream_patch) = capture(py, "stdout");
        let custom = case.root().join("custom_kb_index.json");
        let args = vec![
            "query".to_owned(),
            "how do claims work".to_owned(),
            "--top-k".to_owned(),
            "2".to_owned(),
            "--index".to_owned(),
            custom.display().to_string(),
        ];
        assert_eq!(
            kb.getattr("main")
                .unwrap()
                .call1((args,))
                .unwrap()
                .extract::<i32>()
                .unwrap(),
            0
        );
        assert_eq!(
            calls.lock().unwrap().as_slice(),
            [
                json!({"load":custom.display().to_string()}),
                json!({"query":"how do claims work","index":{"cards":[]},"top_k":2})
            ]
        );
        let output = text(&stdout.getattr("getvalue").unwrap().call0().unwrap());
        assert_eq!(
            serde_json::from_str::<Value>(&output).unwrap(),
            json!([
            {"name":"alpha","score":0.9877,"path":"notes/alpha.md"},
            {"name":"beta","score":0.5,"path":"notes/beta.md"}])
        );
    });
}

#[test]
fn main_retrieve_error_exits_two_and_prints_json_error() {
    let _case = Case::new();
    Python::attach(|py| {
        let kb = module(py, "conductor.kb_retrieve");
        let retrieve_error = kb.getattr("RetrieveError").unwrap().unbind();
        let load = PyCFunction::new_closure(
            py,
            None,
            None,
            move |args: &Bound<'_, PyTuple>, _kw: Option<&Bound<'_, PyDict>>| -> PyResult<()> {
                Err(PyErr::from_value(
                    retrieve_error
                        .bind(args.py())
                        .call1(("broker unreachable",))?,
                ))
            },
        )
        .unwrap();
        let _patch = AttrPatch::replace(kb.as_any(), "load_index", load.as_any());
        let (stderr, _stream_patch) = capture(py, "stderr");
        assert_eq!(
            kb.getattr("main")
                .unwrap()
                .call1((vec!["query", "anything"],))
                .unwrap()
                .extract::<i32>()
                .unwrap(),
            2
        );
        assert_eq!(
            serde_json::from_str::<Value>(&text(
                &stderr.getattr("getvalue").unwrap().call0().unwrap()
            ))
            .unwrap(),
            json!({"error":"broker unreachable"})
        );
    });
}
