//! Rust-owned value and path fixtures for governance baseline merge contracts.

use crate::comm_support::py_json;
use crate::support::{module, path};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyList, PyModule};
use serde_json::Value;
use std::path::{Path, PathBuf};

pub fn baseline(py: Python<'_>) -> Bound<'_, PyModule> {
    module(py, "conductor.baseline_merge")
}

pub fn value(py: Python<'_>, json: Value) -> Bound<'_, PyAny> {
    py_json(py, json)
}

pub fn merge<'py>(
    py: Python<'py>,
    base: &Bound<'py, PyAny>,
    incoming: &Bound<'py, PyAny>,
) -> Bound<'py, PyAny> {
    baseline(py)
        .getattr("merge")
        .unwrap()
        .call1((base, incoming))
        .unwrap()
}

pub fn signature<'py>(
    py: Python<'py>,
    document: &Bound<'py, PyAny>,
    key: &str,
) -> Bound<'py, PyAny> {
    baseline(py)
        .getattr("signature")
        .unwrap()
        .call1((document, key))
        .unwrap()
}

pub fn main(py: Python<'_>, args: &[&str]) -> i64 {
    baseline(py)
        .getattr("main")
        .unwrap()
        .call1((PyList::new(py, args).unwrap(),))
        .unwrap()
        .extract()
        .unwrap()
}

pub fn real_src_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../src")
}

pub fn read_baseline<'py>(py: Python<'py>, file: &Path) -> Bound<'py, PyAny> {
    let kwargs = PyDict::new(py);
    kwargs.set_item("encoding", "utf-8").unwrap();
    let content = path(py, file)
        .call_method("read_text", (), Some(&kwargs))
        .unwrap();
    module(py, "json")
        .getattr("loads")
        .unwrap()
        .call1((content,))
        .unwrap()
}
