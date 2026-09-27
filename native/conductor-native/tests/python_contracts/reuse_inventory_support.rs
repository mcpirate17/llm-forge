//! Rust-owned fixtures for reuse inventory Python/native boundary contracts.

use crate::support::{module, path};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyList, PyModule};
use std::path::Path;

pub fn detectors(py: Python<'_>) -> Bound<'_, PyModule> {
    module(py, "conductor.reuse.detectors")
}

pub fn consolidation(py: Python<'_>) -> Bound<'_, PyModule> {
    module(py, "conductor.reuse.consolidation")
}

pub fn file_families(py: Python<'_>) -> Bound<'_, PyModule> {
    module(py, "conductor.reuse.file_families")
}

pub fn overrides(py: Python<'_>) -> Bound<'_, PyDict> {
    PyDict::new(py)
}

pub fn arguments<'py>(py: Python<'py>, overrides: &Bound<'py, PyDict>) -> Bound<'py, PyDict> {
    let arguments = PyDict::new(py);
    for key in [
        "god_files",
        "god_functions",
        "clusters",
        "families",
        "vulture",
        "ruff",
        "fallbacks",
        "token_clones",
        "native_reuse",
        "dependencies",
        "compliance",
        "contract_candidates",
    ] {
        arguments.set_item(key, PyList::empty(py)).unwrap();
    }
    arguments.set_item("limit", 80).unwrap();
    arguments.set_item("test_limit", 80).unwrap();
    for (key, value) in overrides {
        arguments.set_item(key, value).unwrap();
    }
    arguments
}

pub fn try_inventory<'py>(
    py: Python<'py>,
    repo: &Path,
    overrides: &Bound<'py, PyDict>,
) -> PyResult<Bound<'py, PyDict>> {
    Ok(detectors(py)
        .getattr("_inventory_candidates")?
        .call((path(py, repo),), Some(&arguments(py, overrides)))?
        .cast_into::<PyDict>()?)
}

pub fn inventory<'py>(
    py: Python<'py>,
    repo: &Path,
    overrides: &Bound<'py, PyDict>,
) -> Bound<'py, PyDict> {
    try_inventory(py, repo, overrides).unwrap()
}

pub fn candidate<'py>(
    py: Python<'py>,
    identifier: &str,
    value: i64,
    confidence: f64,
) -> Bound<'py, PyDict> {
    let result = PyDict::new(py);
    result.set_item("id", identifier).unwrap();
    result.set_item("value", value).unwrap();
    result.set_item("confidence", confidence).unwrap();
    result
}

pub fn site<'py>(py: Python<'py>, file: &str, line: i64, name: &str) -> Bound<'py, PyAny> {
    consolidation(py)
        .getattr("FuncRecord")
        .unwrap()
        .call1((file, line, line + 4, name, "hash", 10, "source"))
        .unwrap()
}

pub fn cluster<'py>(py: Python<'py>, sites: &Bound<'py, PyList>) -> Bound<'py, PyAny> {
    let options = PyDict::new(py);
    options.set_item("kind", "exact").unwrap();
    options.set_item("tokens", 20).unwrap();
    options.set_item("sites", sites).unwrap();
    options.set_item("confidence", 0.75).unwrap();
    options.set_item("value_score", 40).unwrap();
    options.set_item("disposition", "auto").unwrap();
    options
        .set_item("rationale", "same normalized body")
        .unwrap();
    consolidation(py)
        .getattr("Cluster")
        .unwrap()
        .call((), Some(&options))
        .unwrap()
}
