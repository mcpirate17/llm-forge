#![cfg(feature = "python-compat-tests")]
//! Rust-owned contracts for native duplicate-body policy and independent digests.

#[path = "python_contracts/reuse_ast_support.rs"]
#[allow(dead_code)]
mod ast_ref;
#[path = "python_contracts/duplicate_bodies_support.rs"]
mod body_ref;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use body_ref::{groups, long_source, native, reference, THRESHOLD_SOURCE};
use pyo3::prelude::*;
use serde_json::{json, Value};
use std::collections::HashSet;
use support::{assert_error, Case};

fn names_and_lines(functions: &[Value]) -> Vec<(String, usize, usize)> {
    functions
        .iter()
        .map(|item| {
            (
                item["name"].as_str().unwrap().to_owned(),
                item["lineno"].as_u64().unwrap() as usize,
                item["end_lineno"].as_u64().unwrap() as usize,
            )
        })
        .collect()
}

fn assert_policy_distinctions(py: Python<'_>) {
    let source = long_source();
    let records = [("sample.py", source.as_str())];
    let candidate = native(py, &records, "candidate").unwrap();
    let standalone = native(py, &records, "standalone").unwrap();
    let candidate = candidate[0]["functions"].as_array().unwrap();
    let standalone = standalone[0]["functions"].as_array().unwrap();
    let hashes: HashSet<&str> = candidate
        .iter()
        .map(|item| item["digest"].as_str().unwrap())
        .collect();
    assert_eq!(hashes.len(), 1);
    assert_eq!(standalone[0]["digest"], standalone[2]["digest"]);
    assert_ne!(standalone[0]["digest"], standalone[1]["digest"]);
    assert_ne!(standalone[0]["digest"], standalone[3]["digest"]);
}

fn assert_thresholds(py: Python<'_>) {
    let records = [
        ("nested.py", THRESHOLD_SOURCE),
        ("bad.py", "def broken(:\n"),
    ];
    let candidate = native(py, &records, "candidate").unwrap();
    let standalone = native(py, &records, "standalone").unwrap();
    let names = |payload: &Value| -> Vec<String> {
        payload[0]["functions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["name"].as_str().unwrap().to_owned())
            .collect()
    };
    assert_eq!(names(&candidate), ["outer", "nested"]);
    assert_eq!(names(&standalone), ["outer", "eight_lines", "nested"]);
    assert_eq!(
        candidate[1],
        json!({"path":"bad.py","parse_error":true,"functions":[]})
    );
    assert_eq!(standalone[1], candidate[1]);
}

fn assert_unknown_policy(py: Python<'_>) {
    let error = native(py, &[], "approximate").unwrap_err();
    let class = py
        .import("builtins")
        .unwrap()
        .getattr("ValueError")
        .unwrap();
    assert_error(py, error, &class, "unknown duplicate-body policy");
}

#[test]
fn native_fingerprints_preserve_cpython_equivalence_partitions() {
    let _case = Case::new();
    Python::attach(|py| {
        let source = long_source();
        let records = [("sample.py", source.as_str())];
        for policy in ["candidate", "standalone"] {
            let actual = native(py, &records, policy).unwrap();
            let expected = reference(py, &records, policy);
            assert_eq!(actual[0]["parse_error"], false);
            let actual_functions = actual[0]["functions"].as_array().unwrap();
            let expected_functions = expected[0]["functions"].as_array().unwrap();
            assert_eq!(
                names_and_lines(actual_functions),
                names_and_lines(expected_functions)
            );
            assert_eq!(groups(actual_functions), groups(expected_functions));
        }
        assert_policy_distinctions(py);
        assert_thresholds(py);
        assert_unknown_policy(py);
    });
}

#[test]
fn candidate_strips_docstrings_but_standalone_keeps_signature_and_docstring() {
    let _case = Case::new();
    Python::attach(assert_policy_distinctions);
}

#[test]
fn thresholds_nested_order_and_parse_failures_are_exact() {
    let _case = Case::new();
    Python::attach(assert_thresholds);
}

#[test]
fn unknown_policy_fails_loudly() {
    let _case = Case::new();
    Python::attach(assert_unknown_policy);
}
