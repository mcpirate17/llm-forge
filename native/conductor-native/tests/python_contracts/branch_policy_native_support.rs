//! Fixtures for the Python-to-Rust branch-policy boundary.

use crate::comm_support::{bind_signature, signature};
use crate::support::{module, path, AttrPatch};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyCFunction, PyDict, PyModule};
use serde_json::Value;
use std::fs;
use std::path::Path;

pub fn bp(py: Python<'_>) -> Bound<'_, PyModule> {
    module(py, "conductor.branch_policy")
}

pub fn utc_now(py: Python<'_>) -> Bound<'_, PyAny> {
    let datetime = module(py, "datetime");
    datetime
        .getattr("datetime")
        .unwrap()
        .call_method1("now", (datetime.getattr("UTC").unwrap(),))
        .unwrap()
}

pub fn minus<'py>(
    py: Python<'py>,
    now: &Bound<'py, PyAny>,
    delta: &[(&str, i64)],
) -> Bound<'py, PyAny> {
    let kwargs = PyDict::new(py);
    for (name, value) in delta {
        kwargs.set_item(*name, *value).unwrap();
    }
    let duration = module(py, "datetime")
        .getattr("timedelta")
        .unwrap()
        .call((), Some(&kwargs))
        .unwrap();
    now.call_method1("__sub__", (duration,)).unwrap()
}

pub fn binding<'py>(py: Python<'py>, created: &str, pushed: Option<&str>) -> Bound<'py, PyAny> {
    bp(py)
        .getattr("BranchBinding")
        .unwrap()
        .call1((
            "claude/topic-20260101",
            "c1",
            "claude",
            created,
            pushed,
            py.None(),
        ))
        .unwrap()
}

pub fn store_patch(py: Python<'_>, root: &Path) -> AttrPatch {
    let expected = signature(py, &["repo"], &[]);
    let result = path(py, root).unbind();
    let callback =
        PyCFunction::new_closure(py, None, None, move |args, kwargs| -> PyResult<Py<PyAny>> {
            bind_signature(&expected, args, kwargs)?;
            Ok(result.clone_ref(args.py()))
        })
        .unwrap();
    AttrPatch::replace(bp(py).as_any(), "git_common_dir", callback.as_any())
}

pub fn store(py: Python<'_>, root: &Path, payload: Value) {
    let location = bp(py)
        .getattr("binding_store_path")
        .unwrap()
        .call1((path(py, root),))
        .unwrap();
    let filename: String = location.str().unwrap().extract().unwrap();
    let location = Path::new(&filename);
    fs::create_dir_all(location.parent().unwrap()).unwrap();
    fs::write(location, payload.to_string()).unwrap();
}

pub fn store_path(py: Python<'_>, root: &Path) -> std::path::PathBuf {
    let location = bp(py)
        .getattr("binding_store_path")
        .unwrap()
        .call1((path(py, root),))
        .unwrap();
    let filename: String = location.str().unwrap().extract().unwrap();
    filename.into()
}
