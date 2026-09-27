#![cfg(feature = "python-compat-tests")]
//! Changed-source review contracts for import declaration, asserted in Rust.

#[path = "python_contracts/candidate_import_support.rs"]
#[allow(dead_code)]
mod import_support;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use import_support::{
    checks, distributions_patch, equal, model, review_context, ExemptionPatch, FileChange, MANIFEST,
};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyList, PyTuple};
use serde_json::json;
use support::Case;

fn check<'py>(py: Python<'py>, context: &Bound<'py, PyAny>) -> Bound<'py, PyAny> {
    checks(py)
        .getattr("check_import_declaration")
        .unwrap()
        .call1((context,))
        .unwrap()
}

fn findings<'py>(result: &Bound<'py, PyAny>) -> Bound<'py, PyAny> {
    result.getattr("findings").unwrap()
}

fn rules<'py>(py: Python<'py>, result: &Bound<'py, PyAny>) -> Bound<'py, PyAny> {
    let items: Vec<_> = findings(result)
        .try_iter()
        .unwrap()
        .map(|item| item.unwrap().getattr("rule_id").unwrap())
        .collect();
    PyList::new(py, items).unwrap().into_any()
}

fn rule_paths<'py>(py: Python<'py>, result: &Bound<'py, PyAny>) -> Bound<'py, PyAny> {
    let items: Vec<_> = findings(result)
        .try_iter()
        .unwrap()
        .map(|item| {
            let item = item.unwrap();
            PyTuple::new(
                py,
                [
                    item.getattr("rule_id").unwrap(),
                    item.getattr("path").unwrap(),
                ],
            )
            .unwrap()
            .into_any()
        })
        .collect();
    PyList::new(py, items).unwrap().into_any()
}

fn expected_rule_path<'py>(py: Python<'py>, rule: &str, source: &str) -> Bound<'py, PyAny> {
    PyList::new(py, [PyTuple::new(py, [rule, source]).unwrap()])
        .unwrap()
        .into_any()
}

fn metric(result: &Bound<'_, PyAny>, name: &str) -> i32 {
    result
        .getattr("metrics")
        .unwrap()
        .get_item(name)
        .unwrap()
        .extract()
        .unwrap()
}

fn is_severity(py: Python<'_>, finding: &Bound<'_, PyAny>, name: &str) {
    let expected = model(py)
        .getattr("Severity")
        .unwrap()
        .getattr(name)
        .unwrap();
    assert!(finding.getattr("severity").unwrap().is(&expected));
}

#[test]
fn the_check_blocks_an_undeclared_import_in_a_shipped_module() {
    let case = Case::new();
    Python::attach(|py| {
        let ctx = review_context(
            py,
            &case,
            &[("probe/mod.py", "import yaml\n")],
            &[FileChange::source("probe/mod.py")],
            MANIFEST,
        );
        let _map = distributions_patch(py, None);
        let result = check(py, &ctx);
        equal(
            &rule_paths(py, &result),
            &expected_rule_path(py, "undeclared-dependency", "probe/mod.py"),
        );
        is_severity(py, &findings(&result).get_item(0).unwrap(), "CRITICAL");
    });
}

#[test]
fn a_test_module_is_not_shipped() {
    let case = Case::new();
    Python::attach(|py| {
        let ctx = review_context(
            py,
            &case,
            &[("probe/test_mod.py", "import yaml\n")],
            &[FileChange::test("probe/test_mod.py")],
            MANIFEST,
        );
        let _map = distributions_patch(py, None);
        equal(&findings(&check(py, &ctx)), PyList::empty(py).as_any());
    });
}

#[test]
fn a_deleted_file_is_never_opened() {
    let case = Case::new();
    Python::attach(|py| {
        let ctx = review_context(
            py,
            &case,
            &[],
            &[FileChange::deleted("probe/mod.py")],
            MANIFEST,
        );
        let _map = distributions_patch(py, None);
        let result = check(py, &ctx);
        assert_eq!(metric(&result, "python_files"), 0);
        equal(&findings(&result), PyList::empty(py).as_any());
    });
}

#[test]
fn an_unparseable_source_is_left_to_python_ast() {
    let case = Case::new();
    Python::attach(|py| {
        let ctx = review_context(
            py,
            &case,
            &[("probe/mod.py", "import yaml\ndef (\n")],
            &[FileChange::source("probe/mod.py")],
            MANIFEST,
        );
        let _map = distributions_patch(py, None);
        equal(&findings(&check(py, &ctx)), PyList::empty(py).as_any());
    });
}

#[test]
fn an_unreadable_manifest_is_itself_the_finding() {
    let case = Case::new();
    Python::attach(|py| {
        let ctx = review_context(
            py,
            &case,
            &[("probe/mod.py", "import yaml\n")],
            &[FileChange::source("probe/mod.py")],
            "[project\nname =",
        );
        let _map = distributions_patch(py, None);
        equal(
            &rules(py, &check(py, &ctx)),
            &PyList::new(py, ["unreadable-manifest"]).unwrap().into_any(),
        );
    });
}

#[test]
fn the_check_blocks_an_extra_only_import_in_a_base_dependency_tree() {
    let case = Case::new();
    Python::attach(|py| {
        let ctx = review_context(
            py,
            &case,
            &[("conductor/mod.py", "import scipy\n")],
            &[FileChange::source("conductor/mod.py")],
            MANIFEST,
        );
        let _map = distributions_patch(py, Some(json!({"scipy":["scipy"]})));
        let result = check(py, &ctx);
        equal(
            &rule_paths(py, &result),
            &expected_rule_path(py, "base-dependency-required", "conductor/mod.py"),
        );
        is_severity(py, &findings(&result).get_item(0).unwrap(), "CRITICAL");
        assert_eq!(metric(&result, "base_dependency_files"), 1);
    });
}

#[test]
fn the_strict_rule_leaves_the_wheel_packages_alone() {
    let case = Case::new();
    Python::attach(|py| {
        let ctx = review_context(
            py,
            &case,
            &[("research/mod.py", "import scipy\n")],
            &[FileChange::source("research/mod.py")],
            MANIFEST,
        );
        let _map = distributions_patch(py, Some(json!({"scipy":["scipy"]})));
        let result = check(py, &ctx);
        equal(&findings(&result), PyList::empty(py).as_any());
        assert_eq!(metric(&result, "base_dependency_files"), 0);
    });
}

#[test]
fn the_probe_may_import_pytest_at_runtime() {
    let case = Case::new();
    Python::attach(|py| {
        let _grant = ExemptionPatch::add(py, "conductor/probe.py", "pytest");
        let ctx = review_context(
            py,
            &case,
            &[("conductor/probe.py", "def run():\n    import pytest\n")],
            &[FileChange::source("conductor/probe.py")],
            MANIFEST,
        );
        let _map = distributions_patch(py, Some(json!({"pytest":["pytest"]})));
        equal(&findings(&check(py, &ctx)), PyList::empty(py).as_any());
    });
}

#[test]
fn an_exemption_that_covers_nothing_is_reported() {
    let case = Case::new();
    Python::attach(|py| {
        let _grant = ExemptionPatch::add(py, "conductor/probe.py", "pytest");
        let ctx = review_context(
            py,
            &case,
            &[("conductor/probe.py", "import polars\n")],
            &[FileChange::source("conductor/probe.py")],
            MANIFEST,
        );
        let _map = distributions_patch(py, Some(json!({"pytest":["pytest"],"polars":["polars"]})));
        let result = check(py, &ctx);
        let rows: Vec<_> = findings(&result)
            .try_iter()
            .unwrap()
            .map(|item| {
                let item = item.unwrap();
                PyTuple::new(
                    py,
                    [
                        item.getattr("rule_id").unwrap(),
                        item.getattr("severity").unwrap(),
                    ],
                )
                .unwrap()
                .into_any()
            })
            .collect();
        let actual = PyList::new(py, rows).unwrap();
        let medium = model(py)
            .getattr("Severity")
            .unwrap()
            .getattr("MEDIUM")
            .unwrap();
        let expected = PyList::new(
            py,
            [PyTuple::new(
                py,
                [
                    "stale-exemption".into_pyobject(py).unwrap().into_any(),
                    medium,
                ],
            )
            .unwrap()],
        )
        .unwrap();
        equal(actual.as_any(), expected.as_any());
    });
}
