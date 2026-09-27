#![cfg(feature = "python-compat-tests")]
//! Rust-owned contracts for batched native-reuse inventory candidates.

#[path = "python_contracts/agent_comm_support.rs"]
#[allow(dead_code)]
mod comm_support;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use comm_support::py_json;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyBool, PyDict, PyList};
use rusqlite::{params, Connection};
use serde_json::json;
use std::path::Path;
use support::{module, path, Case};

#[pyclass]
struct FakeIndex {
    symbols: Vec<Py<PyAny>>,
}

#[pymethods]
impl FakeIndex {
    #[pyo3(signature = (**_kwargs))]
    fn symbols(&self, py: Python<'_>, _kwargs: Option<&Bound<'_, PyDict>>) -> PyResult<Py<PyList>> {
        Ok(PyList::new(py, &self.symbols)?.unbind())
    }

    fn callers(&self, py: Python<'_>, _qualified_name: &Bound<'_, PyAny>) -> PyResult<Py<PyList>> {
        Ok(PyList::new(py, ["consumer"])?.unbind())
    }
}

fn symbol<'py>(
    py: Python<'py>,
    (name, qualified, file, line_start, line_end, language, params): (
        &str,
        &str,
        &str,
        i32,
        i32,
        &str,
        &str,
    ),
) -> Bound<'py, PyAny> {
    module(py, "conductor.reuse.graph_index")
        .getattr("Symbol")
        .unwrap()
        .call1((
            "Function", name, qualified, file, line_start, line_end, language, params, false,
        ))
        .unwrap()
}

fn candidate_rows(py: Python<'_>, symbols: Vec<Py<PyAny>>) -> Bound<'_, PyAny> {
    let index = Py::new(py, FakeIndex { symbols }).unwrap();
    module(py, "conductor.reuse.repo_evidence")
        .getattr("native_reuse_candidates")
        .unwrap()
        .call1((index,))
        .unwrap()
}

fn graph_fixture(root: &Path, database: &Path) {
    let connection = Connection::open(database).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE nodes (name TEXT, file_path TEXT, line_start INTEGER, language TEXT, \
             params TEXT, kind TEXT, is_test INTEGER, qualified_name TEXT); \
             CREATE TABLE edges (kind TEXT, target_qualified TEXT, source_qualified TEXT);",
        )
        .unwrap();
    let rows = [
        (
            "parse_batch",
            "pkg/parser.py",
            17,
            "python",
            "(rows)",
            0,
            "pkg.parser.parse_batch",
        ),
        (
            "test_parse_batch",
            "tests/test_parser.py",
            9,
            "python",
            "()",
            1,
            "tests.test_parser.test_parse_batch",
        ),
        (
            "parse_batch_js",
            "web/parser.js",
            4,
            "javascript",
            "(rows)",
            0,
            "web.parser.parse_batch_js",
        ),
        (
            "external_parse_batch",
            "/outside/parser.py",
            3,
            "python",
            "(rows)",
            0,
            "outside.parser.parse_batch",
        ),
    ];
    for (name, file, line, language, parameters, test, qualified) in rows {
        let absolute = if file.starts_with('/') {
            file.to_owned()
        } else {
            root.join(file).to_str().unwrap().to_owned()
        };
        connection
            .execute(
                "INSERT INTO nodes VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
                params![name, absolute, line, language, parameters, "Function", test, qualified],
            )
            .unwrap();
    }
    for index in 0..25 {
        connection
            .execute(
                "INSERT INTO edges VALUES (?, ?, ?)",
                params![
                    "CALLS",
                    "pkg.parser.parse_batch",
                    format!("consumer.{index}")
                ],
            )
            .unwrap();
    }
    connection
        .execute(
            "INSERT INTO edges VALUES (?, ?, ?)",
            params!["REFERENCES", "pkg.parser.parse_batch", "consumer.0"],
        )
        .unwrap();
    drop(connection);
}

#[test]
fn native_reuse_rows_batch_filters_and_caps_callers() {
    let case = Case::new();
    let database = case.root().join("graph.db");
    graph_fixture(case.root(), &database);
    Python::attach(|py| {
        let rows = module(py, "conductor.reuse.graph_index")
            .getattr("GraphIndex")
            .unwrap()
            .call1((path(py, case.root()), path(py, &database)))
            .unwrap()
            .call_method0("native_reuse_rows")
            .unwrap();
        let expected = py_json(
            py,
            json!([{
                "name":"parse_batch", "file":"pkg/parser.py", "line_start":17,
                "language":"python", "params":"(rows)", "caller_count":20
            }]),
        );
        assert!(rows.eq(expected).unwrap());
    });
}

#[test]
fn native_reuse_normalizes_suffix_noise_and_uses_callers() {
    let _case = Case::new();
    Python::attach(|py| {
        let python = symbol(
            py,
            (
                "interaction_metrics",
                "python::interaction_metrics",
                "research/eval/metrics.py",
                10,
                20,
                "python",
                "(x)",
            ),
        );
        let native = symbol(
            py,
            (
                "interaction_metrics_f32_py",
                "cpp::interaction_metrics_f32_py",
                "aria_core/bindings/bind_graph.cpp",
                440,
                453,
                "cpp",
                "(x)",
            ),
        );
        let stable_id = native.getattr("stable_id").unwrap();
        let candidates = candidate_rows(py, vec![python.unbind(), native.unbind()]);
        assert_eq!(candidates.len().unwrap(), 1);
        let first = candidates.get_item(0).unwrap();
        assert!(first
            .get_item("native_targets")
            .unwrap()
            .eq(PyList::new(py, [stable_id]).unwrap())
            .unwrap());
        assert!(first.get_item("severity").unwrap().eq("high").unwrap());
        assert!(first
            .get_item("evidence_complete")
            .unwrap()
            .is(PyBool::new(py, true)));
    });
}

#[test]
fn native_reuse_filters_common_symbol_names() {
    let _case = Case::new();
    Python::attach(|py| {
        let python = symbol(
            py,
            (
                "forward",
                "python::forward",
                "runner.py",
                1,
                2,
                "python",
                "()",
            ),
        );
        let native = symbol(
            py,
            (
                "forward_native",
                "rust::forward_native",
                "runner.rs",
                1,
                2,
                "rust",
                "()",
            ),
        );
        let candidates = candidate_rows(py, vec![python.unbind(), native.unbind()]);
        assert!(candidates.eq(PyList::empty(py)).unwrap());
    });
}
