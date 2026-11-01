#![cfg(feature = "python-compat-tests")]
//! Independent contracts for the shipped local-clerk policy.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::prelude::*;
use support::{assert_error, module, Case};

#[test]
fn local_ai_policy_fail_closed_edge_cases() {
    let _case = Case::new();
    Python::attach(|py| {
        let policy = module(py, "conductor.local_ai_policy");
        let error_class = policy.getattr("LocalAIPolicyError").unwrap();
        assert_error(
            py,
            policy
                .getattr("require_clerical_task")
                .unwrap()
                .call1(("approval", "summarize notes"))
                .unwrap_err(),
            &error_class,
            "not clerical",
        );
        let unclassified = policy.getattr("UNCLASSIFIED_REASON").unwrap();
        let reason = policy
            .getattr("deny_local_ai_command")
            .unwrap()
            .call1(("ollama run \"unterminated",))
            .unwrap();
        assert!(reason.eq(unclassified).unwrap());
    });
}
