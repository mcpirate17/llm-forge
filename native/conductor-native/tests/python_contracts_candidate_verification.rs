#![cfg(feature = "python-compat-tests")]
//! Retained native adapter malformed-request contract; Python review assertions removed.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::prelude::*;
use support::{module, Case};

#[test]
fn malformed_native_request_fails_closed() {
    let _case = Case::new();
    Python::attach(|py| {
        let native = module(py, "conductor._native");
        let decide = native.getattr("candidate_verification_native").unwrap();
        let invalid_json = decide.call1(("selection_plan", "{")).unwrap_err();
        assert!(invalid_json
            .to_string()
            .contains("invalid verification request"));
        let unknown_operation = decide.call1(("unknown", "{}"));
        assert!(unknown_operation
            .unwrap_err()
            .to_string()
            .contains("unknown candidate verification operation"));
    });
}
