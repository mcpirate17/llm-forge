#![cfg(feature = "python-compat-tests")]
//! Collector failures and stale reports at the remaining Python process boundary.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::prelude::*;
use pyo3::types::{PyCFunction, PyDict};
use support::{module, path, text, Case};

const PYTEST_JUNIT: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<testsuites><testsuite tests="3">
  <testcase classname="conductor.test_mutation_value" name="test_alpha[a]" time="0.2" />
  <testcase classname="conductor.test_mutation_value" name="test_alpha[b]" time="0.3"><failure /></testcase>
  <testcase classname="conductor.test_mutation_value.TestGroup" name="test_beta" time="0.1" />
</testsuite></testsuites>"#;

const CTEST_JUNIT: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<testsuite name="(empty)" tests="4" failures="1" disabled="1" skipped="0">
  <testcase name="test_profiler.test_memory_events" classname="c" time="0.03" status="run"/>
  <testcase name="test_profiler.test_reset_clears_all" classname="c" time="0.02" status="fail"><failure message="Failed"/></testcase>
  <testcase name="test_profiler.test_clock_ns_monotonic" classname="c" time="0" status="disabled"/>
  <testcase name="test_kernels.test_relu" classname="c" time="0.01" status="fail"><failure message="Failed"/></testcase>
</testsuite>"#;

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

#[test]
fn junit_attribution_maps_parameterized_failures_and_incomplete_reports() {
    let case = Case::new();
    let report = case.write("report.xml", PYTEST_JUNIT);
    let alpha = "conductor/test_mutation_value.py::test_alpha";
    let beta = "conductor/test_mutation_value.py::TestGroup::test_beta";
    Python::attach(|py| {
        let value = module(py, "conductor.mutation_value");
        let parse = value.getattr("parse_pytest_junit").unwrap();
        let parsed = parse.call1((path(py, &report), vec![alpha, beta])).unwrap();
        assert_eq!(text(&parsed.get_item("status").unwrap()), "COMPLETE");
        let tests = parsed.get_item("tests").unwrap();
        let a = tests.get_item(alpha).unwrap();
        assert_eq!(text(&a.get_item("outcome").unwrap()), "FAILED");
        assert_eq!(
            a.get_item("duration_seconds")
                .unwrap()
                .extract::<f64>()
                .unwrap(),
            0.5
        );
        assert_eq!(a.get_item("cases").unwrap().extract::<usize>().unwrap(), 2);
        assert_eq!(
            text(&tests.get_item(beta).unwrap().get_item("outcome").unwrap()),
            "PASSED"
        );
        let incomplete = parse
            .call1((path(py, &report), vec![alpha, beta, "x.py::test_x"]))
            .unwrap();
        assert_eq!(text(&incomplete.get_item("status").unwrap()), "INCOMPLETE");
        assert_eq!(
            incomplete
                .get_item("missing_nodeids")
                .unwrap()
                .extract::<Vec<String>>()
                .unwrap(),
            vec!["x.py::test_x"]
        );
        let argv = value
            .getattr("pytest_junit_argv")
            .unwrap()
            .call1((vec!["pytest"], path(py, &report)))
            .unwrap();
        assert!(text(&argv.get_item(1).unwrap()).starts_with("--junitxml="));
        let err_class = value.getattr("ValueEvidenceError").unwrap();
        support::assert_error(
            py,
            value
                .getattr("pytest_junit_argv")
                .unwrap()
                .call1((vec!["pytest", "--junitxml=old.xml"], path(py, &report)))
                .unwrap_err(),
            &err_class,
            "must not set",
        );
        support::assert_error(
            py,
            parse
                .call1((path(py, &report), vec!["native_test.cpp"]))
                .unwrap_err(),
            &err_class,
            "Python nodeid",
        );
        support::assert_error(
            py,
            parse
                .call1((
                    path(py, &case.root().join("missing.xml")),
                    vec![alpha, beta],
                ))
                .unwrap_err(),
            &err_class,
            "cannot parse",
        );
    });
}

#[test]
fn junit_error_and_unmapped_edge_cases_fail_closed() {
    let case = Case::new();
    let report = case.write("report.xml", r#"<testsuite>
<testcase classname="conductor.test_mutation_value" name="test_edge[a]" time="bad"><skipped /></testcase>
<testcase classname="conductor.test_mutation_value" name="test_edge[b]" time="0"><error /></testcase>
<testcase classname="unmapped" name="test_other" time="0" />
</testsuite>"#);
    Python::attach(|py| {
        let value = module(py, "conductor.mutation_value");
        let edge = value
            .getattr("parse_pytest_junit")
            .unwrap()
            .call1((
                path(py, &report),
                vec!["conductor/test_mutation_value.py::test_edge"],
            ))
            .unwrap();
        assert_eq!(text(&edge.get_item("status").unwrap()), "INCOMPLETE");
        assert_eq!(
            text(
                &edge
                    .get_item("tests")
                    .unwrap()
                    .get_item("conductor/test_mutation_value.py::test_edge")
                    .unwrap()
                    .get_item("outcome")
                    .unwrap()
            ),
            "ERROR"
        );
    });
}

#[test]
fn pytest_attribution_support_rejects_unmappable_batches() {
    let _case = Case::new();
    Python::attach(|py| {
        let value = module(py, "conductor.mutation_value");
        let supports = value.getattr("pytest_attribution_supported").unwrap();
        let node = "conductor/test_mutation_value.py::test_alpha";
        for (argv, ranked, expected) in [
            (vec!["pytest"], vec![node], true),
            (vec!["pytest"], vec![], false),
            (vec!["pytest", "--junitxml=existing.xml"], vec![node], false),
            (vec!["pytest"], vec!["native_test.cpp::test_alpha"], false),
        ] {
            assert_eq!(
                supports
                    .call1((argv, ranked))
                    .unwrap()
                    .extract::<bool>()
                    .unwrap(),
                expected
            );
        }
    });
}

#[test]
fn ctest_report_separates_declared_killer_from_collateral() {
    let case = Case::new();
    let report_path = case.write("ctest.xml", CTEST_JUNIT);
    let memory = "research/runtime/native/tests/test_profiler.c::test_memory_events";
    let reset = "research/runtime/native/tests/test_profiler.c::test_reset_clears_all";
    let clock = "research/runtime/native/tests/test_profiler.c::test_clock_ns_monotonic";
    Python::attach(|py| {
        let value = module(py, "conductor.mutation_value");
        let report = value
            .getattr("parse_ctest_junit")
            .unwrap()
            .call1((path(py, &report_path), vec![memory, reset, clock]))
            .unwrap();
        assert_eq!(text(&report.get_item("status").unwrap()), "COMPLETE");
        let tests = report.get_item("tests").unwrap();
        for (node, outcome) in [(memory, "PASSED"), (reset, "FAILED"), (clock, "SKIPPED")] {
            assert_eq!(
                text(&tests.get_item(node).unwrap().get_item("outcome").unwrap()),
                outcome
            );
        }
        assert_eq!(
            tests
                .get_item(memory)
                .unwrap()
                .get_item("duration_seconds")
                .unwrap()
                .extract::<f64>()
                .unwrap(),
            0.03
        );
        assert_eq!(
            report
                .get_item("failed_nodeids")
                .unwrap()
                .extract::<Vec<String>>()
                .unwrap(),
            vec![reset]
        );
        assert_eq!(
            report
                .get_item("unranked_failures")
                .unwrap()
                .extract::<Vec<String>>()
                .unwrap(),
            vec!["test_kernels.test_relu"]
        );
        let fields = PyDict::new(py);
        fields.set_item("expected_killers", vec![reset]).unwrap();
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
        assert_eq!(text(&verdict.get_item("status").unwrap()), "CONFIRMED");
        assert_eq!(
            verdict
                .get_item("matched")
                .unwrap()
                .extract::<Vec<String>>()
                .unwrap(),
            vec![reset]
        );
        let absent = "research/runtime/native/tests/test_profiler.c::test_never_registered";
        let partial = value
            .getattr("parse_ctest_junit")
            .unwrap()
            .call1((path(py, &report_path), vec![reset, absent]))
            .unwrap();
        assert_eq!(text(&partial.get_item("status").unwrap()), "INCOMPLETE");
        assert_eq!(
            partial
                .get_item("missing_nodeids")
                .unwrap()
                .extract::<Vec<String>>()
                .unwrap(),
            vec![absent]
        );
    });
}
