#![cfg(feature = "python-compat-tests")]
//! Rust-owned contracts for the shipped mutation runner lineage gate.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::prelude::*;
use serde_json::{json, Value};
use std::fs;
use std::path::Path;
use support::{module, path, Case};

const LINEAGE: &str = "conductor/mutation_runner_lineage.json";

fn recorded() -> Value {
    json!({
        "audit/orchestrator/snapshot_worktree.py": "a".repeat(64),
        "conductor/mutation_scope.py": "b".repeat(64),
        "conductor/mutation_testing.py": "c".repeat(64),
        "conductor/mutation_testing_support.py": "d".repeat(64),
        "conductor/mutation_value.py": "e".repeat(64),
    })
}

fn valid(entries: Option<Value>) -> Value {
    json!({
        "schema_version": 1,
        "entries": entries.unwrap_or_else(|| json!([
            {"id": "e1", "runner_components_sha256": recorded()}
        ])),
    })
}

fn write_lineage(case: &Case, payload: &Value) {
    case.write(LINEAGE, &serde_json::to_string(payload).unwrap());
}

fn py_json<'py>(py: Python<'py>, value: &Value) -> Bound<'py, PyAny> {
    module(py, "json")
        .getattr("loads")
        .unwrap()
        .call1((serde_json::to_string(value).unwrap(),))
        .unwrap()
}

fn accepts(case: &Case, recorded_value: &Value) -> bool {
    Python::attach(|py| {
        module(py, "conductor.mutation_testing")
            .getattr("_lineage_accepts")
            .unwrap()
            .call1((py_json(py, recorded_value), path(py, case.root())))
            .unwrap()
            .extract()
            .unwrap()
    })
}

#[test]
fn accepts_an_exactly_recorded_hash_set() {
    let case = Case::new();
    write_lineage(&case, &valid(None));
    assert!(accepts(&case, &recorded()));
}

#[test]
fn rejects_a_hash_set_that_was_never_recorded() {
    let case = Case::new();
    write_lineage(&case, &valid(None));
    let mut other = recorded();
    other["conductor/mutation_value.py"] = json!("f".repeat(64));
    assert!(!accepts(&case, &other));
}

#[test]
fn one_differing_component_is_still_a_miss() {
    let case = Case::new();
    write_lineage(&case, &valid(None));
    let mut near = recorded();
    near["conductor/mutation_scope.py"] = json!("0".repeat(64));
    assert!(!accepts(&case, &near));
}

#[test]
fn a_subset_is_not_accepted() {
    let case = Case::new();
    write_lineage(&case, &valid(None));
    let mut subset = recorded();
    subset
        .as_object_mut()
        .unwrap()
        .remove("conductor/mutation_testing_support.py");
    subset
        .as_object_mut()
        .unwrap()
        .remove("conductor/mutation_value.py");
    assert!(!accepts(&case, &subset));
}

#[test]
fn a_superset_is_not_accepted() {
    let case = Case::new();
    write_lineage(&case, &valid(None));
    let mut superset = recorded();
    superset["conductor/extra.py"] = json!("9".repeat(64));
    assert!(!accepts(&case, &superset));
}

#[test]
fn matches_any_entry_not_only_the_first() {
    let case = Case::new();
    write_lineage(
        &case,
        &valid(Some(json!([
            {"id": "old", "runner_components_sha256": {"x": "1".repeat(64)}},
            {"id": "e1", "runner_components_sha256": recorded()},
        ]))),
    );
    assert!(accepts(&case, &recorded()));
}

#[test]
fn absent_file_accepts_nothing() {
    let case = Case::new();
    assert!(!accepts(&case, &recorded()));
}

#[test]
fn malformed_json_accepts_nothing() {
    let case = Case::new();
    case.write(LINEAGE, "{not json");
    assert!(!accepts(&case, &recorded()));
}

#[test]
fn unknown_schema_version_accepts_nothing() {
    let case = Case::new();
    write_lineage(
        &case,
        &json!({"schema_version": 999, "entries": [
            {"runner_components_sha256": recorded()}
        ]}),
    );
    assert!(!accepts(&case, &recorded()));
}

#[test]
fn entries_not_a_list_accepts_nothing() {
    let case = Case::new();
    write_lineage(
        &case,
        &json!({"schema_version": 1, "entries": {
            "runner_components_sha256": recorded()
        }}),
    );
    assert!(!accepts(&case, &recorded()));
}

#[test]
fn empty_entries_accepts_nothing() {
    let case = Case::new();
    write_lineage(&case, &valid(Some(json!([]))));
    assert!(!accepts(&case, &recorded()));
}

#[test]
fn non_dict_entry_is_skipped_not_fatal() {
    let case = Case::new();
    write_lineage(
        &case,
        &valid(Some(json!([
            "garbage", {"id": "e1", "runner_components_sha256": recorded()}
        ]))),
    );
    assert!(accepts(&case, &recorded()));
}

fn non_mapping_is_refused(recorded_value: Value) {
    let case = Case::new();
    write_lineage(&case, &valid(None));
    assert!(!accepts(&case, &recorded_value));
}

#[test]
fn a_non_mapping_none_is_refused() {
    non_mapping_is_refused(Value::Null);
}
#[test]
fn a_non_mapping_string_is_refused() {
    non_mapping_is_refused(json!("string"));
}
#[test]
fn a_non_mapping_number_is_refused() {
    non_mapping_is_refused(json!(42));
}
#[test]
fn a_non_mapping_list_is_refused() {
    non_mapping_is_refused(json!(["list"]));
}

#[test]
fn shipped_lineage_is_wellformed_and_documented() {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../src");
    let file = repo.join(LINEAGE);
    if !file.is_file() {
        eprintln!("{LINEAGE} is not present in this tree");
        return;
    }
    let payload: Value = serde_json::from_str(&fs::read_to_string(file).unwrap()).unwrap();
    assert_eq!(payload["schema_version"], 1);
    let entries = payload["entries"].as_array().unwrap();
    assert!(
        !entries.is_empty(),
        "an empty lineage should be deleted, not shipped"
    );
    let mut previous = 0;
    for entry in entries {
        for field in ["justification", "verified_by", "covers_diff"] {
            assert!(
                entry[field].as_str().is_some_and(|value| !value.is_empty()),
                "{} has no {field}",
                entry["id"]
            );
        }
        let hashes = entry["runner_components_sha256"].as_object().unwrap();
        assert!(
            !hashes.is_empty(),
            "{} pins no runner component",
            entry["id"]
        );
        assert!(
            hashes.len() >= previous,
            "{} pins fewer components than its predecessor",
            entry["id"]
        );
        previous = hashes.len();
        assert!(hashes
            .values()
            .all(|value| value.as_str().is_some_and(|s| s.len() == 64)));
    }
}
