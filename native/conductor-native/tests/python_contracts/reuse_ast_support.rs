//! Python standard-library AST primitives for Rust-owned reuse references.

use crate::support::module;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyList, PyModule};

pub fn ast(py: Python<'_>) -> Bound<'_, PyModule> {
    module(py, "ast")
}

pub fn parse<'py>(py: Python<'py>, source: &str, filename: &str) -> PyResult<Bound<'py, PyAny>> {
    let options = PyDict::new(py);
    options.set_item("filename", filename)?;
    ast(py).getattr("parse")?.call((source,), Some(&options))
}

pub fn walk<'py>(node: &Bound<'py, PyAny>) -> Vec<Bound<'py, PyAny>> {
    ast(node.py())
        .getattr("walk")
        .unwrap()
        .call1((node,))
        .unwrap()
        .try_iter()
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

pub fn children<'py>(node: &Bound<'py, PyAny>) -> Vec<Bound<'py, PyAny>> {
    ast(node.py())
        .getattr("iter_child_nodes")
        .unwrap()
        .call1((node,))
        .unwrap()
        .try_iter()
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

pub fn kind(node: &Bound<'_, PyAny>) -> String {
    node.get_type().name().unwrap().to_string()
}

pub fn is_kind(node: &Bound<'_, PyAny>, expected: &str) -> bool {
    node.is_instance(&ast(node.py()).getattr(expected).unwrap())
        .unwrap()
}

pub fn is_ast(node: &Bound<'_, PyAny>) -> bool {
    node.is_instance(&ast(node.py()).getattr("AST").unwrap())
        .unwrap()
}

pub fn is_any(node: &Bound<'_, PyAny>, names: &[&str]) -> bool {
    names.iter().any(|name| is_kind(node, name))
}

pub fn attr_str(node: &Bound<'_, PyAny>, name: &str) -> String {
    node.getattr(name).unwrap().extract().unwrap()
}

pub fn attr_usize(node: &Bound<'_, PyAny>, name: &str) -> usize {
    node.getattr(name).unwrap().extract().unwrap()
}

pub fn dump(node: &Bound<'_, PyAny>) -> String {
    let options = PyDict::new(node.py());
    options.set_item("annotate_fields", false).unwrap();
    ast(node.py())
        .getattr("dump")
        .unwrap()
        .call((node,), Some(&options))
        .unwrap()
        .extract()
        .unwrap()
}

pub fn hash_hex(py: Python<'_>, algorithm: &str, bytes: &[u8]) -> String {
    let kwargs = PyDict::new(py);
    kwargs.set_item("usedforsecurity", false).unwrap();
    module(py, "hashlib")
        .getattr(algorithm)
        .unwrap()
        .call((pyo3::types::PyBytes::new(py, bytes),), Some(&kwargs))
        .unwrap()
        .call_method0("hexdigest")
        .unwrap()
        .extract()
        .unwrap()
}

pub fn list_items<'py>(value: &Bound<'py, PyAny>) -> Vec<Bound<'py, PyAny>> {
    value.cast::<PyList>().unwrap().iter().collect()
}
