#![cfg(feature = "python-compat-tests")]
//! Shared nodeid-table compression and native evidence-verifier contracts.

#[path = "python_contracts/agent_comm_support.rs"]
#[allow(dead_code)]
mod comm_support;
#[path = "python_contracts/mutation_testing_fixture.rs"]
#[allow(dead_code)]
mod fixture;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use comm_support::py_json;
use fixture::{constant, equal, pass_receipt, py_expected, registry, temporary_campaign, testing};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyList};
use serde_json::{json, Map, Value};
use std::fs;
use support::{assert_error, module, AttrPatch, Case};

const ALPHA: &str = "conductor/test_equivalence_probe.py::test_alpha";
const BETA: &str = "conductor/test_equivalence_probe.py::test_beta";
const GAMMA: &str = "conductor/test_equivalence_probe.py::test_gamma";

fn encoding<'py>(py: Python<'py>) -> Bound<'py, PyAny> {
    module(py, "conductor.mutation_testing_support").into_any()
}

fn report(overrides: Value) -> Value {
    let mut tests = Map::new();
    tests.insert(
        ALPHA.into(),
        json!({"cases":1,"duration_seconds":0.259,"outcome":"PASSED"}),
    );
    tests.insert(
        BETA.into(),
        json!({"cases":2,"duration_seconds":1.488,"outcome":"FAILED"}),
    );
    let mut value = json!({
        "status":"COMPLETE",
        "tests": Value::Object(tests),
        "failed_nodeids":[BETA],
        "missing_nodeids":[],
        "unmapped_cases":[]
    });
    for (key, replacement) in overrides.as_object().unwrap() {
        value[key] = replacement.clone();
    }
    value
}

fn intern<'py>(py: Python<'py>, receipt: &Bound<'py, PyAny>, report: Value) -> Bound<'py, PyAny> {
    encoding(py)
        .getattr("intern_test_attribution")
        .unwrap()
        .call1((receipt, py_json(py, report)))
        .unwrap()
}

fn expand<'py>(
    py: Python<'py>,
    receipt: &Bound<'py, PyAny>,
    interned: &Bound<'py, PyAny>,
) -> PyResult<Bound<'py, PyAny>> {
    encoding(py)
        .getattr("expand_test_attribution")
        .unwrap()
        .call1((receipt, interned))
}

#[test]
fn test_one_table_is_shared_by_every_mutant_in_the_receipt() {
    let _case = Case::new();
    Python::attach(|py| {
        let receipt = PyDict::new(py);
        receipt.set_item("mutants", PyList::empty(py)).unwrap();
        let first = intern(py, receipt.as_any(), report(json!({})));
        let mut tests = Map::new();
        tests.insert(
            BETA.into(),
            json!({"cases":2,"duration_seconds":1.5,"outcome":"PASSED"}),
        );
        tests.insert(
            GAMMA.into(),
            json!({"cases":1,"duration_seconds":0.1,"outcome":"PASSED"}),
        );
        let second = intern(
            py,
            receipt.as_any(),
            report(json!({"tests": Value::Object(tests), "failed_nodeids":[]})),
        );
        let sorted = py.import("builtins").unwrap().getattr("sorted").unwrap();
        equal(
            &sorted.call1((first.get_item("tests").unwrap(),)).unwrap(),
            &py_expected(py, json!(["0", "1"])),
        );
        equal(
            &first.get_item("failed_nodeids").unwrap(),
            &py_expected(py, json!([1])),
        );
        equal(
            &sorted.call1((second.get_item("tests").unwrap(),)).unwrap(),
            &py_expected(py, json!(["1", "2"])),
        );
    });
}

#[test]
fn test_interning_preserves_every_outcome_case_count_and_duration() {
    let _case = Case::new();
    Python::attach(|py| {
        let receipt = PyDict::new(py);
        let expected = py_json(py, report(json!({})));
        let interned = encoding(py)
            .getattr("intern_test_attribution")
            .unwrap()
            .call1((&receipt, &expected))
            .unwrap();
        assert!(!interned.eq(&expected).unwrap());
        equal(&expand(py, receipt.as_any(), &interned).unwrap(), &expected);
    });
}

#[test]
fn test_a_receipt_written_before_interning_expands_unchanged() {
    let _case = Case::new();
    Python::attach(|py| {
        let legacy = py_json(py, report(json!({})));
        let receipt = py_json(py, json!({"mutants":[]}));
        equal(&expand(py, &receipt, &legacy).unwrap(), &legacy);
    });
}

#[test]
fn test_an_index_the_table_cannot_resolve_refuses_instead_of_guessing() {
    let _case = Case::new();
    Python::attach(|py| {
        let receipt = PyDict::new(py);
        let mut tests = Map::new();
        tests.insert(
            ALPHA.into(),
            json!({"cases":1,"duration_seconds":0.1,"outcome":"PASSED"}),
        );
        intern(
            py,
            receipt.as_any(),
            json!({"status":"COMPLETE","tests":Value::Object(tests),"failed_nodeids":[],"missing_nodeids":[]}),
        );
        let invalid = py_expected(py, json!({"failed_nodeids":[1]}));
        let error = expand(py, receipt.as_any(), &invalid).unwrap_err();
        assert_error(
            py,
            error,
            &encoding(py).getattr("ReceiptEncodingError").unwrap(),
            "outside a table of 1",
        );
    });
}

#[test]
fn test_cases_outside_the_ranked_scope_are_never_interned() {
    let _case = Case::new();
    Python::attach(|py| {
        let receipt = PyDict::new(py);
        let interned = intern(
            py,
            receipt.as_any(),
            report(json!({
                "unmapped_cases":["tests/other.py::test_stray"],
                "unranked_failures":["tests/other.py::test_loud"]
            })),
        );
        equal(
            &interned.get_item("failed_nodeids").unwrap(),
            &py_expected(py, json!([1])),
        );
        equal(
            &interned.get_item("unmapped_cases").unwrap(),
            &py_expected(py, json!(["tests/other.py::test_stray"])),
        );
        equal(
            &interned.get_item("unranked_failures").unwrap(),
            &py_expected(py, json!(["tests/other.py::test_loud"])),
        );
    });
}

#[test]
fn test_the_native_verifier_accepts_an_interned_receipt() {
    let case = Case::new();
    Python::attach(|py| {
        let campaign = temporary_campaign(py, &case);
        let reg = registry(py, &case);
        let subject = testing(py);
        let callback = constant(py, &campaign);
        let _patch = AttrPatch::replace(&subject, "load_campaign", callback.as_any());
        let receipt_path = pass_receipt(py, &case, &campaign);
        let payload: Value = serde_json::from_slice(&fs::read(&receipt_path).unwrap()).unwrap();
        let data = py_json(py, payload);
        let nodeid = campaign
            .getattr("ranked_tests")
            .unwrap()
            .get_item(0)
            .unwrap()
            .getattr("nodeid")
            .unwrap();
        let nodeid_text: String = nodeid.extract().unwrap();
        let rows = data.get_item("mutants").unwrap();
        for row in rows.try_iter().unwrap() {
            let row = row.unwrap();
            let mut tests = Map::new();
            tests.insert(
                nodeid_text.clone(),
                json!({"cases":1,"duration_seconds":0.5,"outcome":"PASSED"}),
            );
            let report = py_json(
                py,
                json!({"status":"COMPLETE","tests":Value::Object(tests),"failed_nodeids":[],"missing_nodeids":[],"unmapped_cases":[]}),
            );
            let interned = encoding(py)
                .getattr("intern_test_attribution")
                .unwrap()
                .call1((&data, report))
                .unwrap();
            row.set_item("test_attribution", interned).unwrap();
        }
        equal(
            &data.get_item("test_nodeid_table").unwrap(),
            &PyList::new(py, [&nodeid]).unwrap(),
        );
        let json = module(py, "json");
        let kw = PyDict::new(py);
        kw.set_item("sort_keys", true).unwrap();
        let encoded: String = json
            .getattr("dumps")
            .unwrap()
            .call((&data,), Some(&kw))
            .unwrap()
            .extract()
            .unwrap();
        fs::write(&receipt_path, encoded).unwrap();
        let result = fixture::verify(py, &case, &reg, &["test_one.py"]);
        assert_eq!(
            result
                .get_item("status")
                .unwrap()
                .extract::<String>()
                .unwrap(),
            "PASS"
        );
        equal(
            &result.get_item("missing_evidence").unwrap(),
            &PyList::empty(py),
        );
    });
}
