#![cfg(feature = "python-compat-tests")]
//! Rust-owned fixtures for the public mutation value Python boundary.

use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict};
use serde_json::{json, Value};

pub const NODE: &str = "conductor/test_mutation_value.py::test_contract";
pub const SOURCE: &str = "conductor/mutation_value.py";

pub fn to_python<'py>(py: Python<'py>, value: &Value) -> Bound<'py, PyAny> {
    PyModule::import(py, "json")
        .unwrap()
        .getattr("loads")
        .unwrap()
        .call1((value.to_string(),))
        .unwrap()
}

pub fn from_python(value: &Bound<'_, PyAny>) -> Value {
    let text: String = PyModule::import(value.py(), "json")
        .unwrap()
        .getattr("dumps")
        .unwrap()
        .call1((value,))
        .unwrap()
        .extract()
        .unwrap();
    serde_json::from_str(&text).unwrap()
}

pub fn kwargs<'py>(py: Python<'py>, pairs: &[(&str, Value)]) -> Bound<'py, PyDict> {
    let result = PyDict::new(py);
    for (name, value) in pairs {
        result.set_item(*name, to_python(py, value)).unwrap();
    }
    result
}

pub fn payload(nodeid: &str) -> Value {
    json!({
        "enabled": true,
        "adapter": "pytest-junit",
        "baseline_repetitions": 2,
        "required_contracts": [{
            "id": "contract", "criticality": "critical", "active_paths": [SOURCE]
        }],
        "tests": [{"nodeid": nodeid, "contract_id": "contract"}],
        "mutation_contracts": {"mutant": "contract"}
    })
}

pub fn load_spec<'py>(
    py: Python<'py>,
    module: &Bound<'py, PyAny>,
    value: &Value,
) -> Bound<'py, PyAny> {
    let options = kwargs(
        py,
        &[
            ("ranked_nodeids", json!([NODE])),
            ("mutation_ids", json!(["mutant"])),
            (
                "source_paths",
                json!([SOURCE, "conductor/test_mutation_value.py", "second.py"]),
            ),
        ],
    );
    module
        .getattr("load_value_analysis")
        .unwrap()
        .call((to_python(py, value),), Some(&options))
        .unwrap()
}

pub fn report(outcomes: &[(&str, &str, f64)]) -> Value {
    let mut tests = serde_json::Map::new();
    for (nodeid, outcome, duration) in outcomes {
        tests.insert(
            (*nodeid).to_owned(),
            json!({"outcome": outcome, "duration_seconds": duration, "cases": 1}),
        );
    }
    json!({"status": "COMPLETE", "tests": tests, "missing_nodeids": [], "unmapped_cases": []})
}

pub fn analyze<'py>(
    py: Python<'py>,
    module: &Bound<'py, PyAny>,
    spec: &Bound<'py, PyAny>,
    baseline: &Value,
    mutants: &Value,
    outcomes: &Value,
) -> Value {
    let options = kwargs(
        py,
        &[
            ("baseline_reports", baseline.clone()),
            ("mutant_reports", mutants.clone()),
            ("mutant_outcomes", outcomes.clone()),
        ],
    );
    from_python(
        &module
            .getattr("analyze_test_value")
            .unwrap()
            .call((spec,), Some(&options))
            .unwrap(),
    )
}
