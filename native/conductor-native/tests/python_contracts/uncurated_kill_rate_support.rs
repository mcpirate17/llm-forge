//! Rust-owned source and campaign fixtures for the legacy uncurated scorer.

use crate::support::{module, path};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyList, PySet};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};

pub const SUBJECT: &str = "LIMIT = 10\nMESSAGE = 'unread'\n\n\ndef classify(n):\n    if n < LIMIT:\n        return 'small'\n    return 'large'\n";

pub const SUBJECT_TESTS: &str = "from subject import classify\n\n\ndef test_below_the_limit():\n    assert classify(1) == 'small'\n\n\ndef test_at_the_limit():\n    assert classify(10) == 'large'\n";

pub fn api(py: Python<'_>) -> Bound<'_, pyo3::types::PyModule> {
    module(py, "conductor.uncurated_kill_rate")
}

pub fn generate<'py>(
    py: Python<'py>,
    source: &str,
    path_name: &str,
    covered: &[usize],
) -> Bound<'py, PyList> {
    api(py)
        .getattr("generate")
        .unwrap()
        .call1((source, path_name, PySet::new(py, covered).unwrap()))
        .unwrap()
        .cast_into::<PyList>()
        .unwrap()
}

pub fn all_lines(source: &str) -> Vec<usize> {
    (1..=source.lines().count()).collect()
}

pub fn sources(py: Python<'_>, source: &str, covered: Option<&[usize]>) -> Vec<String> {
    let all = all_lines(source);
    let rows = generate(py, source, "subject.py", covered.unwrap_or(&all));
    rows.iter()
        .map(|row| row.getattr("source").unwrap().extract().unwrap())
        .collect()
}

pub fn details(py: Python<'_>, source: &str, covered: Option<&[usize]>) -> Vec<String> {
    let all = all_lines(source);
    let rows = generate(py, source, "subject.py", covered.unwrap_or(&all));
    rows.iter()
        .map(|row| row.getattr("detail").unwrap().extract().unwrap())
        .collect()
}

pub fn fixture(root: &Path, subjects: Value) -> PathBuf {
    fs::write(root.join("subject.py"), SUBJECT).unwrap();
    fs::write(root.join("test_subject.py"), SUBJECT_TESTS).unwrap();
    let manifest = root.join("campaign.json");
    let contents = json!({
        "source_sha256": subjects,
        "baseline": {"argv": [
            "python", "-m", "pytest", "-q", "-o", "addopts=", "--rootdir=.", "test_subject.py"
        ]}
    });
    fs::write(&manifest, serde_json::to_string(&contents).unwrap()).unwrap();
    manifest
}

pub fn measure<'py>(py: Python<'py>, root: &Path, manifest: &Path) -> Bound<'py, PyAny> {
    let kwargs = pyo3::types::PyDict::new(py);
    kwargs.set_item("cap", 50).unwrap();
    kwargs.set_item("seed", 0).unwrap();
    kwargs.set_item("budget", 300.0).unwrap();
    api(py)
        .getattr("measure")
        .unwrap()
        .call((path(py, root), path(py, manifest)), Some(&kwargs))
        .unwrap()
}
