#![cfg(feature = "python-compat-tests")]
//! Rust-owned independent AST-normalization and function-collection contracts.

#[path = "python_contracts/reuse_ast_support.rs"]
#[allow(dead_code)]
mod ast_ref;
#[path = "python_contracts/reuse_collect_support.rs"]
mod collect_ref;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use ast_ref::{is_any, parse, walk};
use collect_ref::{collect_production, payload, reference_collect, reference_normalize};
use pyo3::prelude::*;
use pyo3::types::PyList;
use std::fs;
use support::{module, Case};

const NORMALIZER_SOURCE: &str = r#"def alpha(item: "Input", /, scale=2, *rest, enabled=True, **options) -> "Output":
    """outer documentation is not semantic"""
    import package.module as pm
    local = pm.convert(item, scale)
    try:
        result = global_call(local, literal=3)
    except Exception as problem:
        recovered = problem
        result = recovered
    def inner(value):
        """nested documentation is not semantic"""
        return value + local
    class Box:
        """class documentation is not semantic"""
        pass
    transform = lambda lambda_value: lambda_value + local
    return inner(result), Box, transform, rest, enabled, options

def beta(value, factor=2):
    renamed = value + factor
    return renamed
"#;

const ORDER_SOURCE: &str = "def outer(value):\n    first = value + 1\n    def nested(item):\n        return item * 2\n    second = nested(first)\n    return second\n\nasync def later(value):\n    first = value + 1\n    second = first + 1\n    third = second + 1\n    fourth = third + 1\n    return fourth\n";

const ENCODED: &[u8] = b"def encoded(value):\r\n    text = 'caf\xc3\xa9'\r\n    invalid = '\xff'\r\n    combined = text + invalid\r\n    result = combined + str(value)\r\n    return result\r\n";

#[test]
fn native_normalizer_matches_cpython_reference() {
    let _case = Case::new();
    Python::attach(|py| {
        let tree = parse(py, NORMALIZER_SOURCE, "<unknown>").unwrap();
        let consolidation = module(py, "conductor.reuse.consolidation");
        let mut functions = 0;
        for node in walk(&tree) {
            if !is_any(&node, &["FunctionDef", "AsyncFunctionDef"]) {
                continue;
            }
            functions += 1;
            let expected = reference_normalize(py, &node);
            let actual = consolidation
                .getattr("_normalize_hash")
                .unwrap()
                .call1((&node,))
                .unwrap();
            assert!(actual.eq(expected.into_pyobject(py).unwrap()).unwrap());
        }
        assert_eq!(functions, 3); // alpha, beta, and nested inner
    });
}

#[test]
fn native_collect_preserves_boundary_async_and_breadth_first_order() {
    let case = Case::new();
    let order = case.root().join("order.py");
    fs::write(&order, ORDER_SOURCE).unwrap();
    Python::attach(|py| {
        let expected = reference_collect(py, &[order.as_path()], case.root(), 6);
        let actual = collect_production(py, &[order.as_path()], case.root(), 6);
        assert!(payload(py, &actual).eq(payload(py, &expected)).unwrap());
        let names =
            PyList::new(py, actual.0.iter().map(|row| row.getattr("name").unwrap())).unwrap();
        assert!(names
            .eq(PyList::new(py, ["outer", "later"]).unwrap())
            .unwrap());
    });
}

#[test]
fn native_collect_preserves_text_decoding_and_failure_accounting() {
    let case = Case::new();
    let valid = case.root().join("encoded.py");
    let broken = case.root().join("broken.py");
    let missing = case.root().join("missing.py");
    fs::write(&valid, ENCODED).unwrap();
    fs::write(&broken, "def broken(:\n").unwrap();
    let paths = [valid.as_path(), broken.as_path(), missing.as_path()];
    Python::attach(|py| {
        let expected = reference_collect(py, &paths, case.root(), 6);
        let actual = collect_production(py, &paths, case.root(), 6);
        assert!(payload(py, &actual).eq(payload(py, &expected)).unwrap());
        assert_eq!(actual.1, 2);
        let source: String = actual
            .0
            .get_item(0)
            .unwrap()
            .getattr("source")
            .unwrap()
            .extract()
            .unwrap();
        assert!(source.contains('é'));
        assert!(source.contains('\u{fffd}'));
        assert!(!source.contains('\r'));
    });
}
