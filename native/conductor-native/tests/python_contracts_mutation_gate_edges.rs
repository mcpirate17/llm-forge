#![cfg(feature = "python-compat-tests")]
//! Fail-closed value-gate evidence envelopes at the candidate review seam.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;
#[path = "python_contracts/mutation_value_support.rs"]
#[allow(dead_code)]
mod value_support;

use pyo3::prelude::*;
use serde_json::{json, Value};
use support::{module, path, text, Case};
use value_support::{kwargs, to_python};

fn findings<'py>(
    py: Python<'py>,
    root: &std::path::Path,
    payload: Value,
) -> Bound<'py, pyo3::types::PyAny> {
    let ctx = module(py, "types")
        .getattr("SimpleNamespace")
        .unwrap()
        .call((), Some(&kwargs(py, &[])))
        .unwrap();
    ctx.setattr("snapshot", path(py, root)).unwrap();
    let gated = json!({"conductor/probe_a.py":["conductor/probe_a.py::test_x"]});
    module(py, "conductor.candidate_review.verification")
        .getattr("_new_test_value_findings")
        .unwrap()
        .call1((ctx, to_python(py, &payload), to_python(py, &gated)))
        .unwrap()
}

#[test]
fn non_list_evidence_envelope_blocks_new_test_value() {
    let case = Case::new();
    Python::attach(|py| {
        let rows = findings(py, case.root(), json!({"evidence":"envelope-not-a-list"}));
        assert_eq!(rows.len().unwrap(), 1);
        let row = rows.get_item(0).unwrap();
        assert_eq!(
            text(&row.getattr("rule_id").unwrap()),
            "test-value-receipt-unavailable"
        );
        assert_eq!(text(&row.getattr("path").unwrap()), "conductor/probe_a.py");
        assert!(text(&row.getattr("message").unwrap()).contains("::test_x"));
        let severity = module(py, "conductor.candidate_review.model")
            .getattr("Severity")
            .unwrap()
            .getattr("CRITICAL")
            .unwrap();
        assert!(row.getattr("severity").unwrap().eq(severity).unwrap());
    });
}

#[test]
fn non_string_receipt_field_blocks_new_test_value() {
    let case = Case::new();
    Python::attach(|py| {
        let rows = findings(
            py,
            case.root(),
            json!({"evidence":[{
                "path":"conductor/probe_a.py","receipt":null
            }]}),
        );
        assert_eq!(rows.len().unwrap(), 1);
        let row = rows.get_item(0).unwrap();
        assert_eq!(
            text(&row.getattr("rule_id").unwrap()),
            "test-value-receipt-unavailable"
        );
        assert_eq!(text(&row.getattr("path").unwrap()), "conductor/probe_a.py");
        assert!(text(&row.getattr("message").unwrap()).contains("::test_x"));
        let severity = module(py, "conductor.candidate_review.model")
            .getattr("Severity")
            .unwrap()
            .getattr("CRITICAL")
            .unwrap();
        assert!(row.getattr("severity").unwrap().eq(severity).unwrap());
    });
}
