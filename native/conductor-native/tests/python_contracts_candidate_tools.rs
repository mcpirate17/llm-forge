#![cfg(feature = "python-compat-tests")]
//! Clang candidate-check command and scope contracts, asserted in Rust.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::prelude::*;
use pyo3::types::{PyDict, PyModule};
use std::path::Path;
use support::{module, path, text, AttrPatch, Case};

fn kwargs<'py>(py: Python<'py>, root: &Path) -> Bound<'py, PyDict> {
    let kwargs = PyDict::new(py);
    kwargs.set_item("root", path(py, root)).unwrap();
    kwargs
}

fn mock_return<'py>(py: Python<'py>, value: &Bound<'py, PyAny>) -> Bound<'py, PyAny> {
    let kwargs = PyDict::new(py);
    kwargs.set_item("return_value", value).unwrap();
    module(py, "unittest.mock")
        .getattr("Mock")
        .unwrap()
        .call((), Some(&kwargs))
        .unwrap()
}

#[test]
fn clang_format_and_tidy_commands_scope_lines_and_database() {
    let _case = Case::new();
    Python::attach(|py| {
        let clang = module(py, "conductor.candidate_review.clang_files");
        let format = clang.getattr("_format_command").unwrap();
        let command: Vec<String> = format
            .call1(("clang-format", "f.c", vec![(3, 5), (9, 9)]))
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(
            command,
            [
                "clang-format",
                "--style=file",
                "--dry-run",
                "--Werror",
                "--lines=3:5",
                "--lines=9:9",
                "f.c"
            ]
        );
        let whole: Vec<String> = format
            .call1(("clang-format", "f.c", py.None()))
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(whole.last().unwrap(), "f.c");
        assert!(!whole.iter().any(|item| item.starts_with("--lines")));

        let tidy = clang.getattr("_tidy_command").unwrap();
        let db = path(py, Path::new("/db/compile_commands.json"));
        let scoped: Vec<String> = tidy
            .call1(("clang-tidy", "a/b/f.cpp", vec![(2, 4)], &db))
            .unwrap()
            .extract()
            .unwrap();
        assert!(scoped.contains(&"-p=/db".to_owned()));
        assert!(scoped.contains(&"--warnings-as-errors=*".to_owned()));
        let filter = scoped
            .iter()
            .find(|item| item.starts_with("-line-filter="))
            .unwrap();
        let value: serde_json::Value =
            serde_json::from_str(filter.split_once('=').unwrap().1).unwrap();
        assert_eq!(value, serde_json::json!([{"name":"f.cpp","lines":[[2,4]]}]));
        let unscoped: Vec<String> = tidy
            .call1(("clang-tidy", "f.cpp", py.None(), &db))
            .unwrap()
            .extract()
            .unwrap();
        assert!(!unscoped.iter().any(|item| item.starts_with("-line-filter")));
        assert!(unscoped.contains(&"--warnings-as-errors=*".to_owned()));
    });
}

#[test]
fn clang_database_prefers_nearest_then_build_and_refuses_absent() {
    let case = Case::new();
    case.mkdir("src");
    Python::attach(|py| {
        let clang = module(py, "conductor.candidate_review.clang_files");
        let compile_db = clang.getattr("_compile_db").unwrap();
        let source = path(py, &case.root().join("src/f.c"));
        let kwargs = kwargs(py, case.root());
        assert!(compile_db
            .call((&source,), Some(&kwargs))
            .unwrap()
            .is_none());
        let build = case.write("build/compile_commands.json", "[]");
        let found = compile_db.call((&source,), Some(&kwargs)).unwrap();
        assert_eq!(text(&found), build.to_string_lossy());
        let beside = case.write("src/compile_commands.json", "[]");
        let found = compile_db.call((&source,), Some(&kwargs)).unwrap();
        assert_eq!(text(&found), beside.to_string_lossy());
    });
}

#[test]
fn clang_analyzable_distinguishes_cuda_missing_database_and_present_database() {
    let case = Case::new();
    Python::attach(|py| {
        let clang = module(py, "conductor.candidate_review.clang_files");
        let stderr = PyModule::import(py, "io")
            .unwrap()
            .getattr("StringIO")
            .unwrap()
            .call0()
            .unwrap();
        let _capture = AttrPatch::replace(
            &PyModule::import(py, "sys").unwrap().into_any(),
            "stderr",
            &stderr,
        );
        let analyzable = clang.getattr("_analyzable").unwrap();
        let kwargs = kwargs(py, case.root());
        let format = analyzable.call(("format", "k.cu"), Some(&kwargs)).unwrap();
        assert!(format.get_item(0).unwrap().extract::<bool>().unwrap());
        assert!(format.get_item(1).unwrap().is_none());
        let cuda = analyzable.call(("tidy", "k.cu"), Some(&kwargs)).unwrap();
        assert!(!cuda.get_item(0).unwrap().extract::<bool>().unwrap());
        assert!(text(&stderr.call_method0("getvalue").unwrap())
            .contains("not a host C/C++ translation unit"));
        let missing = analyzable.call(("tidy", "f.cpp"), Some(&kwargs)).unwrap();
        assert!(!missing.get_item(0).unwrap().extract::<bool>().unwrap());
        assert!(text(&stderr.call_method0("getvalue").unwrap())
            .contains("CMAKE_EXPORT_COMPILE_COMMANDS"));
        let db = case.write("compile_commands.json", "[]");
        let present = analyzable.call(("tidy", "f.cpp"), Some(&kwargs)).unwrap();
        assert!(present.get_item(0).unwrap().extract::<bool>().unwrap());
        assert_eq!(text(&present.get_item(1).unwrap()), db.to_string_lossy());
    });
}

#[test]
fn clang_tool_prefers_binary_beside_interpreter() {
    let case = Case::new();
    let beside = case.write("bin/clang-format", "synthetic tool\n");
    Python::attach(|py| {
        let sys = PyModule::import(py, "sys").unwrap();
        let executable = case.root().join("bin/python");
        let _executable = AttrPatch::replace(&sys.into_any(), "executable", &path(py, &executable));
        let tool = module(py, "conductor.candidate_review.clang_files")
            .getattr("_tool")
            .unwrap();
        assert_eq!(
            text(&tool.call1(("format",)).unwrap()),
            beside.to_string_lossy()
        );
    });
}

#[test]
fn clang_unchanged_file_never_invokes_subprocess() {
    let case = Case::new();
    Python::attach(|py| {
        let clang = module(py, "conductor.candidate_review.clang_files");
        let empty = pyo3::types::PyList::empty(py).into_any();
        let _ranges = AttrPatch::replace(&clang, "_ranges", &mock_return(py, &empty));
        let database = path(py, &case.root().join("db.json"));
        let allowed = pyo3::types::PyList::new(py, [py.None(), database.unbind()]).unwrap();
        allowed.set_item(0, true).unwrap();
        let allowed = allowed.into_any();
        let _analyzable = AttrPatch::replace(&clang, "_analyzable", &mock_return(py, &allowed));
        let error = PyModule::import(py, "builtins")
            .unwrap()
            .getattr("AssertionError")
            .unwrap()
            .call1(("clang was invoked",))
            .unwrap();
        let mock_kwargs = PyDict::new(py);
        mock_kwargs.set_item("side_effect", error).unwrap();
        let explode = module(py, "unittest.mock")
            .getattr("Mock")
            .unwrap()
            .call((), Some(&mock_kwargs))
            .unwrap();
        let subprocess = clang.getattr("subprocess").unwrap();
        let _run = AttrPatch::replace(&subprocess, "run", &explode);
        let kwargs = kwargs(py, case.root());
        kwargs.set_item("base", "HEAD").unwrap();
        assert_eq!(
            clang
                .getattr("_check_one")
                .unwrap()
                .call(("format", "clang-format", "f.c"), Some(&kwargs))
                .unwrap()
                .extract::<i64>()
                .unwrap(),
            0
        );
        assert_eq!(
            explode
                .getattr("call_count")
                .unwrap()
                .extract::<i64>()
                .unwrap(),
            0
        );
    });
}
