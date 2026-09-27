#![cfg(feature = "python-compat-tests")]
//! Rust-owned contracts for the advisory graph test selector.

#[path = "python_contracts/graph_test_select_support.rs"]
mod graph_support;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use graph_support::{capture, graph_module, GraphRepo};
use pyo3::prelude::*;
use pyo3::types::{PyBool, PyList};
use serde_json::Value;
use std::fs;
use std::path::Path;
use support::{path, Case};

fn selected(result: &Bound<'_, pyo3::types::PyAny>, relative: &str) -> bool {
    result
        .call_method1("__contains__", (relative,))
        .unwrap()
        .extract()
        .unwrap()
}

#[test]
fn convention_tests_for_path() {
    let case = Case::new();
    let repo = GraphRepo::new(&case);
    repo.write("pkg/engine.py", "def run(): pass\n");
    repo.write("pkg/test_engine.py", "def test_run(): pass\n");
    Python::attach(|py| {
        let result = graph_module(py)
            .getattr("convention_tests_for_path")
            .unwrap()
            .call1((path(py, repo.root()), "pkg/engine.py"))
            .unwrap();
        assert!(selected(&result, "pkg/test_engine.py"));
    });
}

#[test]
fn convention_tests_for_self_test() {
    let case = Case::new();
    let repo = GraphRepo::new(&case);
    repo.write("tests/test_suite.py", "def test_one(): pass\n");
    Python::attach(|py| {
        let graph = graph_module(py);
        let is_test = graph.getattr("is_test_file").unwrap();
        let positive = is_test.call1(("tests/test_suite.py",)).unwrap();
        let negative = is_test.call1(("pkg/engine.py",)).unwrap();
        assert!(positive.is(PyBool::new(py, true)));
        assert!(negative.is(PyBool::new(py, false)));
        let result = graph
            .getattr("convention_tests_for_path")
            .unwrap()
            .call1((path(py, repo.root()), "tests/test_suite.py"))
            .unwrap();
        assert!(selected(&result, "tests/test_suite.py"));
    });
}

#[test]
fn query_graph_tests() {
    let case = Case::new();
    let repo = GraphRepo::new(&case);
    repo.write("tests/test_engine.py", "def test_it(): pass\n");
    // The source does not exist in the original fixture; Python Path.resolve
    // still supplies its absolute graph key.
    repo.edge("pkg/engine.py", "tests/test_engine.py");
    Python::attach(|py| {
        let result = graph_module(py)
            .getattr("query_graph_tests")
            .unwrap()
            .call1((path(py, repo.root()), vec!["pkg/engine.py"]))
            .unwrap();
        assert!(selected(&result, "tests/test_engine.py"));
    });
}

fn unavailable(damage: &str) {
    let case = Case::new();
    let repo = GraphRepo::new(&case);
    match damage {
        "missing" => fs::remove_file(repo.database()).unwrap(),
        "corrupt" => fs::write(repo.database(), b"not a sqlite database\n").unwrap(),
        _ => panic!("unknown graph damage {damage}"),
    }
    repo.write("pkg/engine.py", "x = 1\n");
    repo.write("pkg/test_engine.py", "def test_x(): pass\n");
    Python::attach(|py| {
        let graph = graph_module(py);
        let error_class = graph.getattr("GraphSelectError").unwrap();
        let (code, stdout, stderr) = capture(py, || {
            let query_error = graph
                .getattr("query_graph_tests")
                .unwrap()
                .call1((path(py, repo.root()), vec!["pkg/engine.py"]))
                .unwrap_err();
            assert!(query_error.matches(py, &error_class).unwrap());
            assert!(query_error.to_string().contains("code-review graph"));
            let select_error = graph
                .getattr("select_tests_for_sources")
                .unwrap()
                .call1((path(py, repo.root()), vec!["pkg/engine.py"]))
                .unwrap_err();
            assert!(select_error.matches(py, &error_class).unwrap());
            graph
                .getattr("main")
                .unwrap()
                .call1((vec![
                    "--repo".to_owned(),
                    repo.root().to_string_lossy().into_owned(),
                    "--json".to_owned(),
                    "pkg/engine.py".to_owned(),
                ],))
                .unwrap()
                .extract::<i64>()
                .unwrap()
        });
        assert_eq!(code, 2);
        assert_eq!(stdout, "");
        assert!(stderr.contains("graph-test-select ERROR"));
    });
}

#[test]
fn graph_unavailable_fails_loud_missing() {
    unavailable("missing");
}

#[test]
fn graph_unavailable_fails_loud_corrupt() {
    unavailable("corrupt");
}

#[test]
fn select_tests_for_sources() {
    let case = Case::new();
    let repo = GraphRepo::new(&case);
    repo.write("pkg/calc.py", "def add(): pass\n");
    repo.write("pkg/test_calc.py", "def test_add(): pass\n");
    Python::attach(|py| {
        let graph = graph_module(py);
        let select = graph.getattr("select_tests_for_sources").unwrap();
        let chosen = select
            .call1((path(py, repo.root()), vec!["pkg/calc.py"]))
            .unwrap();
        assert!(chosen
            .eq(PyList::new(py, ["pkg/test_calc.py"]).unwrap())
            .unwrap());
        let non_source = select
            .call1((path(py, repo.root()), vec!["README.md"]))
            .unwrap();
        assert!(non_source.eq(PyList::empty(py)).unwrap());
    });
}

#[test]
fn git_changed_and_untracked_files() {
    let case = Case::new();
    let repo = GraphRepo::new(&case);
    repo.write("untracked.py", "x = 1\n");
    repo.write("base.py", "def base_modified(): pass\n");
    Python::attach(|py| {
        let changed: Vec<String> = graph_module(py)
            .getattr("git_changed_and_untracked_files")
            .unwrap()
            .call1((path(py, repo.root()),))
            .unwrap()
            .extract()
            .unwrap();
        assert!(changed.contains(&"base.py".to_owned()));
        assert!(changed.contains(&"untracked.py".to_owned()));
    });
}

#[test]
fn run_tests_empty() {
    let case = Case::new();
    Python::attach(|py| {
        let (code, stdout, _) = capture(py, || {
            graph_module(py)
                .getattr("run_tests")
                .unwrap()
                .call1((path(py, Path::new(".")), Vec::<String>::new()))
                .unwrap()
                .extract::<i64>()
                .unwrap()
        });
        assert_eq!(code, 0);
        assert!(stdout.contains("No targeted tests selected."));
    });
    drop(case);
}

#[test]
fn main_cli() {
    let case = Case::new();
    let repo = GraphRepo::new(&case);
    repo.write("pkg/cli_src.py", "x = 1\n");
    repo.write("pkg/test_cli_src.py", "def test_cli(): pass\n");
    Python::attach(|py| {
        let graph = graph_module(py);
        let root = repo.root().to_string_lossy().into_owned();
        let (code, stdout, _) = capture(py, || {
            graph
                .getattr("main")
                .unwrap()
                .call1((vec!["--repo", &root, "--json", "pkg/cli_src.py"],))
                .unwrap()
                .extract::<i64>()
                .unwrap()
        });
        assert_eq!(code, 0);
        let data: Value = serde_json::from_str(&stdout).unwrap();
        assert_eq!(
            data["selected_tests"],
            serde_json::json!(["pkg/test_cli_src.py"])
        );
        assert_eq!(data["scope"], "direct-dependencies-and-conventions");
        let (plain, _, _) = capture(py, || {
            graph
                .getattr("main")
                .unwrap()
                .call1((vec!["--repo", &root, "pkg/cli_src.py"],))
                .unwrap()
                .extract::<i64>()
                .unwrap()
        });
        assert_eq!(plain, 0);
    });
}
