//! Rust-owned synthetic manifest and receipt fixtures for generated engines.

use crate::comm_support::{json_value, py_json};
use crate::support::{module, path, Case};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

pub fn generated(py: Python<'_>) -> Bound<'_, pyo3::types::PyModule> {
    module(py, "conductor.mutation_engine_generated")
}

pub fn campaign_error(py: Python<'_>, result: PyResult<Bound<'_, PyAny>>, message: &str) {
    let error = result.unwrap_err();
    let class = module(py, "conductor.mutation_scope")
        .getattr("CampaignError")
        .unwrap();
    assert!(error.matches(py, &class).unwrap(), "{error}");
    assert!(error.to_string().contains(message), "{error}");
}

pub fn manifest(case: &Case, overrides: Value) -> PathBuf {
    let mut payload = json!({
        "campaign_id":"probe",
        "title":"probe",
        "language":"python",
        "mutation_engine":"fest",
        "generator":{"source":["conductor/gate_rollout.py"],"run_timeout_seconds":60},
        "test_argv":["python","-m","pytest","-q","conductor/test_gate_rollout.py"],
        "source_sha256":{"conductor/gate_rollout.py":"0".repeat(64)},
        "test_sha256":{"conductor/test_gate_rollout.py":"1".repeat(64)}
    });
    for (key, value) in overrides.as_object().unwrap() {
        payload[key] = value.clone();
    }
    case.write("campaign.json", &payload.to_string())
}

pub fn load<'py>(py: Python<'py>, file: &Path) -> Bound<'py, PyAny> {
    generated(py)
        .getattr("load_generated_campaign")
        .unwrap()
        .call1((path(py, file),))
        .unwrap()
}

pub fn scored<'py>(
    py: Python<'py>,
    case: &Case,
    rows: &[(&str, &str)],
    baseline: &[&str],
) -> Bound<'py, PyDict> {
    let campaign = load(py, &manifest(case, json!({"survivor_baseline":baseline})));
    let mutants: Vec<Value> = rows
        .iter()
        .map(|(id, outcome)| json!({"id":id,"outcome":outcome}))
        .collect();
    let receipt = py_json(py, json!({"mutants":mutants}))
        .cast_into::<PyDict>()
        .unwrap();
    generated(py)
        .getattr("score")
        .unwrap()
        .call1((campaign, &receipt))
        .unwrap();
    receipt
}

pub fn survivors<'py>(
    py: Python<'py>,
    case: &Case,
    names: &[&str],
    baseline: &[&str],
) -> Bound<'py, PyDict> {
    let mut rows: Vec<(&str, &str)> = names.iter().map(|name| (*name, "SURVIVED")).collect();
    rows.push(("k", "KILLED"));
    scored(py, case, &rows, baseline)
}

pub fn receipt_json(value: &Bound<'_, PyAny>) -> Value {
    json_value(value)
}

pub fn simple_campaign<'py>(py: Python<'py>, campaign_id: &str) -> Bound<'py, PyAny> {
    let kw = pyo3::types::PyDict::new(py);
    kw.set_item("campaign_id", campaign_id).unwrap();
    module(py, "types")
        .getattr("SimpleNamespace")
        .unwrap()
        .call((), Some(&kw))
        .unwrap()
}
