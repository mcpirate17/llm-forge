//! Rust-owned fixtures for `conductor init` contracts.

use crate::comm_support::{bind_signature, signature};
use crate::support::{module, path, text, AttrPatch, Case};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyCFunction, PyDict, PyList, PyModule};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

pub fn init(py: Python<'_>) -> Bound<'_, PyModule> {
    module(py, "conductor.project_init")
}

pub fn repo(case: &Case) -> PathBuf {
    let root = case.root().join("proj");
    fs::create_dir_all(root.join(".git")).unwrap();
    root
}

pub fn executable(project: &Path, relative: &str) -> PathBuf {
    let file = project.join(relative);
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, "#!/bin/sh\n").unwrap();
    let mut mode = fs::metadata(&file).unwrap().permissions();
    mode.set_mode(mode.mode() | 0o100);
    fs::set_permissions(&file, mode).unwrap();
    file
}

pub fn python_executable(py: Python<'_>) -> PathBuf {
    PathBuf::from(
        module(py, "sys")
            .getattr("executable")
            .unwrap()
            .extract::<String>()
            .unwrap(),
    )
}

pub fn config<'py>(
    py: Python<'py>,
    project: &Path,
    python: Option<&Path>,
    force: bool,
    dry_run: bool,
    check: bool,
) -> Bound<'py, PyAny> {
    let kwargs = PyDict::new(py);
    let default_python = python_executable(py);
    kwargs.set_item("project_dir", path(py, project)).unwrap();
    kwargs
        .set_item("python", path(py, python.unwrap_or(&default_python)))
        .unwrap();
    kwargs.set_item("force", force).unwrap();
    kwargs.set_item("dry_run", dry_run).unwrap();
    kwargs.set_item("check", check).unwrap();
    init(py)
        .getattr("InitConfig")
        .unwrap()
        .call((), Some(&kwargs))
        .unwrap()
}

pub fn today<'py>(py: Python<'py>) -> Bound<'py, PyAny> {
    module(py, "datetime")
        .getattr("date")
        .unwrap()
        .call1((2026, 1, 1))
        .unwrap()
}

pub fn plan<'py>(
    py: Python<'py>,
    config: &Bound<'py, PyAny>,
    fixed_today: bool,
) -> Bound<'py, PyAny> {
    let kwargs = PyDict::new(py);
    if fixed_today {
        kwargs.set_item("today", today(py)).unwrap();
    }
    init(py)
        .getattr("plan")
        .unwrap()
        .call((config,), Some(&kwargs))
        .unwrap()
}

fn constant_callback<'py>(
    py: Python<'py>,
    parameter: &str,
    result: i64,
) -> Bound<'py, PyCFunction> {
    let expected = signature(py, &[parameter], &[]);
    PyCFunction::new_closure(py, None, None, move |args, kwargs| -> PyResult<i64> {
        bind_signature(&expected, args, kwargs)?;
        Ok(result)
    })
    .unwrap()
}

fn boolean_callback<'py>(py: Python<'py>, result: bool) -> Bound<'py, PyCFunction> {
    let expected = signature(py, &["python"], &[]);
    PyCFunction::new_closure(py, None, None, move |args, kwargs| -> PyResult<bool> {
        bind_signature(&expected, args, kwargs)?;
        Ok(result)
    })
    .unwrap()
}

pub fn doctor(py: Python<'_>, status: i64, crg_importable: bool) -> [AttrPatch; 2] {
    let pi = init(py);
    [
        AttrPatch::replace(
            pi.as_any(),
            "run_doctor",
            constant_callback(py, "config", status).as_any(),
        ),
        AttrPatch::replace(
            pi.as_any(),
            "_crg_importable",
            boolean_callback(py, crg_importable).as_any(),
        ),
    ]
}

pub fn which(py: Python<'_>, found: Option<&str>) -> AttrPatch {
    let shutil = init(py).getattr("shutil").unwrap();
    let expected = signature(py, &["name"], &[]);
    let result = found.map(str::to_owned);
    let callback = PyCFunction::new_closure(
        py,
        None,
        None,
        move |args, kwargs| -> PyResult<Option<String>> {
            bind_signature(&expected, args, kwargs)?;
            Ok(result.clone())
        },
    )
    .unwrap();
    AttrPatch::replace(&shutil, "which", callback.as_any())
}

pub fn settings_hooks<'py>(py: Python<'py>) -> Bound<'py, PyAny> {
    module(py, "tooling.hooks.dispatch.registry")
        .getattr("settings_block")
        .unwrap()
        .call0()
        .unwrap()
        .get_item("hooks")
        .unwrap()
}

pub fn events(py: Python<'_>) -> Vec<String> {
    let registry = module(py, "tooling.hooks.dispatch.registry");
    let values = registry.getattr("EVENTS").unwrap();
    values
        .try_iter()
        .unwrap()
        .map(|value| text(&value.unwrap()))
        .collect()
}

pub fn settings_action<'py>(py: Python<'py>, plan: &Bound<'py, PyAny>) -> Bound<'py, PyAny> {
    let settings = init(py).getattr("SETTINGS").unwrap();
    for action in plan.getattr("actions").unwrap().try_iter().unwrap() {
        let action = action.unwrap();
        if action.getattr("path").unwrap().eq(&settings).unwrap() {
            return action;
        }
    }
    panic!("settings action absent");
}

pub fn json_loads<'py>(py: Python<'py>, text: &Bound<'py, PyAny>) -> Bound<'py, PyAny> {
    module(py, "json")
        .getattr("loads")
        .unwrap()
        .call1((text,))
        .unwrap()
}

pub fn json_dumps(value: &Bound<'_, PyAny>) -> String {
    module(value.py(), "json")
        .getattr("dumps")
        .unwrap()
        .call1((value,))
        .unwrap()
        .extract()
        .unwrap()
}

pub fn args<'py>(py: Python<'py>, values: &[&str]) -> Bound<'py, PyList> {
    PyList::new(py, values).unwrap()
}

pub fn event_command<'py>(hooks: &Bound<'py, PyAny>, event: &str) -> Bound<'py, PyAny> {
    hooks
        .get_item(event)
        .unwrap()
        .get_item(0)
        .unwrap()
        .get_item("hooks")
        .unwrap()
        .get_item(0)
        .unwrap()
        .get_item("command")
        .unwrap()
}
