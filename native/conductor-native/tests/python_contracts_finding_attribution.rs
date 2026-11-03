#![cfg(feature = "python-compat-tests")]
//! Standalone retained model fingerprint contract.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::prelude::*;
use pyo3::types::PyDict;
use support::{module, Case};

#[test]
fn inherited_does_not_change_a_finding_fingerprint() {
    let _case = Case::new();
    Python::attach(|py| {
        let model = module(py, "conductor.candidate_review.model");
        let finding = || {
            let kwargs = PyDict::new(py);
            kwargs.set_item("check_id", "python-ast").unwrap();
            kwargs.set_item("rule_id", "rule").unwrap();
            kwargs
                .set_item(
                    "severity",
                    model.getattr("Severity").unwrap().getattr("HIGH").unwrap(),
                )
                .unwrap();
            kwargs.set_item("message", "m").unwrap();
            kwargs.set_item("path", Some("a.py")).unwrap();
            model
                .getattr("Finding")
                .unwrap()
                .call((), Some(&kwargs))
                .unwrap()
        };
        let caused = finding().call_method0("finalize").unwrap();
        let inherited = finding();
        inherited.setattr("inherited", true).unwrap();
        let finalized = inherited.call_method0("finalize").unwrap();
        assert!(finalized
            .getattr("fingerprint")
            .unwrap()
            .eq(caused.getattr("fingerprint").unwrap())
            .unwrap());
    });
}
