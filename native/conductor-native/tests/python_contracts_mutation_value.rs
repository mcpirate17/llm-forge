#![cfg(feature = "python-compat-tests")]
//! Collector failures and stale reports at the remaining Python process boundary.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::prelude::*;
use pyo3::types::{PyCFunction, PyDict};
use support::{module, path, text, Case};

fn check_missing_batch(adapter: &str, stale: bool) {
    let case = Case::new();
    let nodeid = if adapter == "pytest" {
        "conductor/test_mutation_value.py::test_alpha"
    } else {
        "research/runtime/native/tests/test_profiler.c::test_reset_clears_all"
    };
    let report_path = case.root().join("missing/report.xml");
    if stale {
        case.write("missing/report.xml", r#"<testsuite><testcase name="test_profiler.test_reset_clears_all" classname="c" time="0.1"><failure message="old failure"/></testcase></testsuite>"#);
    }
    Python::attach(|py| {
        let value = module(py, "conductor.mutation_value");
        let runner =
            PyCFunction::new_closure(py, None, None, |_, _| Ok::<_, PyErr>("ran")).unwrap();
        let kwargs = PyDict::new(py);
        kwargs.set_item("argv", (adapter,)).unwrap();
        kwargs
            .set_item("report_path", path(py, &report_path))
            .unwrap();
        kwargs.set_item("ranked_nodeids", vec![nodeid]).unwrap();
        kwargs.set_item("run_command", runner).unwrap();
        let result = value
            .getattr(format!("collect_{adapter}_junit_batch"))
            .unwrap()
            .call((), Some(&kwargs))
            .unwrap();
        assert_eq!(text(&result.get_item(0).unwrap()), "ran");
        let report = result.get_item(1).unwrap();
        assert_eq!(text(&report.get_item("status").unwrap()), "INCOMPLETE");
        for field in ["tests", "failed_nodeids", "unmapped_cases"] {
            assert_eq!(report.get_item(field).unwrap().len().unwrap(), 0, "{field}");
        }
        assert_eq!(
            report
                .get_item("missing_nodeids")
                .unwrap()
                .extract::<Vec<String>>()
                .unwrap(),
            vec![nodeid]
        );
        assert!(!text(&report.get_item("error").unwrap()).is_empty());
        if adapter == "ctest" {
            assert_eq!(
                report.get_item("unranked_failures").unwrap().len().unwrap(),
                0
            );
        }
        if stale {
            assert!(!report_path.exists());
            assert!(text(&report.get_item("error").unwrap())
                .contains("cannot parse ctest JUnit report"));
            let fields = PyDict::new(py);
            fields.set_item("expected_killers", vec![nodeid]).unwrap();
            let mutation = module(py, "types")
                .getattr("SimpleNamespace")
                .unwrap()
                .call((), Some(&fields))
                .unwrap();
            let verdict = module(py, "conductor.mutation_testing")
                .getattr("killer_verdict")
                .unwrap()
                .call1((mutation, report, "KILLED"))
                .unwrap();
            assert_eq!(text(&verdict.get_item("status").unwrap()), "UNATTRIBUTED");
        }
    });
}

#[test]
fn pytest_and_ctest_collectors_report_all_missing_evidence_fields() {
    for adapter in ["pytest", "ctest"] {
        check_missing_batch(adapter, false);
    }
}

#[test]
fn unrun_ctest_batch_cannot_inherit_a_previous_mutants_report() {
    check_missing_batch("ctest", true);
}
