#![cfg(feature = "python-compat-tests")]
//! Rust assertions for the mutation runner's killer and receipt-folding boundary.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;
#[path = "python_contracts/mutation_value_support.rs"]
#[allow(dead_code)]
mod value_support;

use pyo3::prelude::*;
use serde_json::{json, Value};
use support::{module, Case};
use value_support::{from_python, kwargs, to_python};

fn verdict(py: Python<'_>, declared: &[&str], report: Option<Value>, outcome: &str) -> Value {
    let mutation = module(py, "types")
        .getattr("SimpleNamespace")
        .unwrap()
        .call(
            (),
            Some(&kwargs(py, &[("expected_killers", json!(declared))])),
        )
        .unwrap();
    let report = report.map_or_else(|| py.None().into_bound(py), |row| to_python(py, &row));
    from_python(
        &module(py, "conductor.mutation_testing")
            .getattr("killer_verdict")
            .unwrap()
            .call1((mutation, report, outcome))
            .unwrap(),
    )
}

#[test]
fn declared_failed_error_and_collateral_tests_are_disjointly_reported() {
    let _case = Case::new();
    Python::attach(|py| {
        let confirmed = verdict(
            py,
            &["t::declared", "t::silent"],
            Some(json!({"status":"COMPLETE","tests":{
                "t::declared":{"outcome":"FAILED"},
                "t::stranger":{"outcome":"ERROR"},
                "t::ok":{"outcome":"PASSED"}
            }})),
            "KILLED",
        );
        assert_eq!(confirmed["status"], "CONFIRMED");
        assert_eq!(confirmed["declared"], json!(["t::declared", "t::silent"]));
        assert_eq!(
            confirmed["observed_failures"],
            json!(["t::declared", "t::stranger"])
        );
        assert_eq!(confirmed["matched"], json!(["t::declared"]));
        assert_eq!(confirmed["unobservable"], json!(["t::silent"]));
        assert_eq!(confirmed["collateral"], json!(["t::stranger"]));
        assert!(confirmed.get("unranked_failures").is_none());

        let misattributed = verdict(
            py,
            &["t::declared"],
            Some(json!({"status":"COMPLETE","tests":{"t::stranger":{"outcome":"FAILED"}}})),
            "KILLED",
        );
        assert_eq!(misattributed["status"], "MISATTRIBUTED");
        assert_eq!(misattributed["matched"], json!([]));
        assert_eq!(misattributed["collateral"], json!(["t::stranger"]));

        let unranked = verdict(
            py,
            &["t::declared"],
            Some(
                json!({"status":"COMPLETE","tests":{"t::declared":{"outcome":"FAILED"}},
                "unranked_failures":["other::a","other::b"]}),
            ),
            "KILLED",
        );
        assert_eq!(
            unranked["unranked_failures"],
            json!(["other::a", "other::b"])
        );
    });
}

#[test]
fn unavailable_incomplete_and_survived_runs_keep_distinct_verdicts() {
    let _case = Case::new();
    Python::attach(|py| {
        assert_eq!(
            verdict(py, &["t::a"], None, "SURVIVED"),
            json!({"status":"NOT_APPLICABLE","declared":["t::a"]})
        );
        let unavailable = verdict(py, &["t::a"], None, "KILLED");
        assert_eq!(unavailable["status"], "UNAVAILABLE");
        assert_eq!(
            unavailable["reason"],
            "campaign batch carries no per-test attribution"
        );
        let incomplete = verdict(
            py,
            &["t::a"],
            Some(json!({"status":"INCOMPLETE","missing_nodeids":["t::a"],"error":"collection"})),
            "KILLED",
        );
        assert_eq!(incomplete["status"], "UNATTRIBUTED");
        assert_eq!(incomplete["reason"], "attribution is INCOMPLETE");
        assert_eq!(incomplete["missing_nodeids"], json!(["t::a"]));
        assert_eq!(incomplete["error"], "collection");
    });
}

#[test]
fn folding_requires_confirmed_kill_and_preserves_actionable_fields() {
    let _case = Case::new();
    Python::attach(|py| {
        let testing = module(py, "conductor.mutation_testing");
        let routine = testing.getattr("_is_routine_kill").unwrap();
        for (outcome, status, expected) in [
            ("KILLED", "CONFIRMED", true),
            ("SURVIVED", "CONFIRMED", false),
            ("KILLED", "MISATTRIBUTED", false),
            ("KILLED", "", false),
        ] {
            let attrs = if status.is_empty() {
                json!({})
            } else {
                json!({"status":status})
            };
            assert_eq!(
                routine
                    .call1((outcome, to_python(py, &attrs)))
                    .unwrap()
                    .extract::<bool>()
                    .unwrap(),
                expected
            );
        }
        let fold = testing.getattr("_attribution_summary").unwrap();
        let summary = from_python(
            &fold
                .call1((to_python(
                    py,
                    &json!({
                        "status":"COMPLETE", "failed_nodeids":["t::a"],
                        "missing_nodeids":["t::b"],"unmapped_cases":["t::c[1]"],
                        "tests":{"t::a":{"outcome":"FAILED"},"t::b":{"outcome":"PASSED"},
                                 "t::d":{"outcome":"PASSED"},"t::e":"not a row"}
                    }),
                ),))
                .unwrap(),
        );
        assert_eq!(summary["status"], "COMPLETE");
        assert_eq!(summary["failed_nodeids"], json!(["t::a"]));
        assert_eq!(summary["missing_nodeids"], json!(["t::b"]));
        assert_eq!(summary["unmapped_cases"], json!(["t::c[1]"]));
        assert_eq!(
            summary["ranked_outcome_counts"],
            json!({"FAILED":1,"PASSED":2})
        );
        assert!(summary.get("tests").is_none());
        let malformed = from_python(
            &fold
                .call1((to_python(
                    py,
                    &json!({
                        "status":"COMPLETE","tests":["t::a"],"failed_nodeids":"t::a"
                    }),
                ),))
                .unwrap(),
        );
        assert_eq!(
            malformed,
            json!({"status":"COMPLETE","ranked_outcome_counts":{}})
        );
    });
}
