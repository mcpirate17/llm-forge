//! Independent Rust-owned duplicate-body digest reference over stdlib AST.

use crate::ast_ref::{self, ast, attr_str, attr_usize, is_any, is_kind, walk};
use crate::support::module;
use pyo3::exceptions::{PyRecursionError, PySyntaxError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyList};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

fn digest(node: &Bound<'_, PyAny>) -> String {
    let options = PyDict::new(node.py());
    options.set_item("include_attributes", false).unwrap();
    let dump: String = ast(node.py())
        .getattr("dump")
        .unwrap()
        .call((node,), Some(&options))
        .unwrap()
        .extract()
        .unwrap();
    format!("{:x}", Sha256::digest(dump.as_bytes()))
}

fn span(node: &Bound<'_, PyAny>) -> (usize, usize) {
    let start = attr_usize(node, "lineno");
    let end = node.getattr("end_lineno").unwrap();
    (
        start,
        if end.is_none() {
            start
        } else {
            end.extract().unwrap()
        },
    )
}

fn candidate_digest(node: &Bound<'_, PyAny>) -> Option<String> {
    let body = node.getattr("body").unwrap().cast_into::<PyList>().unwrap();
    let mut skip = 0;
    if !body.is_empty() {
        let first = body.get_item(0).unwrap();
        if is_kind(&first, "Expr") {
            let value = first.getattr("value").unwrap();
            if is_kind(&value, "Constant")
                && value.getattr("value").unwrap().extract::<String>().is_ok()
            {
                skip = 1;
            }
        }
    }
    let (start, end) = span(node);
    if end - start + 1 < 10 || body.len() <= skip {
        return None;
    }
    let selected = PyList::new(node.py(), body.iter().skip(skip)).unwrap();
    let options = PyDict::new(node.py());
    options.set_item("body", selected).unwrap();
    options
        .set_item("type_ignores", PyList::empty(node.py()))
        .unwrap();
    let wrapper = ast(node.py())
        .getattr("Module")
        .unwrap()
        .call((), Some(&options))
        .unwrap();
    Some(digest(&wrapper))
}

fn standalone_digest(node: &Bound<'_, PyAny>) -> Option<String> {
    let (start, end) = span(node);
    if end - start + 1 < 8 {
        return None;
    }
    let options = PyDict::new(node.py());
    options.set_item("name", "_").unwrap();
    options
        .set_item("args", node.getattr("args").unwrap())
        .unwrap();
    options
        .set_item("body", node.getattr("body").unwrap())
        .unwrap();
    options
        .set_item("decorator_list", PyList::empty(node.py()))
        .unwrap();
    options
        .set_item("returns", node.getattr("returns").unwrap())
        .unwrap();
    options
        .set_item("type_comment", node.getattr("type_comment").unwrap())
        .unwrap();
    let clone = ast(node.py())
        .getattr("FunctionDef")
        .unwrap()
        .call((), Some(&options))
        .unwrap();
    ast(node.py())
        .getattr("fix_missing_locations")
        .unwrap()
        .call1((&clone,))
        .unwrap();
    Some(digest(&clone))
}

pub fn reference(py: Python<'_>, records: &[(&str, &str)], policy: &str) -> Value {
    let mut files = Vec::new();
    for &(path, source) in records {
        let tree = match ast_ref::parse(py, source, path) {
            Ok(tree) => tree,
            Err(error)
                if error.is_instance_of::<PySyntaxError>(py)
                    || error.is_instance_of::<PyValueError>(py)
                    || error.is_instance_of::<PyRecursionError>(py) =>
            {
                files.push(json!({"path":path,"parse_error":true,"functions":[]}));
                continue;
            }
            Err(error) => panic!("unexpected parse error: {error}"),
        };
        let mut functions = Vec::new();
        for node in walk(&tree) {
            if !is_any(&node, &["FunctionDef", "AsyncFunctionDef"]) {
                continue;
            }
            let hash = if policy == "candidate" {
                candidate_digest(&node)
            } else {
                standalone_digest(&node)
            };
            if let Some(hash) = hash {
                let (start, end) = span(&node);
                functions.push(json!({"name":attr_str(&node,"name"),"lineno":start,
                    "end_lineno":end,"digest":hash}));
            }
        }
        files.push(json!({"path":path,"parse_error":false,"functions":functions}));
    }
    Value::Array(files)
}

pub fn native(py: Python<'_>, records: &[(&str, &str)], policy: &str) -> PyResult<Value> {
    let py_records = PyList::new(py, records.iter().map(|&(path, source)| (path, source)))?;
    let payload: String = module(py, "conductor._native")
        .getattr("duplicate_body_fingerprints_native")?
        .call1((py_records, policy))?
        .extract()?;
    Ok(serde_json::from_str(&payload).unwrap())
}

pub fn groups(functions: &[Value]) -> Vec<Vec<(String, usize, usize)>> {
    let mut by_digest: BTreeMap<String, Vec<(String, usize, usize)>> = BTreeMap::new();
    for function in functions {
        let hash = function["digest"].as_str().unwrap().to_owned();
        let value = (
            function["name"].as_str().unwrap().to_owned(),
            function["lineno"].as_u64().unwrap() as usize,
            function["end_lineno"].as_u64().unwrap() as usize,
        );
        by_digest.entry(hash).or_default().push(value);
    }
    let mut groups: Vec<_> = by_digest
        .into_values()
        .map(|mut group| {
            group.sort();
            group
        })
        .collect();
    groups.sort();
    groups
}

pub fn long_function(name: &str, argument: &str, docstring: &str, asynchronous: bool) -> String {
    let prefix = if asynchronous { "async " } else { "" };
    format!("{prefix}def {name}({argument}) -> int:\n    \"{docstring}\"\n    total = 1\n    total += 2\n    total += 3\n    total += 4\n    total += 5\n    total += 6\n    total += 7\n    return total\n")
}

pub fn long_source() -> String {
    [
        long_function("alpha", "left", "first", false),
        long_function("beta", "right, extra=1", "second", false),
        long_function("gamma", "left", "first", true),
        long_function("delta", "left", "second", false),
    ]
    .join("\n")
}

pub const THRESHOLD_SOURCE: &str = "def outer():\n    value = 1\n    def nested():\n        value = 1\n        value += 2\n        value += 3\n        value += 4\n        value += 5\n        value += 6\n        value += 7\n        value += 8\n        return value\n    value += 2\n    value += 3\n    value += 4\n    value += 5\n    value += 6\n    return value\n\ndef eight_lines():\n    value = 1\n    value += 2\n    value += 3\n    value += 4\n    value += 5\n    value += 6\n    return value\n";
