//! Rust-owned report and manifest fixtures for generated mutation adapters.

use crate::support::{module, path};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyList, PyModule};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

pub const CARGO_CRATE: &str = "tooling/native/snapshot-retention";
pub const WORKTREE: &str = "/snap/worktree";

pub fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

pub fn cargo_manifest() -> PathBuf {
    root().join("src/conductor/testdata/cargo/generated_cargo_campaign_fixture.json")
}

pub fn fest_manifest() -> PathBuf {
    root().join("src/conductor/testdata/fest/generated_fest_campaign_fixture.json")
}

pub fn cargo(py: Python<'_>) -> Bound<'_, PyModule> {
    module(py, "conductor.mutation_engine_cargo")
}

pub fn fest(py: Python<'_>) -> Bound<'_, PyModule> {
    module(py, "conductor.mutation_engine_fest")
}

pub fn generated(py: Python<'_>) -> Bound<'_, PyModule> {
    module(py, "conductor.mutation_engine_generated")
}

pub fn py_json<'py>(py: Python<'py>, value: Value) -> Bound<'py, PyAny> {
    module(py, "json")
        .getattr("loads")
        .unwrap()
        .call1((value.to_string(),))
        .unwrap()
}

pub fn equal(actual: &Bound<'_, PyAny>, expected: &Bound<'_, PyAny>) {
    assert!(
        actual.eq(expected).unwrap(),
        "actual {:?}, expected {:?}",
        actual,
        expected
    );
}

pub fn as_list<'py>(value: &Bound<'py, PyAny>) -> Bound<'py, PyList> {
    value.clone().cast_into::<PyList>().unwrap()
}

pub fn campaign<'py>(py: Python<'py>, manifest: &Path) -> Bound<'py, PyAny> {
    generated(py)
        .getattr("load_generated_campaign")
        .unwrap()
        .call1((path(py, manifest),))
        .unwrap()
}

pub fn cargo_outcome(summary: &str) -> Value {
    cargo_outcome_with(summary, 437, "Ok(vec![])")
}

pub fn cargo_outcome_with(summary: &str, line: i64, replacement: &str) -> Value {
    let function = "snapshot_stale_branches";
    let file = "src/lib.rs";
    json!({
        "scenario":{"Mutant":{
            "name":format!("{file}:{line}:5: replace {function}"),
            "package":"snapshot-retention",
            "file":file,
            "function":{
                "function_name":function,
                "return_type":"-> Result<Vec<SnapshotAction>, Error>",
                "span":{"start":{"line":line,"column":1},"end":{"line":line+9,"column":2}}
            },
            "span":{"start":{"line":line,"column":5},"end":{"line":line+1,"column":40}},
            "replacement":replacement,
            "genre":"FnValue"
        }},
        "summary":summary,
        "phase_results":[{"phase":"Build","duration":1.5},{"phase":"Test","duration":0.25}]
    })
}

pub fn cargo_baseline(summary: &str) -> Value {
    json!({"scenario":"Baseline","summary":summary,"phase_results":[]})
}

pub fn cargo_rows<'py>(py: Python<'py>, outcomes: Vec<Value>) -> Bound<'py, PyList> {
    let report = py_json(py, json!({"outcomes":outcomes}));
    as_list(
        &cargo(py)
            .getattr("_rows")
            .unwrap()
            .call1((report, CARGO_CRATE))
            .unwrap(),
    )
}

pub fn fest_mutant(offset: i64, line: i64) -> Value {
    let original = "\"gh\"";
    json!({
        "file_path":format!("{WORKTREE}/conductor/gate_rollout.py"),
        "line":line,"column":1,"byte_offset":offset,
        "byte_length":original.chars().count(),
        "original_text":original,"mutated_text":"\"\"",
        "mutator_name":"constant_replace"
    })
}

pub fn fest_result(status: &str, offset: i64, line: i64) -> Value {
    json!({"mutant":fest_mutant(offset,line),"status":status,"tests_run":[],"duration":{"secs":0,"nanos":1_000_000}})
}

pub fn fest_rows<'py>(py: Python<'py>, results: Vec<Value>) -> Bound<'py, PyList> {
    let report = py_json(py, json!({"results":results}));
    as_list(
        &fest(py)
            .getattr("_rows")
            .unwrap()
            .call1((report, path(py, Path::new(WORKTREE))))
            .unwrap(),
    )
}
