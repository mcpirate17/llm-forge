#![cfg(feature = "python-compat-tests")]
//! Rust-owned contracts for crate-local test selection and Python boundary findings.

#[path = "python_contracts/agent_comm_support.rs"]
#[allow(dead_code)]
mod comm_support;
#[path = "python_contracts/git_fixture_support.rs"]
#[allow(dead_code)]
mod git_fixture_support;
#[path = "python_contracts/scope_contract_support.rs"]
#[allow(dead_code)]
mod scope_support;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use comm_support::{bind_signature, signature};
use pyo3::prelude::*;
use pyo3::types::{PyCFunction, PyDict, PySet, PyTuple};
use scope_support::{
    changed_context, native_change, python_change, write_snapshot, GateFixture, CRATE, SOURCE,
    TESTED, UNTESTED,
};
use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use support::{module, Case};

fn crate_fixture<'py>(py: Python<'py>, case: &Case, lib: &str) -> (GateFixture, Bound<'py, PyAny>) {
    let gate = GateFixture::new(py, case, &PyDict::new(py));
    let ctx = gate.context(py);
    write_snapshot(
        &ctx,
        &format!("{CRATE}/Cargo.toml"),
        "[package]\nname = \"probe\"\n",
    );
    write_snapshot(&ctx, SOURCE, lib);
    (gate, ctx)
}

fn select_with_no_python_tests<'py>(
    py: Python<'py>,
    gate: &GateFixture,
    context: &Bound<'py, PyAny>,
    changes: &[Bound<'py, PyAny>],
) -> Bound<'py, PyAny> {
    let verification = module(py, "conductor.candidate_review.verification");
    let graph_shape = signature(py, &["_ctx", "_sources"], &[]);
    let graph = PyCFunction::new_closure(py, None, None, move |args, kw| {
        bind_signature(&graph_shape, args, kw)?;
        let py = args.py();
        let info = PyDict::new(py);
        info.set_item("status", "stubbed")?;
        info.set_item("selected_edges", 0)?;
        Ok::<_, PyErr>(PyTuple::new(py, [PySet::empty(py)?.as_any(), info.as_any()])?.unbind())
    })
    .unwrap();
    gate.setattr(
        py,
        verification.as_any(),
        "_graph_test_paths",
        graph.as_any(),
    );
    let convention_shape = signature(py, &["_ctx", "_sources"], &[]);
    let convention = PyCFunction::new_closure(py, None, None, move |args, kw| {
        bind_signature(&convention_shape, args, kw)?;
        Ok::<_, PyErr>(PySet::empty(args.py())?.unbind())
    })
    .unwrap();
    gate.setattr(
        py,
        verification.as_any(),
        "_convention_tests",
        convention.as_any(),
    );
    let selected = changed_context(py, context, changes);
    verification
        .getattr("select_tests")
        .unwrap()
        .call1((selected,))
        .unwrap()
}

fn rule_ids(findings: &Bound<'_, PyAny>) -> Vec<String> {
    findings
        .try_iter()
        .unwrap()
        .map(|finding| {
            finding
                .unwrap()
                .getattr("rule_id")
                .unwrap()
                .extract()
                .unwrap()
        })
        .collect()
}

#[test]
fn an_inline_test_module_counts_as_the_crates_tests() {
    let case = Case::new();
    Python::attach(|py| {
        let (_gate, ctx) = crate_fixture(py, &case, TESTED);
        let covered = module(py, "conductor.candidate_review.graph_selection")
            .getattr("_rust_crate_tests")
            .unwrap()
            .call1((ctx, vec![SOURCE]))
            .unwrap();
        let expected = PyDict::new(py);
        expected
            .set_item(SOURCE, PyTuple::new(py, [SOURCE]).unwrap())
            .unwrap();
        assert!(covered.eq(expected).unwrap());
    });
}

#[test]
fn an_integration_test_file_counts_without_the_marker() {
    let case = Case::new();
    Python::attach(|py| {
        let (_gate, ctx) = crate_fixture(py, &case, UNTESTED);
        let integration = format!("{CRATE}/tests/integration.rs");
        write_snapshot(&ctx, &integration, "#[test]\nfn works() {}\n");
        let covered = module(py, "conductor.candidate_review.graph_selection")
            .getattr("_rust_crate_tests")
            .unwrap()
            .call1((ctx, vec![SOURCE]))
            .unwrap();
        let expected = PyDict::new(py);
        expected
            .set_item(SOURCE, PyTuple::new(py, [integration]).unwrap())
            .unwrap();
        assert!(covered.eq(expected).unwrap());
    });
}

#[test]
fn build_output_is_not_evidence() {
    let case = Case::new();
    Python::attach(|py| {
        let (_gate, ctx) = crate_fixture(py, &case, UNTESTED);
        write_snapshot(
            &ctx,
            &format!("{CRATE}/target/debug/build/dep/src/lib.rs"),
            TESTED,
        );
        let covered = module(py, "conductor.candidate_review.graph_selection")
            .getattr("_rust_crate_tests")
            .unwrap()
            .call1((ctx, vec![SOURCE]))
            .unwrap();
        assert!(covered.eq(PyDict::new(py)).unwrap());
    });
}

#[test]
fn a_source_outside_any_crate_is_not_covered() {
    let case = Case::new();
    Python::attach(|py| {
        let (_gate, ctx) = crate_fixture(py, &case, TESTED);
        let loose = "research/scratch/loose.rs";
        write_snapshot(&ctx, loose, TESTED);
        let covered = module(py, "conductor.candidate_review.graph_selection")
            .getattr("_rust_crate_tests")
            .unwrap()
            .call1((ctx, vec![loose]))
            .unwrap();
        assert!(covered.eq(PyDict::new(py)).unwrap());
    });
}

#[test]
fn crate_tests_answer_the_finding_without_being_run() {
    let case = Case::new();
    Python::attach(|py| {
        let (gate, ctx) = crate_fixture(py, &case, TESTED);
        let selected =
            select_with_no_python_tests(py, &gate, &ctx, &[native_change(py, SOURCE, "normal")]);
        assert!(selected
            .getattr("findings")
            .unwrap()
            .eq(PyTuple::empty(py))
            .unwrap());
        assert!(selected
            .getattr("tests")
            .unwrap()
            .eq(PyTuple::empty(py))
            .unwrap());
        let native_count: usize = selected
            .getattr("graph")
            .unwrap()
            .get_item("native_test_files")
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(native_count, 1);
    });
}

#[test]
fn native_mutation_evidence_answers_the_high_risk_gate() {
    let case = Case::new();
    Python::attach(|py| {
        let (gate, ctx) = crate_fixture(py, &case, UNTESTED);
        let native_test = format!("{CRATE}/tests/integration.rs");
        let python_test = "conductor/test_probe_caller.py";
        write_snapshot(&ctx, &native_test, "#[test]\nfn works() {}\n");
        write_snapshot(&ctx, python_test, "def test_caller():\n    assert True\n");
        let verification = module(py, "conductor.candidate_review.verification");
        let graph_shape = signature(py, &["_ctx", "_sources"], &[]);
        let graph = PyCFunction::new_closure(py, None, None, move |args, kw| {
            bind_signature(&graph_shape, args, kw)?;
            let py = args.py();
            let info = PyDict::new(py);
            info.set_item("status", "stubbed")?;
            Ok::<_, PyErr>(
                PyTuple::new(py, [PySet::new(py, [python_test])?.as_any(), info.as_any()])?
                    .unbind(),
            )
        })
        .unwrap();
        gate.setattr(
            py,
            verification.as_any(),
            "_graph_test_paths",
            graph.as_any(),
        );
        let convention_shape = signature(py, &["_ctx", "_sources"], &[]);
        let convention = PyCFunction::new_closure(py, None, None, move |args, kw| {
            bind_signature(&convention_shape, args, kw)?;
            Ok::<_, PyErr>(PySet::empty(args.py())?.unbind())
        })
        .unwrap();
        gate.setattr(
            py,
            verification.as_any(),
            "_convention_tests",
            convention.as_any(),
        );
        let seen = Arc::new(Mutex::new(Vec::<BTreeSet<String>>::new()));
        let evidence_shape = signature(py, &["_ctx", "tests"], &[]);
        let evidence = PyCFunction::new_closure(py, None, None, {
            let seen = Arc::clone(&seen);
            let native_test = native_test.clone();
            move |args, kw| {
                let bound = bind_signature(&evidence_shape, args, kw)?;
                let tests = bound.getattr("arguments")?.get_item("tests")?;
                let paths: BTreeSet<String> = tests
                    .try_iter()?
                    .map(|x| x.unwrap().extract().unwrap())
                    .collect();
                let admitted = paths.contains(&native_test);
                seen.lock().unwrap().push(paths);
                Ok::<_, PyErr>(admitted)
            }
        })
        .unwrap();
        gate.setattr(
            py,
            verification.as_any(),
            "_has_property_evidence",
            evidence.as_any(),
        );
        let selected_ctx = changed_context(py, &ctx, &[native_change(py, SOURCE, "high")]);
        let selected = verification
            .getattr("select_tests")
            .unwrap()
            .call1((selected_ctx,))
            .unwrap();
        assert!(selected
            .getattr("findings")
            .unwrap()
            .eq(PyTuple::empty(py))
            .unwrap());
        assert!(selected
            .getattr("tests")
            .unwrap()
            .eq(PyTuple::new(py, [python_test]).unwrap())
            .unwrap());
        let expected = [python_test.to_owned(), native_test].into_iter().collect();
        assert_eq!(*seen.lock().unwrap(), [expected]);
    });
}

#[test]
fn an_untested_crate_is_still_reported() {
    let case = Case::new();
    Python::attach(|py| {
        let (gate, ctx) = crate_fixture(py, &case, UNTESTED);
        let selected =
            select_with_no_python_tests(py, &gate, &ctx, &[native_change(py, SOURCE, "normal")]);
        let findings = selected.getattr("findings").unwrap();
        assert_eq!(rule_ids(&findings), ["no-targeted-tests"]);
        let expected = PyDict::new(py);
        expected.set_item("source_paths", vec![SOURCE]).unwrap();
        assert!(findings
            .get_item(0)
            .unwrap()
            .getattr("evidence")
            .unwrap()
            .eq(expected)
            .unwrap());
    });
}

#[test]
fn a_covered_crate_does_not_answer_for_python() {
    let case = Case::new();
    Python::attach(|py| {
        let (gate, ctx) = crate_fixture(py, &case, TESTED);
        let python_source = "conductor/probe_module.py";
        let selected = select_with_no_python_tests(
            py,
            &gate,
            &ctx,
            &[
                native_change(py, SOURCE, "normal"),
                python_change(py, python_source),
            ],
        );
        let findings = selected.getattr("findings").unwrap();
        assert_eq!(rule_ids(&findings), ["no-targeted-tests"]);
        let expected = PyDict::new(py);
        expected
            .set_item("source_paths", vec![python_source])
            .unwrap();
        assert!(findings
            .get_item(0)
            .unwrap()
            .getattr("evidence")
            .unwrap()
            .eq(expected)
            .unwrap());
    });
}
