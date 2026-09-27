//! Rust fixtures for the code-review-graph response compaction contract.

use crate::comm_support::{json_value, py_json};
use crate::support::{module, path};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyModule, PyString};
use serde_json::{json, Value};
use std::path::Path;

pub const REPO: &str = "/repo/root";
pub const ABS: &str = "/repo/root/pkg/mod.py";

pub fn shim(py: Python<'_>) -> Bound<'_, PyModule> {
    module(py, "conductor.crg_response_shim")
}

pub fn node() -> Value {
    json!({
        "id": 42,
        "kind": "Function",
        "name": "compact_state",
        "qualified_name": format!("{ABS}::compact_state"),
        "file_path": ABS,
        "line_start": 77,
        "line_end": 104,
        "language": "python",
        "parent_name": null,
        "is_test": false,
    })
}

pub fn compact(
    py: Python<'_>,
    payload: Value,
    hints: Option<bool>,
    max_items: Option<usize>,
) -> Value {
    let options = PyDict::new(py);
    if let Some(hints) = hints {
        options.set_item("keep_hints", hints).unwrap();
    }
    if let Some(max_items) = max_items {
        options.set_item("max_items", max_items).unwrap();
    }
    let output = shim(py)
        .getattr("compact_payload")
        .unwrap()
        .call(
            (py_json(py, payload), path(py, Path::new(REPO))),
            Some(&options),
        )
        .unwrap();
    exact_json_value(&output)
}

/// Keep Python list/dict shape observable when Rust compares JSON fixtures.
pub fn exact_json_value(output: &Bound<'_, PyAny>) -> Value {
    let value = json_value(output);
    let expected = py_json(output.py(), value.clone());
    assert!(
        output.eq(&expected).unwrap(),
        "Python value differs from its JSON container shape: {}",
        output.repr().unwrap()
    );
    value
}

pub fn namespace<'py>(py: Python<'py>, values: &[(&str, &Bound<'py, PyAny>)]) -> Bound<'py, PyAny> {
    let kwargs = PyDict::new(py);
    for (key, value) in values {
        kwargs.set_item(*key, *value).unwrap();
    }
    module(py, "types")
        .getattr("SimpleNamespace")
        .unwrap()
        .call((), Some(&kwargs))
        .unwrap()
}

pub fn fake_mcp<'py>(
    py: Python<'py>,
    tools: &[(&str, Bound<'py, PyAny>)],
) -> (Bound<'py, PyAny>, Bound<'py, PyAny>) {
    let components = PyDict::new(py);
    for (name, tool) in tools {
        tool.setattr("name", *name).unwrap();
        components.set_item(format!("tool:{name}@"), tool).unwrap();
    }
    let probe_name = PyString::new(py, "probe");
    let probe = namespace(py, &[("name", probe_name.as_any())]);
    components.set_item("resource:probe@", probe).unwrap();
    let provider = namespace(py, &[("_components", components.as_any())]);
    let mcp = namespace(py, &[("_local_provider", &provider)]);
    (mcp, provider)
}
