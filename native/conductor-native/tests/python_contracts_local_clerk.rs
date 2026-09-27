#![cfg(feature = "python-compat-tests")]
//! Rust-owned local-clerk security and provenance contracts; no model is contacted.

#[path = "python_contracts/agent_comm_support.rs"]
#[allow(dead_code)]
mod comm_support;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use comm_support::{bind_signature, json_value, py_json, signature};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyCFunction, PyDict, PyList, PyModule, PyTuple};
use serde_json::{json, Value};
use std::fs;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use support::{assert_error, module, path, AttrPatch, Case};

fn patched_roots(py: Python<'_>, clerk: &Bound<'_, PyModule>, case: &Case) -> Vec<AttrPatch> {
    let root = path(py, case.root());
    let allowed = PyTuple::new(py, [path(py, case.root())]).unwrap();
    let output = path(py, &case.root().join("outputs"));
    vec![
        AttrPatch::replace(clerk.as_any(), "ROOT", &root),
        AttrPatch::replace(clerk.as_any(), "ALLOWED_ROOTS", allowed.as_any()),
        AttrPatch::replace(clerk.as_any(), "OUTPUT_ROOT", &output),
    ]
}

fn clerk_error(
    py: Python<'_>,
    clerk: &Bound<'_, PyModule>,
    result: PyResult<Bound<'_, PyAny>>,
    message: &str,
) {
    assert_error(
        py,
        result.unwrap_err(),
        &clerk.getattr("ClerkError").unwrap(),
        message,
    );
}

#[test]
fn ollama_endpoint_accepts_only_plain_loopback_origins() {
    let mut case = Case::new();
    Python::attach(|py| {
        let clerk = module(py, "conductor.local_clerk");
        for (raw, expected) in [
            (
                "http://127.0.0.1:11434",
                ("127.0.0.1", 11434, "/api/generate"),
            ),
            ("http://localhost", ("localhost", 80, "/api/generate")),
            ("http://[::1]:11434", ("::1", 11434, "/api/generate")),
        ] {
            case.set_env("OLLAMA_HOST", raw);
            let actual: (String, i32, String) = clerk
                .getattr("_ollama_endpoint")
                .unwrap()
                .call0()
                .unwrap()
                .extract()
                .unwrap();
            assert_eq!(
                actual,
                (expected.0.to_owned(), expected.1, expected.2.to_owned()),
                "{raw}"
            );
        }
    });
}

#[test]
fn ollama_endpoint_rejects_non_loopback_or_ambient_request_data() {
    let mut case = Case::new();
    Python::attach(|py| {
        let clerk = module(py, "conductor.local_clerk");
        for raw in [
            "https://127.0.0.1:11434",
            "http://example.com:11434",
            "http://user:placeholder@localhost:11434",
            "http://localhost:11434/api/generate",
            "http://localhost:11434?model=other",
        ] {
            case.set_env("OLLAMA_HOST", raw);
            clerk_error(
                py,
                &clerk,
                clerk.getattr("_ollama_endpoint").unwrap().call0(),
                "plain loopback HTTP origin",
            );
        }
    });
}

#[test]
fn output_paths_and_atomic_writer_never_clobber_existing_files() {
    let case = Case::new();
    Python::attach(|py| {
        let clerk = module(py, "conductor.local_clerk");
        let _roots = patched_roots(py, &clerk, &case);
        let source = case.write("outputs/source.json", "source");
        let source_rows = py_json(py, json!([{"path":source}]));
        let kw = PyDict::new(py);
        kw.set_item("sources", &source_rows).unwrap();
        clerk_error(
            py,
            &clerk,
            clerk
                .getattr("_safe_output_path")
                .unwrap()
                .call((path(py, &source),), Some(&kw)),
            "collides with a source",
        );
        let existing = case.write("outputs/existing.json", "preserve me");
        kw.set_item("sources", py_json(py, json!([]))).unwrap();
        clerk_error(
            py,
            &clerk,
            clerk
                .getattr("_safe_output_path")
                .unwrap()
                .call((path(py, &existing),), Some(&kw)),
            "already exists",
        );
        clerk_error(
            py,
            &clerk,
            clerk.getattr("_write_json").unwrap().call1((
                path(py, &existing),
                py_json(py, json!({"replacement":true})),
            )),
            "already exists",
        );
        assert_eq!(fs::read_to_string(existing).unwrap(), "preserve me");
    });
}

fn stat_namespace<'py>(
    py: Python<'py>,
    stat: &Bound<'py, PyAny>,
    mtime_delta: i64,
) -> Bound<'py, PyAny> {
    let kw = PyDict::new(py);
    for field in ["st_dev", "st_ino", "st_size"] {
        kw.set_item(field, stat.getattr(field).unwrap()).unwrap();
    }
    let mtime: i64 = stat.getattr("st_mtime_ns").unwrap().extract().unwrap();
    kw.set_item("st_mtime_ns", mtime + mtime_delta).unwrap();
    py.import("types")
        .unwrap()
        .getattr("SimpleNamespace")
        .unwrap()
        .call((), Some(&kw))
        .unwrap()
}

#[test]
fn read_source_rejects_a_file_that_changes_during_read() {
    let case = Case::new();
    let source = case.write("note.md", "# Stable title\nbody\n");
    Python::attach(|py| {
        let clerk = module(py, "conductor.local_clerk");
        let _roots = patched_roots(py, &clerk, &case);
        let stat = path(py, &source).call_method0("stat").unwrap();
        let stats = [
            stat_namespace(py, &stat, 0).unbind(),
            stat_namespace(py, &stat, 1).unbind(),
        ];
        let calls = Arc::new(AtomicUsize::new(0));
        let shape = signature(py, &["_fd"], &[]);
        let fake = PyCFunction::new_closure(py, None, None, {
            let calls = Arc::clone(&calls);
            move |args: &Bound<'_, PyTuple>,
                  kw: Option<&Bound<'_, PyDict>>|
                  -> PyResult<Py<PyAny>> {
                bind_signature(&shape, args, kw)?;
                let index = calls.fetch_add(1, Ordering::SeqCst);
                Ok(stats
                    .get(index)
                    .expect("two fstat calls")
                    .clone_ref(args.py()))
            }
        })
        .unwrap();
        let os = clerk.getattr("os").unwrap();
        let _patch = AttrPatch::replace(&os, "fstat", fake.as_any());
        clerk_error(
            py,
            &clerk,
            clerk
                .getattr("_read_source")
                .unwrap()
                .call1((path(py, &source),)),
            "changed while being read",
        );
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    });
}

#[test]
fn multi_source_prompt_is_bounded_complete_and_valid_json() {
    let case = Case::new();
    Python::attach(|py| {
        let clerk = module(py, "conductor.local_clerk");
        let count: usize = clerk
            .getattr("MAX_SOURCE_COUNT")
            .unwrap()
            .extract()
            .unwrap();
        let chars: usize = clerk
            .getattr("MAX_SOURCE_TEXT_CHARS")
            .unwrap()
            .extract()
            .unwrap();
        let source = clerk.getattr("Source").unwrap();
        let rows = (0..count)
            .map(|number| {
                let digit = number.to_string();
                source
                    .call1((
                        path(py, &case.root().join(format!("source-{number}.md"))),
                        digit.repeat(64),
                        chars,
                        format!("Source {number}"),
                        digit.repeat(chars),
                        false,
                    ))
                    .unwrap()
                    .unbind()
            })
            .collect::<Vec<_>>();
        let inputs = PyList::new(py, rows.iter().map(|row| row.bind(py))).unwrap();
        let result = clerk.getattr("_prompt").unwrap().call1((inputs,)).unwrap();
        let (prompt, prompt_chars): (String, Bound<'_, PyAny>) = result.extract().unwrap();
        let envelope: Value = serde_json::from_str(&prompt[prompt.find('{').unwrap()..]).unwrap();
        let max_prompt: usize = clerk
            .getattr("MAX_PROMPT_CHARS")
            .unwrap()
            .extract()
            .unwrap();
        let min_chars: usize = clerk
            .getattr("MIN_PROMPT_SOURCE_CHARS")
            .unwrap()
            .extract()
            .unwrap();
        assert!(prompt.chars().count() <= max_prompt);
        assert_eq!(envelope["sources"].as_array().unwrap().len(), count);
        let paths = envelope["sources"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["path"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        let expected = (0..count)
            .map(|number| {
                case.root()
                    .join(format!("source-{number}.md"))
                    .display()
                    .to_string()
            })
            .collect::<Vec<_>>();
        assert_eq!(paths, expected);
        let text_lengths = envelope["sources"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["text"].as_str().unwrap().chars().count())
            .collect::<Vec<_>>();
        assert!(prompt_chars
            .eq(PyList::new(py, &text_lengths).unwrap())
            .unwrap());
        assert!(text_lengths.iter().all(|&n| n >= min_chars));
    });
}

fn valid_model_draft() -> Value {
    json!({"summary":"A bounded summary.","source_decisions":[],"open_tasks":["Review the result."],"todo_items":[],"duplicate_candidates":[]})
}

#[test]
fn draft_provenance_round_trips_and_rejects_tampering() {
    let case = Case::new();
    let source = case.write(
        "note.md",
        &format!("# Note\n{}", "bounded facts ".repeat(40)),
    );
    Python::attach(|py| {
        let clerk = module(py, "conductor.local_clerk");
        let _roots = patched_roots(py, &clerk, &case);
        let observed = Arc::new(Mutex::new(Vec::<String>::new()));
        let draft = py_json(py, valid_model_draft()).unbind();
        let shape = signature(py, &["_prompt"], &["model"]);
        let generate = PyCFunction::new_closure(py, None, None, {
            let observed = Arc::clone(&observed);
            move |args: &Bound<'_, PyTuple>,
                  kw: Option<&Bound<'_, PyDict>>|
                  -> PyResult<(Py<PyAny>, i32)> {
                bind_signature(&shape, args, kw)?;
                let model: String = kw.unwrap().get_item("model")?.unwrap().extract()?;
                observed.lock().unwrap().push(model);
                Ok((draft.clone_ref(args.py()), 12))
            }
        })
        .unwrap();
        let _patch = AttrPatch::replace(clerk.as_any(), "_generate", generate.as_any());
        let payload = clerk
            .getattr("draft")
            .unwrap()
            .call1((vec![path(py, &source)],))
            .unwrap();
        let payload = json_value(&payload);
        assert_eq!(observed.lock().unwrap().as_slice(), ["qwen3.5:9b"]);
        let file = case.write("draft.json", &payload.to_string());
        let result = clerk
            .getattr("validate_document")
            .unwrap()
            .call1((path(py, &file),))
            .unwrap();
        assert_eq!(
            json_value(&result),
            json!({"valid":true,"sources":1,"path":file})
        );
        let mut tampered = payload;
        tampered["sources"][0]["sha256"] = json!("0".repeat(64));
        let tampered_file = case.write("tampered.json", &tampered.to_string());
        clerk_error(
            py,
            &clerk,
            clerk
                .getattr("validate_document")
                .unwrap()
                .call1((path(py, &tampered_file),)),
            "source changed since draft",
        );
    });
}

#[test]
fn clerk_roots_derive_from_home_unless_overridden() {
    let mut case = Case::new();
    case.remove_env("CONDUCTOR_CLERK_ROOTS");
    Python::attach(|py| {
        let clerk = module(py, "conductor.local_clerk");
        let root = clerk.getattr("ROOT").unwrap();
        let home = py
            .import("pathlib")
            .unwrap()
            .getattr("Path")
            .unwrap()
            .call_method0("home")
            .unwrap();
        let roots = clerk.getattr("_clerk_roots").unwrap().call0().unwrap();
        let expected = PyTuple::new(
            py,
            [
                root.clone(),
                home.call_method1("joinpath", (".claude", "tasks")).unwrap(),
                home.call_method1("joinpath", (".codex", "memories"))
                    .unwrap(),
                home.call_method1("joinpath", ("Documents", "CodexVault"))
                    .unwrap(),
            ],
        )
        .unwrap();
        assert!(roots.eq(expected).unwrap());
        let first = case.root().join("notes");
        let second = case.root().join("vault");
        let sep: String = py
            .import("os")
            .unwrap()
            .getattr("pathsep")
            .unwrap()
            .extract()
            .unwrap();
        case.set_env(
            "CONDUCTOR_CLERK_ROOTS",
            &format!("{}{sep}{}", first.display(), second.display()),
        );
        let roots = clerk.getattr("_clerk_roots").unwrap().call0().unwrap();
        let expected = PyTuple::new(py, [root, path(py, &first), path(py, &second)]).unwrap();
        assert!(roots.eq(expected).unwrap());
    });
}
