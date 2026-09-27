#![cfg(feature = "python-compat-tests")]
//! Rust-owned contracts for complete mutation-scope drift reporting.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::prelude::*;
use pyo3::types::{PyFrozenSet, PyList};
use serde_json::json;
use support::{module, path, Case};

const TEST_FILE: &str = "research/tests/test_thing.py";
const CAMPAIGN: &str = "conductor/mutation_campaigns/campaign.json";

fn write_repo(
    case: &Case,
    declared: &[&str],
    source_tests: &[&str],
    registered: bool,
    baseline_unloadable: &[&str],
) {
    let source = source_tests
        .iter()
        .map(|name| format!("def {name}():\n    assert True"))
        .collect::<Vec<_>>()
        .join("\n\n")
        + "\n";
    case.write(TEST_FILE, &source);
    let nodeids: Vec<String> = declared
        .iter()
        .map(|name| format!("{TEST_FILE}::{name}"))
        .collect();
    case.write(
        CAMPAIGN,
        &json!({"test_scopes": {TEST_FILE: {
            "mode": "complete", "inventory": "python_ast", "nodeids": nodeids
        }}})
        .to_string(),
    );
    let campaigns = if registered {
        vec![json!({"manifest": CAMPAIGN})]
    } else {
        vec![]
    };
    case.write(
        "conductor/mutation_campaigns/registry.json",
        &json!({"campaigns": campaigns}).to_string(),
    );
    case.write(
        "conductor/mutation_campaigns/reproducibility_baseline.json",
        &json!({"unloadable_manifests": baseline_unloadable}).to_string(),
    );
}

fn scan<'py>(py: Python<'py>, case: &Case, restrict: &Bound<'py, PyAny>) -> Bound<'py, PyAny> {
    module(py, "conductor.complete_scope_drift")
        .getattr("scan")
        .unwrap()
        .call1((path(py, case.root()), restrict))
        .unwrap()
}

fn none<'py>(py: Python<'py>) -> Bound<'py, PyAny> {
    py.None().into_bound(py)
}

fn field_strings(drift: &Bound<'_, PyAny>, name: &str) -> Vec<String> {
    drift.getattr(name).unwrap().extract().unwrap()
}

#[test]
fn added_test_is_reported_as_new_drift() {
    let case = Case::new();
    write_repo(
        &case,
        &["test_a", "test_b"],
        &["test_a", "test_b", "test_c"],
        true,
        &[],
    );
    Python::attach(|py| {
        let drifts = scan(py, &case, &none(py));
        assert_eq!(drifts.len().unwrap(), 1);
        let drift = drifts.get_item(0).unwrap();
        assert_eq!(
            field_strings(&drift, "missing"),
            [format!("{TEST_FILE}::test_c")]
        );
        assert!(field_strings(&drift, "extra").is_empty());
        assert!(!drift.getattr("known").unwrap().extract::<bool>().unwrap());
    });
}

#[test]
fn removed_test_is_reported_as_extra() {
    let case = Case::new();
    write_repo(&case, &["test_a", "test_b"], &["test_a"], true, &[]);
    Python::attach(|py| {
        let drifts = scan(py, &case, &none(py));
        assert_eq!(drifts.len().unwrap(), 1);
        let drift = drifts.get_item(0).unwrap();
        assert_eq!(
            field_strings(&drift, "extra"),
            [format!("{TEST_FILE}::test_b")]
        );
        assert!(field_strings(&drift, "missing").is_empty());
    });
}

#[test]
fn reordering_alone_is_drift() {
    let case = Case::new();
    write_repo(
        &case,
        &["test_b", "test_a"],
        &["test_a", "test_b"],
        true,
        &[],
    );
    Python::attach(|py| {
        let drifts = scan(py, &case, &none(py));
        assert_eq!(drifts.len().unwrap(), 1);
        let drift = drifts.get_item(0).unwrap();
        assert!(field_strings(&drift, "missing").is_empty());
        assert!(field_strings(&drift, "extra").is_empty());
        assert!(drift
            .getattr("reordered")
            .unwrap()
            .extract::<bool>()
            .unwrap());
    });
}

#[test]
fn scope_in_sync_reports_nothing() {
    let case = Case::new();
    write_repo(
        &case,
        &["test_a", "test_b"],
        &["test_a", "test_b"],
        true,
        &[],
    );
    Python::attach(|py| assert!(scan(py, &case, &none(py)).eq(PyList::empty(py)).unwrap()));
}

#[test]
fn unregistered_manifest_is_ignored() {
    let case = Case::new();
    write_repo(&case, &["test_a"], &["test_a", "test_b"], false, &[]);
    Python::attach(|py| assert!(scan(py, &case, &none(py)).eq(PyList::empty(py)).unwrap()));
}

#[test]
fn baseline_absorbed_drift_is_labelled_known() {
    let case = Case::new();
    write_repo(&case, &["test_a"], &["test_a", "test_b"], true, &[CAMPAIGN]);
    Python::attach(|py| {
        let drifts = scan(py, &case, &none(py));
        assert_eq!(drifts.len().unwrap(), 1);
        assert!(drifts
            .get_item(0)
            .unwrap()
            .getattr("known")
            .unwrap()
            .extract::<bool>()
            .unwrap());
    });
}

#[test]
fn restrict_skips_files_that_were_not_changed() {
    let case = Case::new();
    write_repo(&case, &["test_a"], &["test_a", "test_b"], true, &[]);
    Python::attach(|py| {
        let other = PyFrozenSet::new(py, ["some/other/file.py"]).unwrap();
        let selected = PyFrozenSet::new(py, [TEST_FILE]).unwrap();
        assert!(scan(py, &case, other.as_any())
            .eq(PyList::empty(py))
            .unwrap());
        assert_eq!(scan(py, &case, selected.as_any()).len().unwrap(), 1);
    });
}

fn assert_non_complete_scope_ignored(mode: Option<&str>) {
    let case = Case::new();
    case.write(TEST_FILE, "def test_a():\n    assert True\n");
    let mut scope =
        json!({"inventory": "python_ast", "nodeids": [format!("{TEST_FILE}::test_missing")]});
    if let Some(mode) = mode {
        scope["mode"] = json!(mode);
    }
    case.write(
        CAMPAIGN,
        &json!({"test_scopes": {TEST_FILE: scope}}).to_string(),
    );
    case.write(
        "conductor/mutation_campaigns/registry.json",
        &json!({"campaigns": [{"manifest": CAMPAIGN}]}).to_string(),
    );
    case.write(
        "conductor/mutation_campaigns/reproducibility_baseline.json",
        &json!({"unloadable_manifests": []}).to_string(),
    );
    Python::attach(|py| assert!(scan(py, &case, &none(py)).eq(PyList::empty(py)).unwrap()));
}

#[test]
fn non_complete_scopes_are_ignored_partial() {
    assert_non_complete_scope_ignored(Some("partial"));
}

#[test]
fn non_complete_scopes_are_ignored_unset() {
    assert_non_complete_scope_ignored(None);
}
