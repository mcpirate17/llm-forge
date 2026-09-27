#![cfg(feature = "python-compat-tests")]
//! Rust-owned per-finding attribution and policy-boundary contracts.

#[path = "python_contracts/finding_attribution_support.rs"]
mod attribution_support;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use attribution_support::{attribution, changed, context, equal_set, finding, mark};
use pyo3::prelude::*;
use pyo3::types::PyBool;
use support::{module, Case};

fn inherited(py: Python<'_>, finding: &Bound<'_, pyo3::types::PyAny>, expected: bool) {
    assert!(finding
        .getattr("inherited")
        .unwrap()
        .is(PyBool::new(py, expected)));
}

#[test]
fn changed_paths_include_both_sides_of_a_rename() {
    let _case = Case::new();
    Python::attach(|py| {
        let ctx = context(py, &[(Some("new.py"), Some("old.py"))], &[]);
        assert!(equal_set(py, &changed(py, &ctx), &["new.py", "old.py"]));
    });
}

#[test]
fn changed_paths_skip_missing_sides() {
    let _case = Case::new();
    Python::attach(|py| {
        let ctx = context(py, &[(Some("added.py"), None)], &[]);
        assert!(equal_set(py, &changed(py, &ctx), &["added.py"]));
    });
}

#[test]
fn changed_paths_of_an_empty_candidate_is_empty() {
    let _case = Case::new();
    Python::attach(|py| assert!(equal_set(py, &changed(py, &context(py, &[], &[])), &[])));
}

fn check_one(
    changes: &[(Option<&str>, Option<&str>)],
    checks: &[(&str, &str)],
    check_id: &str,
    path: Option<&str>,
    expected: bool,
) {
    let _case = Case::new();
    Python::attach(|py| {
        let ctx = context(py, changes, checks);
        let row = finding(py, check_id, path);
        mark(py, &ctx, &[&row]);
        inherited(py, &row, expected);
    });
}

#[test]
fn diff_check_finding_on_a_changed_file_is_not_inherited() {
    check_one(
        &[(Some("a.py"), None)],
        &[("python-ast", "diff")],
        "python-ast",
        Some("a.py"),
        false,
    );
}

#[test]
fn diff_check_finding_on_an_unchanged_file_is_inherited() {
    check_one(
        &[(Some("a.py"), None)],
        &[("python-ast", "diff")],
        "python-ast",
        Some("somewhere/else.py"),
        true,
    );
}

#[test]
fn diff_check_pathless_finding_is_inherited() {
    check_one(
        &[(Some("a.py"), None)],
        &[("python-ast", "diff")],
        "python-ast",
        None,
        true,
    );
}

#[test]
fn diff_check_attributes_a_renamed_file_to_the_candidate() {
    check_one(
        &[(Some("new.py"), Some("old.py"))],
        &[("python-ast", "diff")],
        "python-ast",
        Some("old.py"),
        false,
    );
}

#[test]
fn candidate_check_never_marks_inherited_even_off_diff() {
    check_one(
        &[(Some("a.py"), None)],
        &[("secret-scan", "candidate")],
        "secret-scan",
        Some("somewhere/else.py"),
        false,
    );
}

#[test]
fn candidate_check_pathless_finding_still_blocks() {
    check_one(
        &[(Some("a.py"), None)],
        &[("mutation-evidence", "candidate")],
        "mutation-evidence",
        None,
        false,
    );
}

#[test]
fn a_check_absent_from_policy_defaults_to_candidate() {
    check_one(
        &[(Some("a.py"), None)],
        &[],
        "not-in-policy",
        Some("elsewhere.py"),
        false,
    );
}

#[test]
fn two_checks_are_attributed_independently() {
    let _case = Case::new();
    Python::attach(|py| {
        let ctx = context(
            py,
            &[(Some("a.py"), None)],
            &[("python-ast", "diff"), ("secret-scan", "candidate")],
        );
        let diff = finding(py, "python-ast", Some("elsewhere.py"));
        let candidate = finding(py, "secret-scan", Some("elsewhere.py"));
        mark(py, &ctx, &[&diff, &candidate]);
        inherited(py, &diff, true);
        inherited(py, &candidate, false);
    });
}

#[test]
fn inherited_does_not_change_a_finding_fingerprint() {
    let _case = Case::new();
    Python::attach(|py| {
        let caused = finding(py, "python-ast", Some("a.py"));
        let caused = caused.call_method0("finalize").unwrap();
        let inherited_row = finding(py, "python-ast", Some("a.py"));
        inherited_row.setattr("inherited", true).unwrap();
        let finalized = inherited_row.call_method0("finalize").unwrap();
        assert!(finalized
            .getattr("fingerprint")
            .unwrap()
            .eq(caused.getattr("fingerprint").unwrap())
            .unwrap());
    });
}

#[test]
fn attribution_accepts_both_declared_modes() {
    let _case = Case::new();
    Python::attach(|py| {
        assert!(
            attribution(py, &"diff".into_pyobject(py).unwrap().into_any())
                .unwrap()
                .eq("diff")
                .unwrap()
        );
        assert!(
            attribution(py, &"candidate".into_pyobject(py).unwrap().into_any())
                .unwrap()
                .eq("candidate")
                .unwrap()
        );
    });
}

#[test]
fn attribution_refuses_an_unknown_mode() {
    let _case = Case::new();
    Python::attach(|py| {
        let err = attribution(py, &"dif".into_pyobject(py).unwrap().into_any()).unwrap_err();
        assert!(err
            .matches(
                py,
                &module(py, "conductor.candidate_review.policy")
                    .getattr("PolicyError")
                    .unwrap()
            )
            .unwrap());
    });
}

#[test]
fn attribution_refuses_a_non_string() {
    let _case = Case::new();
    Python::attach(|py| {
        let err = attribution(py, &PyBool::new(py, true).to_owned().into_any()).unwrap_err();
        assert!(err
            .matches(
                py,
                &module(py, "conductor.candidate_review.policy")
                    .getattr("PolicyError")
                    .unwrap()
            )
            .unwrap());
    });
}
