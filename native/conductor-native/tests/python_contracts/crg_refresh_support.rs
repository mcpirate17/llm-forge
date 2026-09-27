//! Rust-owned fixtures for the asynchronous graph-refresh contracts.

use crate::support::{module, path, AttrPatch, Case};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyCFunction, PyDict, PyList, PyModule, PyTuple};
use serde_json::Value;
use std::fs::{self, File, OpenOptions};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};

pub fn child_bin() -> &'static str {
    env!("CARGO_BIN_EXE_crg_refresh_child")
}

pub struct RefreshCase {
    pub case: Case,
    pub store_root: PathBuf,
    pub repo: PathBuf,
}

impl RefreshCase {
    pub fn new() -> Self {
        let case = Case::new();
        let store_root = case.mkdir("store");
        let repo = case.mkdir("repo");
        Self {
            case,
            store_root,
            repo,
        }
    }

    pub fn state<'py>(&self, py: Python<'py>) -> Bound<'py, PyModule> {
        module(py, "tooling.hooks.agent.crg_refresh_state")
    }

    pub fn store<'py>(&self, py: Python<'py>) -> Bound<'py, PyAny> {
        self.state(py)
            .getattr("Store")
            .unwrap()
            .call1((path(py, &self.store_root),))
            .unwrap()
    }

    pub fn setup_body(&mut self) {
        fs::create_dir_all(self.repo.join("pkg")).unwrap();
        fs::write(self.repo.join("pkg/mod.py"), "X = 1\n").unwrap();
        fs::write(self.repo.join("notes.md"), "# n\n").unwrap();
        self.case
            .set_env("CRG_DATA_DIR", self.store_root.to_str().unwrap());
    }

    pub fn body<'py>(&self, py: Python<'py>) -> (Bound<'py, PyModule>, Vec<AttrPatch>) {
        let body = module(py, "tooling.hooks.agent.crg_graph_refresh");
        let gate = module(py, "crg_gate");
        let repo = path(py, &self.repo);
        let patches = vec![
            AttrPatch::replace(body.as_any(), "REPO_ROOT", &repo),
            AttrPatch::replace(gate.as_any(), "REPO_ROOT", &repo),
        ];
        (body, patches)
    }

    pub fn pending(&self) -> PathBuf {
        self.store_root.join("refresh.pending")
    }

    pub fn lock(&self) -> PathBuf {
        self.store_root.join("refresh.lock")
    }

    pub fn hold_lock(&self) -> File {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.lock())
            .unwrap();
        let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
        assert_eq!(result, 0);
        file
    }
}

pub fn json_obj<'py>(py: Python<'py>, value: Value) -> Bound<'py, PyAny> {
    py.import("json")
        .unwrap()
        .getattr("loads")
        .unwrap()
        .call1((value.to_string(),))
        .unwrap()
}

pub fn request(
    py: Python<'_>,
    state: &Bound<'_, PyModule>,
    store: &Bound<'_, PyAny>,
    paths: &[&str],
    argv: &[String],
    cwd: &Path,
) -> String {
    let kwargs = PyDict::new(py);
    kwargs.set_item("worker_argv", argv).unwrap();
    kwargs.set_item("cwd", path(py, cwd)).unwrap();
    state
        .getattr("request")
        .unwrap()
        .call((store, paths), Some(&kwargs))
        .unwrap()
        .extract()
        .unwrap()
}

pub fn batch_command<'py>(
    py: Python<'py>,
    mode: &'static str,
    extra: Option<&str>,
) -> Bound<'py, PyCFunction> {
    let binary = child_bin().to_owned();
    let extra = extra.map(str::to_owned);
    let signature = signature(py, &["paths"], false);
    PyCFunction::new_closure(
        py,
        None,
        None,
        move |args, kwargs| -> PyResult<Vec<String>> {
            bind_signature(&signature, args, kwargs)?;
            let mut command = vec![binary.clone(), mode.to_owned()];
            if let Some(value) = &extra {
                command.push(value.clone());
            }
            Ok(command)
        },
    )
    .unwrap()
}

/// Bind Rust callbacks exactly like the Python fixtures they replace.
pub fn signature(py: Python<'_>, positional: &[&str], var_keywords: bool) -> Py<PyAny> {
    let inspect = py.import("inspect").unwrap();
    let parameter = inspect.getattr("Parameter").unwrap();
    let params = PyList::empty(py);
    for name in positional {
        let kind = parameter.getattr("POSITIONAL_OR_KEYWORD").unwrap();
        params
            .append(parameter.call1((*name, kind)).unwrap())
            .unwrap();
    }
    if var_keywords {
        let kind = parameter.getattr("VAR_KEYWORD").unwrap();
        params
            .append(parameter.call1(("kw", kind)).unwrap())
            .unwrap();
    }
    inspect
        .getattr("Signature")
        .unwrap()
        .call1((params,))
        .unwrap()
        .unbind()
}

pub fn bind_signature<'py>(
    signature: &Py<PyAny>,
    args: &Bound<'py, PyTuple>,
    kwargs: Option<&Bound<'py, PyDict>>,
) -> PyResult<Bound<'py, PyDict>> {
    Ok(signature
        .bind(args.py())
        .call_method("bind", args, kwargs)?
        .getattr("arguments")?
        .cast_into::<PyDict>()?)
}
