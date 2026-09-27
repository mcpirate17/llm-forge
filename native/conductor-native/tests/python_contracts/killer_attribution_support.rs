//! Exact mutation-record and per-test-report fixtures for attribution contracts.

use crate::support::{module, path};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyTuple};
use std::path::Path;

pub const DECLARED: &str = "pkg/test_thing.py::test_declared";
pub const OTHER: &str = "pkg/test_thing.py::test_other";

pub fn mutation<'py>(py: Python<'py>, killers: &[&str]) -> Bound<'py, PyAny> {
    let kwargs = PyDict::new(py);
    kwargs.set_item("mutation_id", "m1").unwrap();
    kwargs
        .set_item("patch_file", path(py, Path::new("patches/m1.patch")))
        .unwrap();
    kwargs.set_item("patch_sha256", "0".repeat(64)).unwrap();
    kwargs
        .set_item("allowed_paths", PyTuple::new(py, ["pkg/thing.py"]).unwrap())
        .unwrap();
    kwargs
        .set_item("expected_killers", PyTuple::new(py, killers).unwrap())
        .unwrap();
    module(py, "conductor.mutation_testing")
        .getattr("Mutation")
        .unwrap()
        .call((), Some(&kwargs))
        .unwrap()
}

pub fn report<'py>(
    py: Python<'py>,
    status: &str,
    declared: Option<&str>,
    other: Option<&str>,
) -> Bound<'py, PyAny> {
    let tests = PyDict::new(py);
    for (nodeid, outcome) in [(DECLARED, declared), (OTHER, other)] {
        if let Some(outcome) = outcome {
            let row = PyDict::new(py);
            row.set_item("outcome", outcome).unwrap();
            row.set_item("duration_seconds", 0.1).unwrap();
            row.set_item("cases", 1).unwrap();
            tests.set_item(nodeid, row).unwrap();
        }
    }
    let result = PyDict::new(py);
    result.set_item("status", status).unwrap();
    result.set_item("tests", tests).unwrap();
    result
        .set_item("missing_nodeids", Vec::<String>::new())
        .unwrap();
    result
        .set_item("unmapped_cases", Vec::<String>::new())
        .unwrap();
    result.into_any()
}

pub fn verdict<'py>(
    py: Python<'py>,
    killers: &[&str],
    report: Option<&Bound<'py, PyAny>>,
    outcome: &str,
) -> Bound<'py, PyAny> {
    module(py, "conductor.mutation_testing")
        .getattr("killer_verdict")
        .unwrap()
        .call1((mutation(py, killers), report, outcome))
        .unwrap()
}

pub fn supported(py: Python<'_>, argv: &[&str], ranked: &[&str]) -> bool {
    module(py, "conductor.mutation_value")
        .getattr("pytest_attribution_supported")
        .unwrap()
        .call1((
            PyTuple::new(py, argv).unwrap(),
            PyTuple::new(py, ranked).unwrap(),
        ))
        .unwrap()
        .extract()
        .unwrap()
}
