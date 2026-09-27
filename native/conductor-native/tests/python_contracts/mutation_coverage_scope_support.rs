//! Isolated Git and Python fixtures for mutation coverage and run-scope contracts.

use crate::comm_support::{bind_signature, py_json, signature};
use crate::support::{module, Case};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyCFunction, PyDict, PyTuple};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

pub fn equal(actual: &Bound<'_, PyAny>, expected: &Bound<'_, PyAny>) {
    assert!(
        actual.eq(expected).unwrap(),
        "actual={actual:?}, expected={expected:?}"
    );
}

pub fn git(repo: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

pub fn repo(case: &Case) -> PathBuf {
    let repo = case.mkdir("repo");
    git(&repo, &["init", "-q"]);
    git(&repo, &["config", "user.email", "test@example.invalid"]);
    git(&repo, &["config", "user.name", "test"]);
    repo
}

pub fn commit(repo: &Path, relative: &str, contents: &str, message: &str) -> PathBuf {
    let target = repo.join(relative);
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    fs::write(&target, contents).unwrap();
    git(repo, &["add", relative]);
    git(repo, &["commit", "-qm", message]);
    target
}

pub fn registry(py: Python<'_>, repo: &Path) -> PathBuf {
    let location = repo.join("conductor/mutation_campaigns/registry.json");
    fs::create_dir_all(location.parent().unwrap()).unwrap();
    let patterns: Vec<String> = module(py, "conductor.mutation_coverage")
        .getattr("CANONICAL_TEST_PATTERNS")
        .unwrap()
        .extract()
        .unwrap();
    let payload = json!({
        "schema_version": 1,
        "enforcement": "changed_tests",
        "test_patterns": patterns,
        "receipt_directories": ["conductor/mutation_campaigns/receipts"],
        "campaigns": [{"manifest": "conductor/mutation_campaigns/placeholder.json"}],
    });
    fs::write(&location, payload.to_string()).unwrap();
    location
}

pub fn namespace<'py>(py: Python<'py>, fields: &Bound<'py, PyDict>) -> Bound<'py, PyAny> {
    module(py, "types")
        .getattr("SimpleNamespace")
        .unwrap()
        .call((), Some(fields))
        .unwrap()
}

/// Preserve Python's original tuple/set/dict shapes rather than serializing the fixture.
pub fn scope_campaign<'py>(
    py: Python<'py>,
    root: &Path,
    sources: &[&str],
    engine: &str,
) -> Bound<'py, PyAny> {
    let pins = PyDict::new(py);
    for source in sources {
        let target = root.join(source);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(target, "x = 1\n").unwrap();
        pins.set_item(source, "pin").unwrap();
    }
    let fields = PyDict::new(py);
    fields.set_item("mutation_engine", engine).unwrap();
    fields
        .set_item("source", PyTuple::new(py, sources).unwrap())
        .unwrap();
    fields.set_item("source_sha256", pins).unwrap();
    fields.set_item("test_sha256", PyDict::new(py)).unwrap();
    fields.set_item("options", PyDict::new(py)).unwrap();
    fields.set_item("operators", PyTuple::empty(py)).unwrap();
    namespace(py, &fields)
}

pub fn set_sources<'py>(py: Python<'py>, manifest: &Bound<'py, PyAny>, sources: &[&str]) {
    manifest
        .setattr("source", PyTuple::new(py, sources).unwrap())
        .unwrap();
}

/// The original `changes` fixture accepts arbitrary positional/keyword calls.
pub fn changed_callback<'py>(py: Python<'py>, paths: &[&str]) -> Bound<'py, PyCFunction> {
    let paths = paths
        .iter()
        .map(|path| path.to_string())
        .collect::<Vec<_>>();
    PyCFunction::new_closure(py, None, None, move |args, _kwargs| {
        let builtins = args.py().import("builtins")?;
        builtins
            .getattr("set")?
            .call1((paths.clone(),))
            .map(Bound::unbind)
    })
    .unwrap()
}

pub fn strict_callback<'py, F>(
    py: Python<'py>,
    positional: &[&str],
    keyword_only: &[&str],
    body: F,
) -> Bound<'py, PyCFunction>
where
    F: Fn(Python<'_>, Bound<'_, PyAny>) -> PyResult<Py<PyAny>> + Send + Sync + 'static,
{
    let sig = signature(py, positional, keyword_only);
    PyCFunction::new_closure(py, None, None, move |args, kwargs| {
        let bound = bind_signature(&sig, args, kwargs)?;
        body(args.py(), bound.getattr("arguments")?)
    })
    .unwrap()
}

pub fn constant_callback<'py>(
    py: Python<'py>,
    value: &Bound<'py, PyAny>,
) -> Bound<'py, PyCFunction> {
    let value = value.clone().unbind();
    PyCFunction::new_closure(py, None, None, move |args, _kwargs| {
        Ok::<_, PyErr>(value.clone_ref(args.py()))
    })
    .unwrap()
}

pub fn json_object<'py>(py: Python<'py>, value: Value) -> Bound<'py, PyAny> {
    py_json(py, value)
}
