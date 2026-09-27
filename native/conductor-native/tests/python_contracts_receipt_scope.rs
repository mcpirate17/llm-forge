#![cfg(feature = "python-compat-tests")]
//! Rust-owned contracts for the anchored historical-test receipt boundary.

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

use comm_support::bind_signature;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyCFunction, PyDict, PyList};
use scope_support::{
    changed_context, probe_source, signature_with_kwargs, test_change, write_snapshot, GateFixture,
    LEGACY, PROBE_PATH,
};
use std::sync::{Arc, Mutex};
use support::{module, Case};

const OTHER_PATH: &str = "research/tests/test_probe_other.py";

fn historical<'py>(
    py: Python<'py>,
    case: &Case,
    labels: &[&str],
) -> (GateFixture, Bound<'py, PyAny>) {
    let inventory = PyDict::new(py);
    inventory.set_item(PROBE_PATH, LEGACY.to_vec()).unwrap();
    let gate = GateFixture::new(py, case, &inventory);
    let ctx = gate.context(py);
    write_snapshot(&ctx, PROBE_PATH, &probe_source(labels));
    (gate, ctx)
}

fn record_calls(py: Python<'_>, gate: &GateFixture) -> Arc<Mutex<Vec<Vec<String>>>> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let shape = signature_with_kwargs(py, &["_registry", "paths"], "_kwargs");
    let callback = PyCFunction::new_closure(py, None, None, {
        let seen = Arc::clone(&seen);
        move |args, kw| {
            let bound = bind_signature(&shape, args, kw)?;
            let paths: Vec<String> = bound.getattr("arguments")?.get_item("paths")?.extract()?;
            seen.lock().unwrap().push(paths.clone());
            let payload = PyDict::new(args.py());
            payload.set_item("status", if paths.is_empty() { "PASS" } else { "FAIL" })?;
            payload.set_item("checked_test_paths", &paths)?;
            payload.set_item("evidence", PyList::empty(args.py()))?;
            let missing = PyList::empty(args.py());
            for item in &paths {
                let row = PyDict::new(args.py());
                row.set_item("path", item)?;
                row.set_item("reason", "no registered campaign ranks this test file")?;
                missing.append(row)?;
            }
            payload.set_item("missing_evidence", missing)?;
            payload.set_item("malformed_receipts", PyList::empty(args.py()))?;
            Ok::<_, PyErr>(payload.unbind())
        }
    })
    .unwrap();
    let mutation = module(py, "conductor.mutation_testing");
    gate.setattr(py, mutation.as_any(), "verify_evidence", callback.as_any());
    seen
}

fn result<'py>(py: Python<'py>, context: Bound<'py, PyAny>) -> Bound<'py, PyAny> {
    module(py, "conductor.candidate_review.checks")
        .getattr("check_mutation_evidence")
        .unwrap()
        .call1((context,))
        .unwrap()
}

fn rules(result: &Bound<'_, PyAny>) -> Vec<String> {
    result
        .getattr("findings")
        .unwrap()
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

fn assert_exempt(result: &Bound<'_, PyAny>, expected: &[&str]) {
    let exempt: Vec<String> = result
        .getattr("metrics")
        .unwrap()
        .get_item("receipt_exempt_tests")
        .unwrap()
        .extract()
        .unwrap();
    assert_eq!(exempt, expected);
}

#[test]
fn a_touched_historical_test_file_needs_no_campaign() {
    let case = Case::new();
    Python::attach(|py| {
        let (gate, ctx) = historical(py, &case, &LEGACY);
        let seen = record_calls(py, &gate);
        let checked = result(py, ctx);
        assert!(rules(&checked).is_empty());
        assert!(checked.getattr("status").unwrap().is(module(
            py,
            "conductor.candidate_review.model"
        )
        .getattr("CheckStatus")
        .unwrap()
        .getattr("PASSED")
        .unwrap()));
        assert_eq!(*seen.lock().unwrap(), [Vec::<String>::new()]);
        assert_exempt(&checked, &[PROBE_PATH]);
    });
}

#[test]
fn a_new_test_definition_still_demands_a_receipt() {
    let case = Case::new();
    Python::attach(|py| {
        let (gate, ctx) = historical(py, &case, &[LEGACY[0], LEGACY[1], "test_probe_new"]);
        let seen = record_calls(py, &gate);
        let checked = result(py, ctx);
        assert_eq!(
            rules(&checked),
            ["missing-mutation-receipt", "new-test-value-not-admitted"]
        );
        assert!(checked.getattr("status").unwrap().is(module(
            py,
            "conductor.candidate_review.model"
        )
        .getattr("CheckStatus")
        .unwrap()
        .getattr("FAILED")
        .unwrap()));
        assert_eq!(*seen.lock().unwrap(), [vec![PROBE_PATH.to_owned()]]);
        assert_exempt(&checked, &[]);
    });
}

#[test]
fn an_unreadable_inventory_fails_closed() {
    let case = Case::new();
    Python::attach(|py| {
        let (gate, ctx) = historical(py, &case, &LEGACY);
        let seen = record_calls(py, &gate);
        let verification = module(py, "conductor.candidate_review.verification");
        gate.setattr(
            py,
            verification.as_any(),
            "GRANDFATHER_INVENTORY_SHA256",
            "0".repeat(64).into_pyobject(py).unwrap().as_any(),
        );
        let checked = result(py, ctx);
        assert_eq!(
            rules(&checked),
            ["missing-mutation-receipt", "grandfather-inventory-invalid"]
        );
        assert_eq!(*seen.lock().unwrap(), [vec![PROBE_PATH.to_owned()]]);
        assert_exempt(&checked, &[]);
    });
}

#[test]
fn exempt_and_gated_files_are_separated_in_one_candidate() {
    let case = Case::new();
    Python::attach(|py| {
        let inventory = PyDict::new(py);
        inventory.set_item(PROBE_PATH, LEGACY.to_vec()).unwrap();
        inventory.set_item(OTHER_PATH, LEGACY.to_vec()).unwrap();
        let gate = GateFixture::new(py, &case, &inventory);
        let ctx = gate.context(py);
        write_snapshot(&ctx, PROBE_PATH, &probe_source(&LEGACY));
        write_snapshot(
            &ctx,
            OTHER_PATH,
            &probe_source(&[LEGACY[0], LEGACY[1], "test_probe_new"]),
        );
        let seen = record_calls(py, &gate);
        let changed = changed_context(
            py,
            &ctx,
            &[test_change(py, PROBE_PATH), test_change(py, OTHER_PATH)],
        );
        let checked = result(py, changed);
        assert_eq!(*seen.lock().unwrap(), [vec![OTHER_PATH.to_owned()]]);
        assert_eq!(
            rules(&checked),
            ["missing-mutation-receipt", "new-test-value-not-admitted"]
        );
        let finding_paths: Vec<Option<String>> = checked
            .getattr("findings")
            .unwrap()
            .try_iter()
            .unwrap()
            .map(|finding| finding.unwrap().getattr("path").unwrap().extract().unwrap())
            .collect();
        assert_eq!(
            finding_paths,
            [Some(OTHER_PATH.to_owned()), Some(OTHER_PATH.to_owned())]
        );
        assert_exempt(&checked, &[PROBE_PATH]);
    });
}

fn required_paths(py: Python<'_>, paths: &[&str], gated: &Bound<'_, PyAny>) -> Vec<String> {
    module(py, "conductor.candidate_review.verification")
        .getattr("_receipt_required_paths")
        .unwrap()
        .call1((paths.to_vec(), gated))
        .unwrap()
        .extract()
        .unwrap()
}

#[test]
fn only_paths_with_gated_nodeids_are_required() {
    let _case = Case::new();
    Python::attach(|py| {
        let gated = PyDict::new(py);
        gated
            .set_item("a/test_new.py", ["a/test_new.py::test_x"])
            .unwrap();
        assert_eq!(
            required_paths(py, &["a/test_new.py", "b/test_old.py"], gated.as_any()),
            ["a/test_new.py"]
        );
    });
}

#[test]
fn an_unevaluable_inventory_requires_every_changed_test() {
    let _case = Case::new();
    Python::attach(|py| {
        let paths = ["a/test_new.py", "b/test_old.py"];
        assert_eq!(required_paths(py, &paths, py.None().bind(py)), paths);
    });
}

#[test]
fn an_empty_nodeid_tuple_does_not_gate_a_path() {
    let _case = Case::new();
    Python::attach(|py| {
        let gated = PyDict::new(py);
        gated
            .set_item("b/test_old.py", pyo3::types::PyTuple::empty(py))
            .unwrap();
        assert!(required_paths(py, &["b/test_old.py"], gated.as_any()).is_empty());
    });
}
