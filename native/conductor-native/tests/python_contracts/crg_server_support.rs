//! Rust PyO3 fixtures for the CRG server and embedding-bridge contracts.

use crate::comm_support::{bind_signature, signature};
use crate::support::{module, AttrPatch};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyCFunction, PyDict, PyList, PyModule, PyString, PyTuple};

pub struct ModulePatch {
    modules: Py<PyDict>,
    name: String,
    original: Option<Py<PyAny>>,
}

impl ModulePatch {
    pub fn new(py: Python<'_>, name: &str, replacement: &Bound<'_, PyModule>) -> Self {
        let modules = module(py, "sys")
            .getattr("modules")
            .unwrap()
            .cast_into::<PyDict>()
            .unwrap();
        let original = modules.get_item(name).unwrap().map(Bound::unbind);
        modules.set_item(name, replacement).unwrap();
        Self {
            modules: modules.unbind(),
            name: name.to_owned(),
            original,
        }
    }
}

impl Drop for ModulePatch {
    fn drop(&mut self) {
        Python::attach(|py| {
            let modules = self.modules.bind(py);
            if let Some(original) = &self.original {
                modules.set_item(&self.name, original.bind(py)).unwrap();
            } else {
                modules.del_item(&self.name).unwrap();
            }
        });
    }
}

pub fn fake_crg<'py>(
    py: Python<'py>,
) -> (Bound<'py, PyModule>, Bound<'py, PyModule>, Vec<ModulePatch>) {
    let package = PyModule::new(py, "code_review_graph").unwrap();
    let embeddings = PyModule::new(py, "code_review_graph.embeddings").unwrap();
    let main = PyModule::new(py, "code_review_graph.main").unwrap();
    let patches = vec![
        ModulePatch::new(py, "code_review_graph", &package),
        ModulePatch::new(py, "code_review_graph.embeddings", &embeddings),
        ModulePatch::new(py, "code_review_graph.main", &main),
    ];
    (embeddings, main, patches)
}

pub fn fake_main<'py>(py: Python<'py>) -> (Bound<'py, PyModule>, Vec<ModulePatch>) {
    let package = PyModule::new(py, "code_review_graph").unwrap();
    let main = PyModule::new(py, "code_review_graph.main").unwrap();
    let patches = vec![
        ModulePatch::new(py, "code_review_graph", &package),
        ModulePatch::new(py, "code_review_graph.main", &main),
    ];
    (main, patches)
}

pub fn append_record(
    py: Python<'_>,
    calls: &Bound<'_, PyList>,
    name: &str,
    value: &Bound<'_, PyAny>,
) -> PyResult<()> {
    let name = PyString::new(py, name);
    calls.append(PyTuple::new(py, [name.as_any(), value])?)
}

pub fn no_arg(py: Python<'_>) -> Bound<'_, PyCFunction> {
    let expected = signature(py, &[], &[]);
    PyCFunction::new_closure(py, None, None, move |args, kwargs| -> PyResult<()> {
        bind_signature(&expected, args, kwargs)?;
        Ok(())
    })
    .unwrap()
}

pub fn no_service(py: Python<'_>, bridge: &Bound<'_, PyModule>) -> AttrPatch {
    AttrPatch::replace(bridge.as_any(), "ensure_service", no_arg(py).as_any())
}

pub fn version<'py>(py: Python<'py>, value: &str) -> Bound<'py, PyCFunction> {
    let expected = signature(py, &["name"], &[]);
    let value = value.to_owned();
    PyCFunction::new_closure(py, None, None, move |args, kwargs| -> PyResult<String> {
        bind_signature(&expected, args, kwargs)?;
        Ok(value.clone())
    })
    .unwrap()
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

pub fn signature_optional_none(py: Python<'_>, name: &str) -> Py<PyAny> {
    let inspect = module(py, "inspect");
    let parameter = inspect.getattr("Parameter").unwrap();
    let kind = parameter.getattr("POSITIONAL_OR_KEYWORD").unwrap();
    let options = PyDict::new(py);
    options.set_item("default", py.None()).unwrap();
    let params = PyList::empty(py);
    params
        .append(parameter.call((name, kind), Some(&options)).unwrap())
        .unwrap();
    inspect
        .getattr("Signature")
        .unwrap()
        .call1((params,))
        .unwrap()
        .unbind()
}
