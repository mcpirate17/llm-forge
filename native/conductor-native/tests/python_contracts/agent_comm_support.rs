//! Small PyO3 fixtures shared only by the agent-communication migration targets.

use crate::support::AttrPatch;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyList, PyModule, PyTuple};
use serde_json::Value;

pub fn py_json<'py>(py: Python<'py>, value: Value) -> Bound<'py, PyAny> {
    PyModule::import(py, "json")
        .unwrap()
        .getattr("loads")
        .unwrap()
        .call1((value.to_string(),))
        .unwrap()
}

pub fn json_value(value: &Bound<'_, PyAny>) -> Value {
    let encoded: String = PyModule::import(value.py(), "json")
        .unwrap()
        .getattr("dumps")
        .unwrap()
        .call1((value,))
        .unwrap()
        .extract()
        .unwrap();
    serde_json::from_str(&encoded).unwrap()
}

pub fn capture<'py>(py: Python<'py>, name: &str) -> (Bound<'py, PyAny>, AttrPatch) {
    let buffer = PyModule::import(py, "io")
        .unwrap()
        .getattr("StringIO")
        .unwrap()
        .call0()
        .unwrap();
    let sys = PyModule::import(py, "sys").unwrap();
    let patch = AttrPatch::replace(sys.as_any(), name, &buffer);
    (buffer, patch)
}

pub fn buffer_text(buffer: &Bound<'_, PyAny>) -> String {
    buffer.call_method0("getvalue").unwrap().extract().unwrap()
}

pub fn clear_buffer(buffer: &Bound<'_, PyAny>) {
    buffer.call_method1("truncate", (0,)).unwrap();
    buffer.call_method1("seek", (0,)).unwrap();
}

/// Reproduce the call binding of a fixed-signature Python fixture. The
/// returned `BoundArguments` also exposes values passed by either position or
/// name for Rust assertions.
pub fn signature(py: Python<'_>, positional: &[&str], keyword_only: &[&str]) -> Py<PyAny> {
    let inspect = py.import("inspect").unwrap();
    let parameter = inspect.getattr("Parameter").unwrap();
    let parameters = PyList::empty(py);
    for name in positional {
        let kind = parameter.getattr("POSITIONAL_OR_KEYWORD").unwrap();
        parameters
            .append(parameter.call1((*name, kind)).unwrap())
            .unwrap();
    }
    for name in keyword_only {
        let kind = parameter.getattr("KEYWORD_ONLY").unwrap();
        parameters
            .append(parameter.call1((*name, kind)).unwrap())
            .unwrap();
    }
    inspect
        .getattr("Signature")
        .unwrap()
        .call1((parameters,))
        .unwrap()
        .unbind()
}

pub fn bind_signature<'py>(
    signature: &Py<PyAny>,
    args: &Bound<'py, PyTuple>,
    kw: Option<&Bound<'py, PyDict>>,
) -> PyResult<Bound<'py, PyAny>> {
    signature.bind(args.py()).call_method("bind", args, kw)
}
