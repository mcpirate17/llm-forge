//! Rust fixture objects for candidate finding attribution contracts.

use crate::support::module;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyList, PySet, PyTuple};

fn namespace<'py>(py: Python<'py>, values: &[(&str, Bound<'py, PyAny>)]) -> Bound<'py, PyAny> {
    let kwargs = PyDict::new(py);
    for (name, value) in values {
        kwargs.set_item(*name, value).unwrap();
    }
    module(py, "types")
        .getattr("SimpleNamespace")
        .unwrap()
        .call((), Some(&kwargs))
        .unwrap()
}

pub fn context<'py>(
    py: Python<'py>,
    changes: &[(Option<&str>, Option<&str>)],
    checks: &[(&str, &str)],
) -> Bound<'py, PyAny> {
    let changes = changes.iter().map(|(path, old_path)| {
        namespace(
            py,
            &[
                ("path", path.into_pyobject(py).unwrap().into_any()),
                ("old_path", old_path.into_pyobject(py).unwrap().into_any()),
            ],
        )
    });
    let changes = PyList::new(py, changes).unwrap();
    let checks = checks.iter().map(|(check_id, attribution)| {
        namespace(
            py,
            &[
                ("check_id", check_id.into_pyobject(py).unwrap().into_any()),
                (
                    "attribution",
                    attribution.into_pyobject(py).unwrap().into_any(),
                ),
            ],
        )
    });
    let checks = PyTuple::new(py, checks).unwrap();
    namespace(
        py,
        &[
            (
                "candidate",
                namespace(py, &[("changes", changes.into_any())]),
            ),
            ("policy", namespace(py, &[("checks", checks.into_any())])),
        ],
    )
}

pub fn finding<'py>(py: Python<'py>, check_id: &str, location: Option<&str>) -> Bound<'py, PyAny> {
    let model = module(py, "conductor.candidate_review.model");
    let kwargs = PyDict::new(py);
    kwargs.set_item("check_id", check_id).unwrap();
    kwargs.set_item("rule_id", "rule").unwrap();
    kwargs
        .set_item(
            "severity",
            model.getattr("Severity").unwrap().getattr("HIGH").unwrap(),
        )
        .unwrap();
    kwargs.set_item("message", "m").unwrap();
    kwargs.set_item("path", location).unwrap();
    model
        .getattr("Finding")
        .unwrap()
        .call((), Some(&kwargs))
        .unwrap()
}

pub fn changed<'py>(py: Python<'py>, ctx: &Bound<'py, PyAny>) -> Bound<'py, PyAny> {
    module(py, "conductor.candidate_review.engine")
        .getattr("candidate_changed_paths")
        .unwrap()
        .call1((ctx,))
        .unwrap()
}

pub fn mark(py: Python<'_>, ctx: &Bound<'_, PyAny>, findings: &[&Bound<'_, PyAny>]) {
    let rows = PyList::new(py, findings.iter().copied()).unwrap();
    module(py, "conductor.candidate_review.engine")
        .getattr("mark_inherited")
        .unwrap()
        .call1((ctx, rows, changed(py, ctx)))
        .unwrap();
}

pub fn equal_set(py: Python<'_>, actual: &Bound<'_, PyAny>, expected: &[&str]) -> bool {
    actual.eq(PySet::new(py, expected).unwrap()).unwrap()
}

pub fn attribution<'py>(py: Python<'py>, value: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    let kwargs = PyDict::new(py);
    kwargs.set_item("field", "f").unwrap();
    module(py, "conductor.candidate_review.policy")
        .getattr("_attribution_value")
        .unwrap()
        .call((value,), Some(&kwargs))
}
