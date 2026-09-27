//! Isolated Git and callback fixtures for active-state and session-close contracts.

use crate::support::{self, Case};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyCFunction, PyDict, PyList, PyModule};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct SessionRepo {
    _case: Case,
    root: PathBuf,
}

impl SessionRepo {
    pub fn new() -> Self {
        let case = Case::new();
        let root = case.mkdir("session_ws");
        run_git(&root, &["init", "--quiet"]);
        run_git(&root, &["config", "user.name", "session-tester"]);
        run_git(&root, &["config", "user.email", "session@test.org"]);
        fs::create_dir_all(root.join(".git/governance")).unwrap();
        fs::create_dir_all(root.join("conductor")).unwrap();
        fs::write(root.join(".current_work.md"), "# Active Coordination\n\n").unwrap();
        Self { _case: case, root }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}

pub fn run_git(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .expect("start fixture Git command");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

pub fn create_claim<'py>(
    py: Python<'py>,
    ownership: &Bound<'py, PyModule>,
    repo: &Path,
    owner: &str,
    claim_path: &str,
    justification: &str,
) -> Bound<'py, PyAny> {
    let kwargs = PyDict::new(py);
    kwargs.set_item("owner", owner).unwrap();
    kwargs
        .set_item("paths", PyList::new(py, [claim_path]).unwrap())
        .unwrap();
    kwargs.set_item("justification", justification).unwrap();
    kwargs.set_item("max_minutes", 60.0).unwrap();
    ownership
        .getattr("create_claim")
        .unwrap()
        .call((support::path(py, repo),), Some(&kwargs))
        .unwrap()
}

pub fn python_signature<'py>(
    py: Python<'py>,
    positional: &[&str],
    keyword_only: &[&str],
) -> Bound<'py, PyAny> {
    let inspect = PyModule::import(py, "inspect").unwrap();
    let parameter = inspect.getattr("Parameter").unwrap();
    let params = PyList::empty(py);
    for name in positional {
        let kind = parameter.getattr("POSITIONAL_OR_KEYWORD").unwrap();
        params
            .append(parameter.call1((*name, kind)).unwrap())
            .unwrap();
    }
    for name in keyword_only {
        let kind = parameter.getattr("KEYWORD_ONLY").unwrap();
        params
            .append(parameter.call1((*name, kind)).unwrap())
            .unwrap();
    }
    inspect
        .getattr("Signature")
        .unwrap()
        .call1((params,))
        .unwrap()
}

pub fn strict_callback<'py, F>(
    py: Python<'py>,
    positional: &[&str],
    keyword_only: &[&str],
    body: F,
) -> Bound<'py, PyAny>
where
    F: for<'a> Fn(&Bound<'a, PyDict>) -> PyResult<Py<PyAny>> + Send + Sync + 'static,
{
    let signature = python_signature(py, positional, keyword_only);
    let callback_signature = signature.clone().unbind();
    let callback = PyCFunction::new_closure(py, None, None, move |args, kwargs| {
        let bound = callback_signature
            .bind(args.py())
            .call_method("bind", args, kwargs)?;
        let arguments = bound.getattr("arguments")?.cast_into::<PyDict>()?;
        body(&arguments)
    })
    .unwrap();
    let mock = PyModule::import(py, "unittest.mock").unwrap();
    let options = PyDict::new(py);
    options.set_item("wraps", callback).unwrap();
    let wrapper = mock
        .getattr("Mock")
        .unwrap()
        .call((), Some(&options))
        .unwrap();
    wrapper.setattr("__signature__", signature).unwrap();
    wrapper
}
