#![cfg(feature = "python-compat-tests")]
//! Public value-analysis contract, with Rust fixtures and assertions.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;
#[path = "python_contracts/mutation_value_support.rs"]
#[allow(dead_code)]
mod value_support;

use pyo3::prelude::*;
use serde_json::{json, Value};
use support::{assert_error, module, Case};
use value_support::{
    analyze, from_python, kwargs, load_spec, payload, report, to_python, NODE, SOURCE,
};

fn load_result(py: Python<'_>, value: &Value, ranked: Value) -> PyResult<Py<PyAny>> {
    let m = module(py, "conductor.mutation_value");
    let options = kwargs(
        py,
        &[
            ("ranked_nodeids", ranked),
            ("mutation_ids", json!(["mutant"])),
            (
                "source_paths",
                json!([SOURCE, "conductor/test_mutation_value.py", "second.py"]),
            ),
        ],
    );
    m.getattr("load_value_analysis")?
        .call((to_python(py, value),), Some(&options))
        .map(Bound::unbind)
}

fn rejects(py: Python<'_>, value: &Value, ranked: Value, message: &str) {
    let m = module(py, "conductor.mutation_value");
    assert_error(
        py,
        load_result(py, value, ranked).unwrap_err(),
        &m.getattr("ValueEvidenceError").unwrap(),
        message,
    );
}

#[test]
fn value_spec_requires_high_risk_contracts_bound_to_production() {
    let _case = Case::new();
    Python::attach(|py| {
        let m = module(py, "conductor.mutation_value");
        let valid = payload(NODE);
        let spec = load_spec(py, m.as_any(), &valid);
        assert_eq!(
            from_python(
                &spec
                    .getattr("contracts")
                    .unwrap()
                    .get_item(0)
                    .unwrap()
                    .getattr("active_paths")
                    .unwrap()
            ),
            json!([SOURCE])
        );
        let none_options = kwargs(
            py,
            &[
                ("ranked_nodeids", json!([])),
                ("mutation_ids", json!([])),
                ("source_paths", json!([])),
            ],
        );
        assert!(m
            .getattr("load_value_analysis")
            .unwrap()
            .call((py.None(),), Some(&none_options))
            .unwrap()
            .is_none());
        let mut low = valid.clone();
        low["required_contracts"][0]["criticality"] = json!("low");
        rejects(py, &low, json!([NODE]), "critical or high");
        let mut test_bound = valid.clone();
        test_bound["required_contracts"][0]["active_paths"] =
            json!(["conductor/test_mutation_value.py"]);
        rejects(py, &test_bound, json!([NODE]), "tests, not production");
        rejects(py, &json!([]), json!([NODE]), "must be an object");
    });
}

#[test]
fn value_spec_rejects_every_invalid_field_and_unbound_contract() {
    let _case = Case::new();
    Python::attach(|py| {
        let valid = payload(NODE);
        for (pointer, bad, message) in [
            ("/enabled", json!(false), "enabled must be true"),
            ("/adapter", json!("unsupported"), "adapter must be"),
            ("/baseline_repetitions", json!(1), "integer in"),
            ("/required_contracts", json!([]), "non-empty list"),
            ("/tests", json!([]), "non-empty list"),
            ("/mutation_contracts", json!([]), "must be an object"),
            ("/required_contracts/0/id", json!(""), "non-empty string"),
            (
                "/required_contracts/0/active_paths",
                json!([]),
                "must be non-empty",
            ),
            (
                "/required_contracts/0/active_paths",
                json!(["/absolute.py"]),
                "repository-relative",
            ),
            (
                "/required_contracts/0/active_paths",
                json!(["missing.py"]),
                "unbound active paths",
            ),
            (
                "/tests/0/intentional_redundancy",
                json!("yes"),
                "must be boolean",
            ),
            (
                "/tests/0/nodeid",
                json!("conductor/test_mutation_value.py::other"),
                "exactly match ranked_tests",
            ),
            (
                "/tests/0/contract_id",
                json!("unknown"),
                "unknown contracts",
            ),
            (
                "/mutation_contracts",
                json!({"other": "contract"}),
                "exactly match planned mutations",
            ),
            (
                "/mutation_contracts",
                json!({"mutant": "unknown"}),
                "unknown contracts",
            ),
        ] {
            let mut invalid = valid.clone();
            if pointer == "/tests/0/intentional_redundancy" {
                invalid["tests"][0]["intentional_redundancy"] = bad;
            } else {
                *invalid
                    .pointer_mut(pointer)
                    .unwrap_or_else(|| panic!("{pointer}")) = bad;
            }
            rejects(py, &invalid, json!([NODE]), message);
        }
        let mut duplicate = valid.clone();
        duplicate["required_contracts"]
            .as_array_mut()
            .unwrap()
            .push(valid["required_contracts"][0].clone());
        rejects(py, &duplicate, json!([NODE]), "duplicate ids");
        let mut unbound = valid.clone();
        unbound["required_contracts"]
            .as_array_mut()
            .unwrap()
            .push(json!({"id": "second", "criticality": "high", "active_paths": ["second.py"]}));
        unbound["tests"]
            .as_array_mut()
            .unwrap()
            .push(json!({"nodeid": NODE, "contract_id": "second"}));
        unbound["tests"][0]["nodeid"] = json!("conductor/test_mutation_value.py::first");
        rejects(
            py,
            &unbound,
            json!(["conductor/test_mutation_value.py::first", NODE]),
            "every contract needs",
        );
    });
}

fn two_contract_spec<'py>(
    py: Python<'py>,
    m: &Bound<'py, PyAny>,
    first: &str,
    second: &str,
) -> Bound<'py, PyAny> {
    let mut value = payload(first);
    value["required_contracts"] = json!([
        {"id": "first", "criticality": "critical", "active_paths": [SOURCE]},
        {"id": "second", "criticality": "high", "active_paths": ["conductor/mutation_testing.py"]}
    ]);
    value["tests"] = json!([
        {"nodeid": first, "contract_id": "first"},
        {"nodeid": second, "contract_id": "second"}
    ]);
    value["mutation_contracts"] = json!({"first_mutant": "first", "second_mutant": "second"});
    let options = kwargs(
        py,
        &[
            ("ranked_nodeids", json!([first, second])),
            ("mutation_ids", json!(["first_mutant", "second_mutant"])),
            (
                "source_paths",
                json!([SOURCE, "conductor/mutation_testing.py"]),
            ),
        ],
    );
    m.getattr("load_value_analysis")
        .unwrap()
        .call((to_python(py, &value),), Some(&options))
        .unwrap()
}

#[test]
fn value_analysis_selects_core_and_flags_merge_and_delete_candidates() {
    let _case = Case::new();
    Python::attach(|py| {
        let m = module(py, "conductor.mutation_value");
        let names = ["test_fast", "test_slow", "test_merge", "test_empty"]
            .map(|suffix| format!("conductor/test_mutation_value.py::{suffix}"));
        let mut value = payload(&names[0]);
        value["tests"] = json!(names.iter().enumerate().map(|(i, nodeid)|
            json!({"nodeid": nodeid, "contract_id": "contract", "intentional_redundancy": i == 1}))
            .collect::<Vec<_>>());
        let options = kwargs(
            py,
            &[
                ("ranked_nodeids", json!(names)),
                ("mutation_ids", json!(["mutant"])),
                ("source_paths", json!([SOURCE])),
            ],
        );
        let spec = m
            .getattr("load_value_analysis")
            .unwrap()
            .call((to_python(py, &value),), Some(&options))
            .unwrap();
        let baseline = report(&[
            (&names[0], "PASSED", 0.1),
            (&names[1], "PASSED", 0.4),
            (&names[2], "PASSED", 0.5),
            (&names[3], "PASSED", 0.2),
        ]);
        let mutant = report(&[
            (&names[0], "FAILED", 0.1),
            (&names[1], "FAILED", 0.4),
            (&names[2], "FAILED", 0.5),
            (&names[3], "PASSED", 0.2),
        ]);
        let result = analyze(
            py,
            m.as_any(),
            &spec,
            &json!([baseline.clone(), baseline]),
            &json!({"mutant": mutant}),
            &json!({"mutant": "KILLED"}),
        );
        assert_eq!(result["status"], "PASS");
        assert_eq!(result["retained_core"], json!([names[0]]));
        let rows = result["tests"].as_array().unwrap();
        let by_name = |index: usize| {
            rows.iter()
                .find(|row| row["nodeid"] == names[index])
                .unwrap()
        };
        assert_eq!(by_name(0)["classification"], "CORE");
        assert_eq!(by_name(1)["classification"], "INTENTIONAL_REDUNDANCY");
        assert_eq!(by_name(1)["dominated_by"], json!([names[0]]));
        assert_eq!(by_name(2)["classification"], "MERGE");
        assert_eq!(by_name(3)["classification"], "DELETE_CANDIDATE");
    });
}

#[test]
fn value_analysis_fails_closed_on_flakes_cross_contract_kills_and_incomplete_maps() {
    let _case = Case::new();
    Python::attach(|py| {
        let m = module(py, "conductor.mutation_value");
        let first = "conductor/test_mutation_value.py::test_first";
        let second = "conductor/test_mutation_value.py::test_second";
        let spec = two_contract_spec(py, m.as_any(), first, second);
        let clean = report(&[(first, "PASSED", 0.1), (second, "PASSED", 0.1)]);
        let flaky = report(&[(first, "FAILED", 0.1), (second, "PASSED", 0.1)]);
        let wrong = report(&[(first, "PASSED", 0.1), (second, "FAILED", 0.1)]);
        let result = analyze(
            py,
            m.as_any(),
            &spec,
            &json!([clean, flaky]),
            &json!({"first_mutant": wrong, "second_mutant": wrong}),
            &json!({"first_mutant": "KILLED", "second_mutant": "KILLED"}),
        );
        assert_eq!(result["status"], "FAIL_CLOSED");
        let errors = result["errors"].as_array().unwrap();
        assert!(errors
            .iter()
            .any(|e| e.as_str().unwrap().contains("baseline instability")));
        assert!(errors
            .iter()
            .any(|e| e.as_str().unwrap().contains("first_mutant")
                && e.as_str().unwrap().contains("contract 'first'")));
        let incomplete = analyze(
            py,
            m.as_any(),
            &spec,
            &json!([{"status": "INCOMPLETE", "tests": null}]),
            &json!({"first_mutant": {"status": "INCOMPLETE"},
                "second_mutant": {"status": "COMPLETE", "tests": null}}),
            &json!({"first_mutant": "SURVIVED", "second_mutant": "KILLED"}),
        );
        assert_eq!(incomplete["status"], "FAIL_CLOSED");
        assert_eq!(incomplete["retained_core"], json!([]));
        for phrase in [
            "repetition count mismatch",
            "attribution is incomplete",
            "has no test map",
        ] {
            assert!(
                incomplete["errors"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|e| e.as_str().unwrap().contains(phrase)),
                "{phrase}"
            );
        }
    });
}

#[test]
fn value_analysis_marks_missing_test_map_without_dropping_evidence() {
    let _case = Case::new();
    Python::attach(|py| {
        let m = module(py, "conductor.mutation_value");
        let spec = load_spec(py, m.as_any(), &payload(NODE));
        let other = "conductor/test_mutation_value.py::test_first";
        let clean = report(&[(other, "PASSED", 0.1)]);
        let result = analyze(
            py,
            m.as_any(),
            &spec,
            &json!([clean.clone(), clean]),
            &json!({"mutant": {"status": "COMPLETE", "tests": null}}),
            &json!({"mutant": "KILLED"}),
        );
        assert_eq!(result["killers_by_mutant"], json!({"mutant": []}));
        assert!(result["errors"].as_array().unwrap().iter().any(|e| e
            .as_str()
            .unwrap()
            .contains("mutant 'mutant' has no test map")));
    });
}

#[test]
fn value_admission_rejects_unmeasured_and_low_value_new_tests() {
    let _case = Case::new();
    Python::attach(|py| {
        let m = module(py, "conductor.mutation_value");
        let names = ["test_core", "test_redundant", "test_delete"]
            .map(|suffix| format!("conductor/test_mutation_value.py::{suffix}"));
        let evidence = json!({"schema_version": "llm.mutation-testing.test-value.v1", "status": "PASS",
            "tests": [
                {"nodeid": names[0], "classification": "CORE"},
                {"nodeid": names[1], "classification": "INTENTIONAL_REDUNDANCY"},
                {"nodeid": names[2], "classification": "DELETE_CANDIDATE"}]});
        let admission = m.getattr("admission_errors").unwrap();
        let call = |data: &Value, required: Value| {
            from_python(
                &admission
                    .call1((to_python(py, data), to_python(py, &required)))
                    .unwrap(),
            )
        };
        assert_eq!(call(&evidence, json!([names[0], names[1]])), json!([]));
        let errors = call(&evidence, json!([names[2], "missing::test"]));
        assert!(errors
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e.as_str().unwrap().contains("DELETE_CANDIDATE")));
        assert!(errors
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e.as_str().unwrap().contains("no value classification")));
        assert_eq!(
            call(&Value::Null, json!([names[0]])),
            json!(["receipt has no test_value evidence"])
        );
        let bad = json!({"schema_version": "old", "status": "FAIL", "tests": null});
        assert_eq!(
            call(&bad, json!([names[0]])),
            json!([
                "test_value schema is not current",
                "test_value status='FAIL'",
                "test_value.tests must be a list"
            ])
        );
        let receipt = m.getattr("test_value_receipt_errors").unwrap();
        let receipt_call = |data: &Value| {
            let options = kwargs(
                py,
                &[
                    ("expected_nodeids", json!([names[0]])),
                    ("expected_repetitions", json!(2)),
                ],
            );
            from_python(
                &receipt
                    .call((to_python(py, data),), Some(&options))
                    .unwrap(),
            )
        };
        assert_eq!(
            receipt_call(&Value::Null),
            json!(["test_value evidence is missing"])
        );
        let errors = receipt_call(&json!({"schema_version": "old", "status": "FAIL", "tests": []}));
        let actual = errors
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_str().unwrap().to_owned())
            .collect::<std::collections::BTreeSet<_>>();
        let expected = json!([
            "test_value schema is not current",
            "test_value status='FAIL'",
            "test_value nodeids do not match ranked tests",
            "test_value baseline repetitions mismatch"
        ])
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap().to_owned())
        .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(actual, expected);
    });
}

#[test]
fn value_analysis_uses_explicit_failed_nodeids_for_killers() {
    let _case = Case::new();
    Python::attach(|py| {
        let m = module(py, "conductor.mutation_value");
        let spec = load_spec(py, m.as_any(), &payload(NODE));
        let clean = report(&[(NODE, "PASSED", 0.1)]);
        let mut explicit = clean.clone();
        explicit["failed_nodeids"] = json!([NODE]);
        let result = analyze(
            py,
            m.as_any(),
            &spec,
            &json!([clean.clone(), clean]),
            &json!({"mutant": explicit}),
            &json!({"mutant": "KILLED"}),
        );
        assert_eq!(result["status"], "PASS");
        assert_eq!(result["killers_by_mutant"], json!({"mutant": [NODE]}));
    });
}
