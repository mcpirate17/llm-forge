#![cfg(feature = "python-compat-tests")]
//! Rust-owned contracts for the automatic memory-index mark and flush API.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::exceptions::PyTypeError;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyCFunction, PyDict, PyModule, PySet, PyTuple};
use serde_json::{json, Value};
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use support::{assert_error, module, path, Case};

fn runner_sources<'py>(
    args: &Bound<'py, PyTuple>,
    kw: Option<&Bound<'py, PyDict>>,
    sources_name: &str,
) -> PyResult<Bound<'py, PyAny>> {
    let parameters = ["_repo", "_timeout", sources_name];
    if args.len() > parameters.len() {
        return Err(PyTypeError::new_err("runner takes three arguments"));
    }
    if let Some(kw) = kw {
        for (key, _) in kw.iter() {
            if !parameters.contains(&key.extract::<String>()?.as_str()) {
                return Err(PyTypeError::new_err("unexpected runner keyword"));
            }
        }
    }
    let mut sources = None;
    for (index, name) in parameters.into_iter().enumerate() {
        let keyword = kw.map(|kw| kw.get_item(name)).transpose()?.flatten();
        let value = if index < args.len() {
            if keyword.is_some() {
                return Err(PyTypeError::new_err("duplicate runner argument"));
            }
            args.get_item(index)?
        } else {
            keyword.ok_or_else(|| PyTypeError::new_err(format!("missing {name}")))?
        };
        if index == 2 {
            sources = Some(value);
        }
    }
    Ok(sources.unwrap())
}

fn json_object<'py>(py: Python<'py>, value: Value) -> Bound<'py, PyAny> {
    py.import("json")
        .unwrap()
        .getattr("loads")
        .unwrap()
        .call1((value.to_string(),))
        .unwrap()
}

fn json_value(value: &Bound<'_, PyAny>) -> Value {
    let encoded: String = value
        .py()
        .import("json")
        .unwrap()
        .getattr("dumps")
        .unwrap()
        .call1((value,))
        .unwrap()
        .extract()
        .unwrap();
    serde_json::from_str(&encoded).unwrap()
}

fn catalog(case: &Case, notes: &Path) -> std::path::PathBuf {
    let file = case.root().join("sources.toml");
    let root = serde_json::to_string(&notes.to_str().unwrap()).unwrap();
    fs::write(
        &file,
        format!(
            "schema_version = 1\n[[source]]\nid = \"notes\"\nkind = \"index\"\nabsolute_root = {root}\nglob = \"*.md\"\nchunk = \"heading\""
        ),
    )
    .unwrap();
    file
}

fn payload<'py>(py: Python<'py>, file: &Path) -> Bound<'py, PyAny> {
    json_object(
        py,
        json!({"hook_event_name":"PostToolUse","tool_name":"write_file",
            "cwd":file.parent().unwrap(),"tool_input":{"file_path":file}}),
    )
}

fn mark<'py>(
    py: Python<'py>,
    auto: &Bound<'py, PyModule>,
    case: &Case,
    file: &Path,
    catalog_file: &Path,
) -> Bound<'py, PyAny> {
    let kw = PyDict::new(py);
    kw.set_item("repo_root", path(py, case.root())).unwrap();
    kw.set_item("state_dir", path(py, &case.root().join("state")))
        .unwrap();
    kw.set_item("catalog_path", path(py, catalog_file)).unwrap();
    auto.getattr("mark_pending")
        .unwrap()
        .call((payload(py, file),), Some(&kw))
        .unwrap()
}

fn completed<'py>(py: Python<'py>, updated: bool, status: i32) -> Bound<'py, PyAny> {
    let output = if status == 0 {
        format!(
            "{}\n",
            json!({"index":"/tmp/index.jsonl","chunks":2,
                "sources":["notes"],"updated":updated})
        )
    } else {
        String::new()
    };
    let stderr = if status == 0 { "" } else { "schema mismatch" };
    py.import("subprocess")
        .unwrap()
        .getattr("CompletedProcess")
        .unwrap()
        .call1((vec!["memory-index"], status, output, stderr))
        .unwrap()
}

fn flush<'py>(
    py: Python<'py>,
    auto: &Bound<'py, PyModule>,
    case: &Case,
    runner: &Bound<'py, PyCFunction>,
    scan_if_clean: bool,
) -> PyResult<Bound<'py, PyAny>> {
    let kw = PyDict::new(py);
    kw.set_item("repo_root", path(py, case.root())).unwrap();
    kw.set_item("state_dir", path(py, &case.root().join("state")))
        .unwrap();
    kw.set_item("runner", runner).unwrap();
    kw.set_item("scan_if_clean", scan_if_clean).unwrap();
    auto.getattr("flush_pending").unwrap().call((), Some(&kw))
}

fn pending_count(case: &Case) -> usize {
    fs::read_dir(case.root().join("state/pending"))
        .unwrap()
        .filter(|entry| {
            entry
                .as_ref()
                .unwrap()
                .path()
                .extension()
                .is_some_and(|ext| ext == "json")
        })
        .count()
}

#[test]
fn mark_pending_filters_non_indexed_paths() {
    let case = Case::new();
    let notes = case.mkdir("notes");
    let note = case.write("notes/finding.md", "finding");
    let other = case.write("code.py", "code");
    let catalog_file = catalog(&case, &notes);
    Python::attach(|py| {
        let auto = module(py, "conductor.memory_auto_index");
        assert!(mark(py, &auto, &case, &other, &catalog_file).is_none());
        let marker = mark(py, &auto, &case, &note, &catalog_file);
        assert!(!marker.is_none());
        assert!(marker
            .call_method0("is_file")
            .unwrap()
            .extract::<bool>()
            .unwrap());
        assert_eq!(pending_count(&case), 1);
    });
}

#[test]
fn flush_coalesces_markers_and_writes_receipt() {
    let case = Case::new();
    let notes = case.mkdir("notes");
    let first = case.write("notes/first.md", "first");
    let second = case.write("notes/second.md", "second");
    let catalog_file = catalog(&case, &notes);
    Python::attach(|py| {
        let auto = module(py, "conductor.memory_auto_index");
        for file in [&first, &second] {
            mark(py, &auto, &case, file, &catalog_file);
        }
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = Arc::new(std::sync::Mutex::new(Vec::new()));
        let result = completed(py, true, 0).unbind();
        let runner = PyCFunction::new_closure(py, None, None, {
            let calls = Arc::clone(&calls);
            let observed = Arc::clone(&observed);
            move |args: &Bound<'_, PyTuple>,
                  kw: Option<&Bound<'_, PyDict>>|
                  -> PyResult<Py<PyAny>> {
                let sources = runner_sources(args, kw, "sources")?;
                calls.fetch_add(1, Ordering::SeqCst);
                observed
                    .lock()
                    .unwrap()
                    .push(sources.eq(PySet::new(args.py(), ["notes"])?)?);
                Ok(result.clone_ref(args.py()))
            }
        })
        .unwrap();
        let receipts = flush(py, &auto, &case, &runner, false).unwrap();
        let rows = json_value(&receipts);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(*observed.lock().unwrap(), vec![true]);
        assert_eq!(rows[0]["status"], "PASS");
        assert_eq!(rows[0]["pending_markers"], 2);
        assert_eq!(pending_count(&case), 0);
        let latest: Value =
            serde_json::from_slice(&fs::read(case.root().join("state/latest.json")).unwrap())
                .unwrap();
        assert_eq!(latest["status"], "PASS");
    });
}

#[test]
fn flush_single_writer_across_concurrent_calls() {
    let case = Case::new();
    case.write("state/pending/one.json", "{\"paths\": [\"one.md\"]}");
    Python::attach(|py| {
        let auto = module(py, "conductor.memory_auto_index");
        let calls = Arc::new(AtomicUsize::new(0));
        let result = completed(py, true, 0).unbind();
        let runner = PyCFunction::new_closure(py, None, None, {
            let calls = Arc::clone(&calls);
            move |args: &Bound<'_, PyTuple>,
                  kw: Option<&Bound<'_, PyDict>>|
                  -> PyResult<Py<PyAny>> {
                runner_sources(args, kw, "_sources")?;
                calls.fetch_add(1, Ordering::SeqCst);
                args.py().import("time")?.getattr("sleep")?.call1((0.05,))?;
                Ok(result.clone_ref(args.py()))
            }
        })
        .unwrap();
        let pool = py
            .import("concurrent.futures")
            .unwrap()
            .getattr("ThreadPoolExecutor")
            .unwrap()
            .call1((2,))
            .unwrap();
        let kw = PyDict::new(py);
        kw.set_item("repo_root", path(py, case.root())).unwrap();
        kw.set_item("state_dir", path(py, &case.root().join("state")))
            .unwrap();
        kw.set_item("runner", &runner).unwrap();
        let futures = (0..2)
            .map(|_| {
                pool.call_method(
                    "submit",
                    (auto.getattr("flush_pending").unwrap(),),
                    Some(&kw),
                )
                .unwrap()
            })
            .collect::<Vec<_>>();
        let mut sizes = futures
            .iter()
            .map(|future| future.call_method0("result").unwrap().len().unwrap())
            .collect::<Vec<_>>();
        pool.call_method0("shutdown").unwrap();
        sizes.sort();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(sizes, [0, 1]);
    });
}

#[test]
fn flush_repeats_when_new_marker_arrives_mid_refresh() {
    let case = Case::new();
    let notes = case.mkdir("notes");
    let first = case.write("notes/first.md", "first");
    let second = case.write("notes/second.md", "second");
    let catalog_file = catalog(&case, &notes);
    Python::attach(|py| {
        let auto = module(py, "conductor.memory_auto_index");
        mark(py, &auto, &case, &first, &catalog_file);
        let calls = Arc::new(AtomicUsize::new(0));
        let saw_notes = Arc::new(std::sync::Mutex::new(Vec::new()));
        let result = completed(py, true, 0).unbind();
        let module = auto.clone().unbind();
        let second_payload = payload(py, &second).unbind();
        let kw = PyDict::new(py);
        kw.set_item("repo_root", path(py, case.root())).unwrap();
        kw.set_item("state_dir", path(py, &case.root().join("state")))
            .unwrap();
        kw.set_item("catalog_path", path(py, &catalog_file))
            .unwrap();
        let mark_kwargs = kw.unbind();
        let runner = PyCFunction::new_closure(py, None, None, {
            let calls = Arc::clone(&calls);
            let saw_notes = Arc::clone(&saw_notes);
            move |args: &Bound<'_, PyTuple>,
                  kw: Option<&Bound<'_, PyDict>>|
                  -> PyResult<Py<PyAny>> {
                let sources = runner_sources(args, kw, "sources")?;
                saw_notes
                    .lock()
                    .unwrap()
                    .push(sources.eq(PySet::new(args.py(), ["notes"])?)?);
                if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                    let py = args.py();
                    module
                        .bind(py)
                        .getattr("mark_pending")?
                        .call((second_payload.bind(py),), Some(mark_kwargs.bind(py)))?;
                }
                Ok(result.clone_ref(args.py()))
            }
        })
        .unwrap();
        let rows = json_value(&flush(py, &auto, &case, &runner, false).unwrap());
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(*saw_notes.lock().unwrap(), vec![true, true]);
        assert_eq!(rows.as_array().unwrap().len(), 2);
        assert_eq!(pending_count(&case), 0);
    });
}

#[test]
fn flush_failure_retains_markers_and_fails_closed() {
    let case = Case::new();
    let marker = case.write("state/pending/one.json", "{\"paths\": [\"one.md\"]}");
    Python::attach(|py| {
        let auto = module(py, "conductor.memory_auto_index");
        let result = completed(py, true, 2).unbind();
        let runner = PyCFunction::new_closure(
            py,
            None,
            None,
            move |args: &Bound<'_, PyTuple>,
                  kw: Option<&Bound<'_, PyDict>>|
                  -> PyResult<Py<PyAny>> {
                runner_sources(args, kw, "_sources")?;
                Ok(result.clone_ref(args.py()))
            },
        )
        .unwrap();
        assert_error(
            py,
            flush(py, &auto, &case, &runner, false).unwrap_err(),
            &auto.getattr("AutoIndexError").unwrap(),
            "schema mismatch",
        );
        assert!(marker.is_file());
        let latest: Value =
            serde_json::from_slice(&fs::read(case.root().join("state/latest.json")).unwrap())
                .unwrap();
        assert_eq!(latest["status"], "FAIL-CLOSED");
        assert_eq!(latest["is_valid"], false);
    });
}

#[test]
fn session_end_scan_runs_without_pending_markers() {
    let case = Case::new();
    Python::attach(|py| {
        let auto = module(py, "conductor.memory_auto_index");
        let calls = Arc::new(AtomicUsize::new(0));
        let saw_none = Arc::new(std::sync::Mutex::new(Vec::new()));
        let result = completed(py, false, 0).unbind();
        let runner = PyCFunction::new_closure(py, None, None, {
            let calls = Arc::clone(&calls);
            let saw_none = Arc::clone(&saw_none);
            move |args: &Bound<'_, PyTuple>,
                  kw: Option<&Bound<'_, PyDict>>|
                  -> PyResult<Py<PyAny>> {
                let sources = runner_sources(args, kw, "sources")?;
                calls.fetch_add(1, Ordering::SeqCst);
                saw_none.lock().unwrap().push(sources.is_none());
                Ok(result.clone_ref(args.py()))
            }
        })
        .unwrap();
        let rows = json_value(&flush(py, &auto, &case, &runner, true).unwrap());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(*saw_none.lock().unwrap(), vec![true]);
        assert_eq!(rows[0]["status"], "PASS");
        assert_eq!(rows[0]["pending_markers"], 0);
    });
}
