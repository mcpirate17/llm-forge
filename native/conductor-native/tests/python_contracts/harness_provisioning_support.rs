//! Rust PyO3 fixtures for portable provider-hook provisioning contracts.

use crate::comm_support::{bind_signature, signature};
use crate::support::{module, path, AttrPatch};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyCFunction, PyDict, PyList, PyModule, PyTuple};
use std::path::{Path, PathBuf};

pub fn hp(py: Python<'_>) -> Bound<'_, PyModule> {
    module(py, "conductor.harness_provisioning")
}

pub fn pi(py: Python<'_>) -> Bound<'_, PyModule> {
    module(py, "conductor.project_init")
}

pub fn provider_path(py: Python<'_>, provider: &str, root: &Path) -> PathBuf {
    let relative: String = module(py, "conductor.hook_installer")
        .getattr("PROVIDERS")
        .unwrap()
        .get_item(provider)
        .unwrap()
        .getattr("relative_path")
        .unwrap()
        .str()
        .unwrap()
        .extract()
        .unwrap();
    root.join(relative)
}

pub fn config<'py>(
    py: Python<'py>,
    root: &Path,
    provider: &str,
    interpreter: Option<&Path>,
) -> Bound<'py, PyAny> {
    let options = PyDict::new(py);
    options.set_item("project_dir", path(py, root)).unwrap();
    options
        .set_item("providers", PyTuple::new(py, [provider]).unwrap())
        .unwrap();
    if let Some(interpreter) = interpreter {
        options.set_item("python", path(py, interpreter)).unwrap();
    }
    pi(py)
        .getattr("InitConfig")
        .unwrap()
        .call((), Some(&options))
        .unwrap()
}

pub fn importable(py: Python<'_>) -> AttrPatch {
    let expected = signature(py, &["python"], &[]);
    let callback =
        PyCFunction::new_closure(py, None, None, move |args, kwargs| -> PyResult<bool> {
            bind_signature(&expected, args, kwargs)?;
            Ok(true)
        })
        .unwrap();
    AttrPatch::replace(pi(py).as_any(), "_crg_importable", callback.as_any())
}

pub fn signature_with_kwargs(py: Python<'_>, name: &str) -> Py<PyAny> {
    let inspect = module(py, "inspect");
    let parameter = inspect.getattr("Parameter").unwrap();
    let ordinary = parameter.getattr("POSITIONAL_OR_KEYWORD").unwrap();
    let rest = parameter.getattr("VAR_KEYWORD").unwrap();
    let params = PyList::empty(py);
    params
        .append(parameter.call1((name, ordinary)).unwrap())
        .unwrap();
    params
        .append(parameter.call1(("kwargs", rest)).unwrap())
        .unwrap();
    inspect
        .getattr("Signature")
        .unwrap()
        .call1((params,))
        .unwrap()
        .unbind()
}
