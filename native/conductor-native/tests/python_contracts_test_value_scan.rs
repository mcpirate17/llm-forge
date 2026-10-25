#![cfg(feature = "python-compat-tests")]
//! Rust assertions for the test-value-scan CLI: version answer, span scoping, exit codes.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::prelude::*;
use pyo3::types::PyModule;
use std::process::Command;
use support::{module, AttrPatch, Case};

const ASSERTLESS: &str = "def test_a():\n    run()\n    other()\n";

fn cli<'py>(py: Python<'py>) -> Bound<'py, PyModule> {
    module(py, "conductor.candidate_review.test_value_scan")
}

fn git(case: &Case, args: &[&str]) {
    let status = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
        .args(args)
        .current_dir(case.root())
        .status()
        .expect("run git");
    assert!(status.success(), "git {args:?} failed");
}

fn repo() -> Case {
    let case = Case::new();
    git(&case, &["init", "-q"]);
    case.write("tests/test_x.py", ASSERTLESS);
    git(&case, &["add", "."]);
    git(&case, &["commit", "-q", "-m", "base"]);
    case
}

/// Run `main(args)`; return its exit code and what it wrote to stdout and stderr.
fn run(py: Python<'_>, args: &[&str]) -> (i64, String, String) {
    let sys = module(py, "sys").into_any();
    let sinks: Vec<_> = ["stdout", "stderr"]
        .iter()
        .map(|stream| {
            let sink = module(py, "io")
                .getattr("StringIO")
                .unwrap()
                .call0()
                .unwrap();
            (AttrPatch::replace(&sys, stream, &sink), sink)
        })
        .collect();
    let code = cli(py)
        .getattr("main")
        .unwrap()
        .call1((args,))
        .unwrap()
        .extract()
        .unwrap();
    let read = |index: usize| {
        sinks[index]
            .1
            .call_method0("getvalue")
            .unwrap()
            .extract::<String>()
            .unwrap()
    };
    (code, read(0), read(1))
}

#[test]
fn version_answers_without_files() {
    let _case = Case::new();
    Python::attach(|py| {
        let (code, out, _) = run(py, &["--version"]);
        assert_eq!(code, 0);
        assert!(out.starts_with("test-value-scan "), "{out}");
    });
}

#[test]
fn full_tree_run_reports_the_assertless_test() {
    let case = repo();
    let _cwd = case.chdir("");
    Python::attach(|py| {
        let (code, _, err) = run(py, &["tests/test_x.py"]);
        assert_eq!(code, 1);
        assert!(
            err.contains("tests/test_x.py:1: test/no-assertion:"),
            "{err}"
        );
    });
}

#[test]
fn non_python_files_are_ignored() {
    let case = repo();
    let _cwd = case.chdir("");
    Python::attach(|py| {
        assert_eq!(run(py, &["tests/data.json"]).0, 0);
    });
}

#[test]
fn an_edit_to_the_body_holds_the_candidate_to_the_def_line_finding() {
    let case = repo();
    let _cwd = case.chdir("");
    case.write("tests/test_x.py", &ASSERTLESS.replace("other()", "more()"));
    Python::attach(|py| {
        let (code, _, err) = run(py, &["--base", "HEAD", "tests/test_x.py"]);
        assert_eq!(code, 1);
        assert!(err.contains("no-assertion"), "{err}");
    });
}

#[test]
fn an_edit_outside_the_test_is_not_held_to_it() {
    let case = repo();
    let _cwd = case.chdir("");
    case.write("tests/test_x.py", &format!("{ASSERTLESS}\nVALUE = 1\n"));
    Python::attach(|py| {
        assert_eq!(run(py, &["--base", "HEAD", "tests/test_x.py"]).0, 0);
    });
}

#[test]
fn a_test_with_an_assert_passes() {
    let case = repo();
    let _cwd = case.chdir("");
    case.write("tests/test_x.py", "def test_a():\n    assert run() == 1\n");
    Python::attach(|py| {
        assert_eq!(run(py, &["--base", "HEAD", "tests/test_x.py"]).0, 0);
    });
}
