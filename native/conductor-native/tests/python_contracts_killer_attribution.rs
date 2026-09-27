#![cfg(feature = "python-compat-tests")]
//! Rust-owned declared-killer attribution verdict contracts.

#[path = "python_contracts/killer_attribution_support.rs"]
mod killer_support;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use killer_support::{report, supported, verdict, DECLARED, OTHER};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyList};
use support::Case;

fn status(row: &Bound<'_, PyAny>, expected: &str) {
    assert!(row.get_item("status").unwrap().eq(expected).unwrap());
}

fn list(py: Python<'_>, row: &Bound<'_, PyAny>, key: &str, expected: &[&str]) {
    assert!(row
        .get_item(key)
        .unwrap()
        .eq(PyList::new(py, expected).unwrap())
        .unwrap());
}

#[test]
fn a_mutant_killed_by_its_declared_test_is_confirmed() {
    let _case = Case::new();
    Python::attach(|py| {
        let report = report(py, "COMPLETE", Some("FAILED"), Some("PASSED"));
        let row = verdict(py, &[DECLARED], Some(&report), "KILLED");
        status(&row, "CONFIRMED");
        list(py, &row, "matched", &[DECLARED]);
    });
}

#[test]
fn a_mutant_killed_only_by_another_test_is_misattributed() {
    let _case = Case::new();
    Python::attach(|py| {
        let report = report(py, "COMPLETE", Some("PASSED"), Some("FAILED"));
        let row = verdict(py, &[DECLARED], Some(&report), "KILLED");
        status(&row, "MISATTRIBUTED");
        list(py, &row, "matched", &[]);
        list(py, &row, "collateral", &[OTHER]);
    });
}

#[test]
fn an_erroring_declared_test_still_counts_as_the_killer() {
    let _case = Case::new();
    Python::attach(|py| {
        let report = report(py, "COMPLETE", Some("ERROR"), None);
        status(
            &verdict(py, &[DECLARED], Some(&report), "KILLED"),
            "CONFIRMED",
        );
    });
}

#[test]
fn a_declared_killer_absent_from_the_report_is_named_unobservable() {
    let _case = Case::new();
    Python::attach(|py| {
        let report = report(py, "COMPLETE", None, Some("FAILED"));
        let row = verdict(py, &[DECLARED], Some(&report), "KILLED");
        status(&row, "MISATTRIBUTED");
        list(py, &row, "unobservable", &[DECLARED]);
    });
}

#[test]
fn incomplete_attribution_never_confirms_a_contract() {
    let _case = Case::new();
    Python::attach(|py| {
        let report = report(py, "INCOMPLETE", Some("FAILED"), None);
        status(
            &verdict(py, &[DECLARED], Some(&report), "KILLED"),
            "UNATTRIBUTED",
        );
    });
}

#[test]
fn a_batch_without_attribution_is_unavailable_not_confirmed() {
    let _case = Case::new();
    Python::attach(|py| status(&verdict(py, &[DECLARED], None, "KILLED"), "UNAVAILABLE"));
}

#[test]
fn a_survivor_is_adjudicated_by_the_outcome_not_the_killers() {
    let _case = Case::new();
    Python::attach(|py| {
        let report = report(py, "COMPLETE", Some("PASSED"), None);
        status(
            &verdict(py, &[DECLARED], Some(&report), "SURVIVED"),
            "NOT_APPLICABLE",
        );
    });
}

#[test]
fn python_nodeids_without_a_junitxml_flag_are_attributable() {
    let _case = Case::new();
    Python::attach(|py| assert!(supported(py, &["pytest", "-q"], &[DECLARED, OTHER])));
}

#[test]
fn a_preexisting_junitxml_flag_blocks_attribution() {
    let _case = Case::new();
    Python::attach(|py| {
        assert!(!supported(
            py,
            &["pytest", "--junitxml=out.xml"],
            &[DECLARED]
        ))
    });
}

#[test]
fn a_non_python_nodeid_blocks_attribution() {
    let _case = Case::new();
    Python::attach(|py| assert!(!supported(py, &["pytest"], &["crate::tests::case"])));
}

#[test]
fn a_batch_with_no_ranked_tests_is_not_attributable() {
    let _case = Case::new();
    Python::attach(|py| assert!(!supported(py, &["pytest"], &[])));
}
