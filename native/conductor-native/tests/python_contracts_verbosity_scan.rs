#![cfg(feature = "python-compat-tests")]
//! Rust assertions for the verbosity-scan wrapper contract.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyList, PyModule};
use support::{module, text, AttrPatch, Case};

const HEAVY: &str = "def heavy(x):\n    \"\"\"Docstring.\"\"\"\n    # step 0 of the plan\n    # step 1 of the plan\n    # step 2 of the plan\n    # step 3 of the plan\n    # step 4 of the plan\n    # step 5 of the plan\n    # step 6 of the plan\n    # step 7 of the plan\n    # step 8 of the plan\n    return x\n";
const LIGHT: &str = "def light(x):\n    # one note\n    return x\n";

fn verbosity<'py>(py: Python<'py>) -> Bound<'py, PyModule> {
    module(py, "conductor.candidate_review.verbosity_scan")
}

fn main(py: Python<'_>, args: &[&str]) -> i64 {
    verbosity(py)
        .getattr("main")
        .unwrap()
        .call1((args,))
        .unwrap()
        .extract()
        .unwrap()
}

fn capture<'py>(py: Python<'py>) -> (Bound<'py, PyAny>, AttrPatch) {
    let sink = module(py, "io")
        .getattr("StringIO")
        .unwrap()
        .call0()
        .unwrap();
    let guard = AttrPatch::replace(&module(py, "sys").into_any(), "stdout", &sink);
    (sink, guard)
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
fn version_prints_and_exits_zero() {
    let _case = Case::new();
    Python::attach(|py| {
        let (out, _capture) = capture(py);
        assert_eq!(main(py, &["--version"]), 0);
        assert!(text(&out.call_method0("getvalue").unwrap()).starts_with("verbosity-scan"));
    });
}

#[test]
fn comment_heavy_function_is_reported_and_exits_one() {
    let case = Case::new();
    let src = case.write("heavy.py", HEAVY);
    let src = src.to_string_lossy().to_string();
    Python::attach(|py| {
        let (out, _capture) = capture(py);
        assert_eq!(main(py, &[&src]), 1);
        let printed = text(&out.call_method0("getvalue").unwrap());
        assert!(printed.contains(&format!("{src}:1: comment/comment-heavy-function")));
        assert!(printed.contains("1 finding(s)"));
    });
}

#[test]
fn light_function_exits_zero() {
    let case = Case::new();
    let src = case.write("light.py", LIGHT);
    let src = src.to_string_lossy().to_string();
    Python::attach(|py| {
        let (out, _capture) = capture(py);
        assert_eq!(main(py, &[&src]), 0);
        assert!(text(&out.call_method0("getvalue").unwrap()).contains("0 finding(s)"));
    });
}

#[test]
fn non_python_paths_are_ignored() {
    let case = Case::new();
    let other = case.write("notes.txt", HEAVY);
    let other = other.to_string_lossy().to_string();
    Python::attach(|py| {
        let (out, _capture) = capture(py);
        assert_eq!(main(py, &[&other]), 0);
        assert_eq!(text(&out.call_method0("getvalue").unwrap()), "");
    });
}

fn with_changed_lines(ranges: &[(i64, i64)]) -> i64 {
    let case = Case::new();
    let src = case.write("heavy.py", HEAVY);
    let src = src.to_string_lossy().to_string();
    Python::attach(|py| {
        let mock = mock_return(py, &PyList::new(py, ranges.to_vec()).unwrap().into_any());
        let _style = AttrPatch::replace(
            &module(py, "conductor.candidate_review.style_scan"),
            "changed_line_ranges",
            &mock,
        );
        let _verbosity = AttrPatch::replace(&verbosity(py), "changed_line_ranges", &mock);
        let (_out, _capture) = capture(py);
        main(py, &["--base", "HEAD", &src])
    })
}

#[test]
fn change_inside_the_body_keeps_the_finding() {
    // Line 1 is the def line; only line 6, inside the body, changed.
    assert_eq!(with_changed_lines(&[(6, 6)]), 1);
}

#[test]
fn change_outside_the_function_drops_the_finding() {
    assert_eq!(with_changed_lines(&[(40, 50)]), 0);
}
