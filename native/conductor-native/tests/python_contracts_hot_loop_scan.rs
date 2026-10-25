#![cfg(feature = "python-compat-tests")]
//! Rust assertions for the hot-loop scan wrapper and its `[tool.forge.hot_paths]` loader.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::prelude::*;
use support::{module, path, text, AttrPatch, Case};

const LOOP_OVER_TENSOR: &str =
    "def f():\n    t = torch.zeros(4)\n    for row in t:\n        pass\n";
const LOOP_OVER_NAMES: &str = "def f(names):\n    for name in names:\n        pass\n";
const HOT_TABLE: &str = "[tool.forge.hot_paths]\npaths = [\"hot\"]\n";

fn repo(case: &Case, pyproject: &str) {
    case.write("pyproject.toml", pyproject);
    case.write("hot/loop.py", LOOP_OVER_TENSOR);
    case.write("hot/names.py", LOOP_OVER_NAMES);
    case.write("cold/loop.py", LOOP_OVER_TENSOR);
}

fn run(py: Python<'_>, args: &[&str]) -> (i64, String) {
    let sink = module(py, "io")
        .getattr("StringIO")
        .unwrap()
        .call0()
        .unwrap();
    let _guard = AttrPatch::replace(&module(py, "sys").into_any(), "stderr", &sink);
    let code = module(py, "conductor.candidate_review.hot_loop_scan")
        .getattr("main")
        .unwrap()
        .call1((args,))
        .unwrap()
        .extract()
        .unwrap();
    (code, text(&sink.call_method0("getvalue").unwrap()))
}

fn load(py: Python<'_>, root: &std::path::Path) -> PyResult<Vec<String>> {
    module(py, "conductor.hot_paths_config")
        .getattr("hot_paths")?
        .call1((path(py, root),))?
        .extract()
}

#[test]
fn cli_flags_only_the_loop_over_a_tensor_in_a_hot_folder() {
    let case = Case::new();
    repo(&case, HOT_TABLE);
    let _cwd = case.chdir(".");
    Python::attach(|py| {
        let (code, err) = run(py, &["hot/loop.py", "hot/names.py", "cold/loop.py"]);
        assert_eq!(code, 1);
        assert!(err.contains("hot folders: hot"), "{err}");
        assert!(
            err.contains("hot/loop.py:3: perf/python-tensor-loop"),
            "{err}"
        );
        assert!(!err.contains("hot/names.py"), "{err}");
        assert!(!err.contains("cold/loop.py"), "{err}");
    });
}

#[test]
fn cli_passes_a_hot_folder_with_no_array_loop() {
    let case = Case::new();
    repo(&case, HOT_TABLE);
    let _cwd = case.chdir(".");
    Python::attach(|py| {
        let (code, err) = run(py, &["hot/names.py"]);
        assert_eq!(code, 0, "{err}");
        assert!(err.contains("0 finding(s)"), "{err}");
    });
}

#[test]
fn cli_without_the_table_says_so_and_finds_nothing() {
    let case = Case::new();
    repo(&case, "[project]\nname = \"x\"\n");
    let _cwd = case.chdir(".");
    Python::attach(|py| {
        let (code, err) = run(py, &["hot/loop.py"]);
        assert_eq!(code, 0);
        assert!(err.contains("no [tool.forge.hot_paths]"), "{err}");
    });
}

#[test]
fn version_answers() {
    Python::attach(|py| {
        let (code, _) = run(py, &["--version"]);
        assert_eq!(code, 0);
    });
}

#[test]
fn loader_reads_the_declared_folders() {
    let case = Case::new();
    case.write(
        "pyproject.toml",
        "[tool.forge.hot_paths]\npaths = [\"a\", \"b/c\"]\n",
    );
    Python::attach(|py| assert_eq!(load(py, case.root()).unwrap(), ["a", "b/c"]));
}

#[test]
fn loader_returns_nothing_without_the_table() {
    let case = Case::new();
    case.write("pyproject.toml", "[tool.other]\nx = 1\n");
    Python::attach(|py| assert!(load(py, case.root()).unwrap().is_empty()));
}

#[test]
fn loader_rejects_an_unknown_key_and_every_malformed_table() {
    for table in [
        "[tool.forge.hot_paths]\npaths = [\"a\"]\nextra = 1\n",
        "[tool.forge.hot_paths]\npaths = []\n",
        "[tool.forge.hot_paths]\n",
        "[tool.forge.hot_paths]\npaths = [\"/abs\"]\n",
        "[tool.forge.hot_paths]\npaths = [\"../up\"]\n",
        "[tool.forge.hot_paths]\npaths = [1]\n",
    ] {
        let case = Case::new();
        case.write("pyproject.toml", table);
        Python::attach(|py| {
            let error = load(py, case.root()).expect_err(table);
            assert!(error.to_string().contains("hot_paths"), "{table}: {error}");
        });
    }
}
