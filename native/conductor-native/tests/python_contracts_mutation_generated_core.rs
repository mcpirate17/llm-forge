#![cfg(feature = "python-compat-tests")]
//! Scope refusal, mutant identity, timeout, and survivor ratchet contracts.

#[path = "python_contracts/agent_comm_support.rs"]
#[allow(dead_code)]
mod comm_support;
#[path = "python_contracts/mutation_generated_fixture.rs"]
#[allow(dead_code)]
mod fixture;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use comm_support::{bind_signature, py_json, signature};
use fixture::{campaign_error, generated, load, manifest, scored, survivors};
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use pyo3::types::{PyBool, PyCFunction, PyDict, PyList, PySet, PyTuple};
use serde_json::{json, Value};
use std::fs;
use std::sync::{Arc, Mutex};
use support::{module, path, AttrPatch, Case};

#[test]
fn direct_run_refuses_unowned_sources_before_resolving_an_engine() {
    let case = Case::new();
    case.write("conductor/gate_rollout.py", "value = 1\n");
    let file = manifest(&case, json!({}));
    Python::attach(|py| {
        let campaign = load(py, &file);
        let runner = generated(py);
        let scope = module(py, "conductor.mutation_run_scope");
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let recorded = Arc::clone(&seen);
        let engine_signature = signature(py, &["name"], &[]);
        let engine = PyCFunction::new_closure(py, None, None, move |args, kw| -> PyResult<()> {
            let bound = bind_signature(&engine_signature, args, kw)?;
            recorded
                .lock()
                .unwrap()
                .push(bound.getattr("arguments")?.get_item("name")?.extract()?);
            Err(PyRuntimeError::new_err("engine reached"))
        })
        .unwrap();
        let _engine = AttrPatch::replace(runner.as_any(), "adapter_for", engine.as_any());
        let forbidden_snapshot =
            PyCFunction::new_closure(py, None, None, |_args, _kw| -> PyResult<()> {
                Err(PyRuntimeError::new_err(
                    "test must refuse before isolated_snapshot",
                ))
            })
            .unwrap();
        let _snapshot = AttrPatch::replace(
            runner.as_any(),
            "isolated_snapshot",
            forbidden_snapshot.as_any(),
        );
        let no_changes = PyCFunction::new_closure(py, None, None, |args, _kw| {
            Ok::<Py<PySet>, pyo3::PyErr>(PySet::empty(args.py())?.unbind())
        })
        .unwrap();
        let changes = AttrPatch::replace(scope.as_any(), "changed_sources", no_changes.as_any());
        let kw = PyDict::new(py);
        kw.set_item("allow_mutations", true).unwrap();
        kw.set_item("repo_root", path(py, case.root())).unwrap();
        campaign_error(
            py,
            runner
                .getattr("run_generated_campaign")
                .unwrap()
                .call((&campaign,), Some(&kw)),
            "outside this agent's changes",
        );
        assert!(seen.lock().unwrap().is_empty());
        drop(changes);
        let owned = PyCFunction::new_closure(py, None, None, |args, _kw| {
            Ok::<Py<PySet>, pyo3::PyErr>(
                PySet::new(args.py(), ["conductor/gate_rollout.py"])?.unbind(),
            )
        })
        .unwrap();
        let _changes = AttrPatch::replace(scope.as_any(), "changed_sources", owned.as_any());
        let error = runner
            .getattr("run_generated_campaign")
            .unwrap()
            .call((campaign,), Some(&kw))
            .unwrap_err();
        assert!(error.matches(py, &py.get_type::<PyRuntimeError>()).unwrap());
        assert!(error.to_string().contains("engine reached"));
        assert_eq!(*seen.lock().unwrap(), ["fest"]);
    });
}

#[test]
fn a_mutant_is_named_by_what_it_does_not_by_where_it_sits() {
    let _case = Case::new();
    Python::attach(|py| {
        let mutant_id = generated(py).getattr("mutant_id").unwrap();
        let here = mutant_id
            .call1(("a.py", "constant_replace", "\"gh\"", "\"\"", 0))
            .unwrap();
        assert!(here
            .eq(mutant_id
                .call1(("a.py", "constant_replace", "\"gh\"", "\"\"", 0))
                .unwrap())
            .unwrap());
        for values in [
            ("b.py", "constant_replace", "\"gh\"", "\"\"", 0),
            ("a.py", "constant_replace", "\"gh\"", "\"gg\"", 0),
            ("a.py", "operator_swap", "\"gh\"", "\"\"", 0),
            ("a.py", "constant_replace", "\"gh\"", "\"\"", 1),
        ] {
            assert!(here.ne(mutant_id.call1(values).unwrap()).unwrap());
        }
    });
}

#[test]
fn identical_rewrites_in_one_file_get_distinct_names() {
    let _case = Case::new();
    Python::attach(|py| {
        let same = PyTuple::new(py, ["a.py", "constant_replace", "\"gh\"", "\"\""]).unwrap();
        let different = PyTuple::new(py, ["b.py", "constant_replace", "\"gh\"", "\"\""]).unwrap();
        let rows = PyList::new(py, [&same, &same, &different]).unwrap();
        let names = generated(py)
            .getattr("identify")
            .unwrap()
            .call1((rows,))
            .unwrap()
            .cast_into::<PyList>()
            .unwrap();
        assert_eq!(
            module(py, "builtins")
                .getattr("set")
                .unwrap()
                .call1((&names,))
                .unwrap()
                .len()
                .unwrap(),
            3
        );
        let id = generated(py).getattr("mutant_id").unwrap();
        let expected = PyList::new(
            py,
            [
                id.call1(("a.py", "constant_replace", "\"gh\"", "\"\"", 0))
                    .unwrap(),
                id.call1(("a.py", "constant_replace", "\"gh\"", "\"\"", 1))
                    .unwrap(),
                id.call1(("b.py", "constant_replace", "\"gh\"", "\"\"", 0))
                    .unwrap(),
            ],
        )
        .unwrap();
        assert!(names.eq(expected).unwrap());
    });
}

#[test]
fn a_run_that_executed_nothing_is_refused() {
    let _case = Case::new();
    Python::attach(|py| {
        let require = generated(py).getattr("require_executed").unwrap();
        campaign_error(py, require.call1((0, 0, vec!["x/*.py"])), "matched nothing");
        campaign_error(
            py,
            require.call1((174, 0, vec!["x/*.py"])),
            "no mutant was executed",
        );
        require.call1((174, 1, vec!["x/*.py"])).unwrap();
    });
}

#[test]
fn the_per_mutant_bound_comes_from_the_manifest_or_the_baseline() {
    let case = Case::new();
    Python::attach(|py| {
        let runner = generated(py);
        let pinned = load(
            py,
            &manifest(
                &case,
                json!({"generator":{
                    "source":["conductor/gate_rollout.py"],"mutant_timeout_seconds":300,"run_timeout_seconds":60
                }}),
            ),
        );
        assert_eq!(
            pinned
                .getattr("mutant_timeout_seconds")
                .unwrap()
                .extract::<i32>()
                .unwrap(),
            300
        );
        assert_eq!(
            runner
                .getattr("resolve_mutant_timeout")
                .unwrap()
                .call1((&pinned, 5.0))
                .unwrap()
                .extract::<i32>()
                .unwrap(),
            300
        );
        assert_eq!(
            pinned
                .getattr("mutant_timeout_seconds")
                .unwrap()
                .extract::<i32>()
                .unwrap(),
            300
        );
        for (baseline, expected) in [(0.5, 60), (41.2, 124), (-7.0, 60)] {
            let campaign = load(py, &manifest(&case, json!({})));
            assert!(campaign
                .getattr("mutant_timeout_seconds")
                .unwrap()
                .is_none());
            let actual: i32 = runner
                .getattr("resolve_mutant_timeout")
                .unwrap()
                .call1((&campaign, baseline))
                .unwrap()
                .extract()
                .unwrap();
            assert_eq!(actual, expected);
            if baseline >= 0.0 {
                assert_eq!(
                    campaign
                        .getattr("mutant_timeout_seconds")
                        .unwrap()
                        .extract::<i32>()
                        .unwrap(),
                    expected
                );
            }
        }
    });
}

fn assert_status(receipt: &Bound<'_, PyDict>, expected: &str) {
    assert!(receipt
        .get_item("status")
        .unwrap()
        .unwrap()
        .eq(expected)
        .unwrap());
}

fn score_value(receipt: &Bound<'_, PyDict>) -> f64 {
    receipt
        .get_item("mutation_score")
        .unwrap()
        .unwrap()
        .extract()
        .unwrap()
}

fn assert_list(py: Python<'_>, receipt: &Bound<'_, PyDict>, key: &str, expected: &[&str]) {
    let list = receipt
        .get_item(key)
        .unwrap()
        .unwrap()
        .cast_into::<PyList>()
        .unwrap();
    assert!(list.eq(PyList::new(py, expected).unwrap()).unwrap());
}

#[test]
fn a_new_survivor_fails_and_a_known_one_only_holds() {
    let case = Case::new();
    Python::attach(|py| {
        let fails = survivors(py, &case, &["a", "b"], &["a"]);
        assert_status(&fails, "FAIL");
        assert_list(py, &fails, "new_survivors", &["b"]);
        assert!((score_value(&fails) - 1.0 / 3.0).abs() <= 1.0e-6 / 3.0);
        assert_eq!(
            fails
                .get_item("outcome_counts")
                .unwrap()
                .unwrap()
                .get_item("SURVIVED")
                .unwrap()
                .extract::<i32>()
                .unwrap(),
            2
        );
        let holds = survivors(py, &case, &["a"], &["a", "z"]);
        assert_status(&holds, "RATCHET_HELD");
        assert_list(py, &holds, "new_survivors", &[]);
        assert_list(py, &holds, "resolved_survivors", &["z"]);
        let clean = survivors(py, &case, &[], &[]);
        assert_status(&clean, "PASS");
        assert_eq!(score_value(&clean), 1.0);
    });
}

#[test]
fn a_timeout_is_never_scored_as_a_kill() {
    let case = Case::new();
    Python::attach(|py| {
        let receipt = scored(py, &case, &[("t", "TIMED_OUT"), ("k", "KILLED")], &[]);
        assert_status(&receipt, "PASS");
        assert_eq!(
            receipt
                .get_item("timed_out")
                .unwrap()
                .unwrap()
                .extract::<i32>()
                .unwrap(),
            1
        );
        assert_eq!(score_value(&receipt), 1.0);
        let survivors = receipt
            .get_item("survivors")
            .unwrap()
            .unwrap()
            .cast_into::<PyList>()
            .unwrap();
        assert!(!survivors.contains("t").unwrap());
        let held = scored(
            py,
            &case,
            &[("t", "TIMED_OUT"), ("s", "SURVIVED"), ("k", "KILLED")],
            &["s"],
        );
        assert_status(&held, "RATCHET_HELD");
        assert_eq!(
            held.get_item("timed_out")
                .unwrap()
                .unwrap()
                .extract::<i32>()
                .unwrap(),
            1
        );
        let regressed = scored(
            py,
            &case,
            &[("t", "TIMED_OUT"), ("s", "SURVIVED"), ("k", "KILLED")],
            &[],
        );
        assert_status(&regressed, "FAIL");
        assert_list(py, &regressed, "new_survivors", &["s"]);
        let errored = scored(py, &case, &[("e", "ERROR"), ("k", "KILLED")], &[]);
        assert_status(&errored, "ERROR");
    });
}

#[test]
fn uncovered_mutants_are_counted_but_never_scored() {
    let case = Case::new();
    Python::attach(|py| {
        let rows = [
            ("n0", "NO_COVERAGE"),
            ("n1", "NO_COVERAGE"),
            ("n2", "NO_COVERAGE"),
            ("k", "KILLED"),
        ];
        let receipt = scored(py, &case, &rows, &[]);
        assert_eq!(
            receipt
                .get_item("no_coverage")
                .unwrap()
                .unwrap()
                .extract::<i32>()
                .unwrap(),
            3
        );
        assert_eq!(score_value(&receipt), 1.0);
        assert_status(&receipt, "PASS");
    });
}

#[test]
fn unviable_mutants_are_counted_but_never_scored() {
    let case = Case::new();
    Python::attach(|py| {
        let ids: Vec<String> = (0..24).map(|index| format!("u{index}")).collect();
        let mut rows: Vec<(&str, &str)> = ids.iter().map(|id| (id.as_str(), "UNVIABLE")).collect();
        rows.push(("k", "KILLED"));
        let receipt = scored(py, &case, &rows, &[]);
        assert_eq!(
            receipt
                .get_item("unviable")
                .unwrap()
                .unwrap()
                .extract::<i32>()
                .unwrap(),
            24
        );
        assert_eq!(score_value(&receipt), 1.0);
        assert_status(&receipt, "PASS");
    });
}

#[test]
fn an_unknown_outcome_is_refused_not_averaged() {
    let case = Case::new();
    Python::attach(|py| {
        let campaign = load(py, &manifest(&case, json!({"survivor_baseline":[]})));
        let receipt = py_json(
            py,
            json!({"mutants":[{"id":"x","outcome":"PROBABLY_FINE"}]}),
        );
        campaign_error(
            py,
            generated(py)
                .getattr("score")
                .unwrap()
                .call1((campaign, receipt)),
            "unknown mutant outcome",
        );
    });
}

#[test]
fn the_first_run_records_its_own_survivor_baseline() {
    let case = Case::new();
    let file = manifest(
        &case,
        json!({"survivor_baseline_note":"Awaiting first measured run"}),
    );
    Python::attach(|py| {
        let runner = generated(py);
        let campaign = load(py, &file);
        assert!(campaign
            .getattr("survivor_baseline_recorded")
            .unwrap()
            .is(PyBool::new(py, false)));
        let receipt = py_json(
            py,
            json!({"mutants":[
                {"id":"a","outcome":"SURVIVED"},{"id":"k","outcome":"KILLED"}
            ]}),
        )
        .cast_into::<PyDict>()
        .unwrap();
        runner
            .getattr("score")
            .unwrap()
            .call1((&campaign, &receipt))
            .unwrap();
        assert_status(&receipt, "FAIL");
        assert!(runner
            .getattr("record_survivor_baseline")
            .unwrap()
            .call1((&campaign, &receipt))
            .unwrap()
            .is(PyBool::new(py, true)));
        assert!(campaign
            .getattr("survivor_baseline_recorded")
            .unwrap()
            .is(PyBool::new(py, true)));
        assert_status(&receipt, "RATCHET_HELD");
        assert_list(py, &receipt, "new_survivors", &[]);
        assert!(receipt
            .get_item("survivor_baseline_recorded_by_this_run")
            .unwrap()
            .unwrap()
            .is(PyBool::new(py, true)));
        let written: Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
        assert_eq!(written["survivor_baseline"], json!(["a"]));
        assert_eq!(written["survivor_baseline_recorded"], true);
        assert!(written.get("survivor_baseline_recorded_at").is_some());
        assert!(written.get("survivor_baseline_note").is_none());
        assert!(receipt
            .get_item("manifest_sha256")
            .unwrap()
            .unwrap()
            .eq(campaign.getattr("manifest_sha256").unwrap())
            .unwrap());
        let again = load(py, &file);
        let second = py_json(py, json!({"mutants":[{"id":"b","outcome":"SURVIVED"}]}))
            .cast_into::<PyDict>()
            .unwrap();
        runner
            .getattr("score")
            .unwrap()
            .call1((&again, &second))
            .unwrap();
        assert!(runner
            .getattr("record_survivor_baseline")
            .unwrap()
            .call1((again, &second))
            .unwrap()
            .is(PyBool::new(py, false)));
        assert_status(&second, "FAIL");
        assert_list(py, &second, "new_survivors", &["b"]);
    });
}

#[test]
fn recorded_flag_and_legacy_survivors_preserve_the_ratchet() {
    let case = Case::new();
    Python::attach(|py| {
        let recorded = load(
            py,
            &manifest(&case, json!({"survivor_baseline_recorded":true})),
        );
        assert!(recorded
            .getattr("survivor_baseline_recorded")
            .unwrap()
            .is(PyBool::new(py, true)));
        let legacy = load(
            py,
            &manifest(&case, json!({"survivor_baseline":["legacy"]})),
        );
        assert!(legacy
            .getattr("survivor_baseline_recorded")
            .unwrap()
            .is(PyBool::new(py, true)));
        let unrecorded = load(
            py,
            &manifest(
                &case,
                json!({"survivor_baseline":["legacy"],"survivor_baseline_recorded":false}),
            ),
        );
        assert!(unrecorded
            .getattr("survivor_baseline_recorded")
            .unwrap()
            .is(PyBool::new(py, false)));
    });
}

#[test]
fn an_errored_run_never_becomes_a_baseline() {
    let case = Case::new();
    let file = manifest(&case, json!({}));
    Python::attach(|py| {
        let campaign = load(py, &file);
        let receipt = py_json(py, json!({"status":"ERROR","survivors":["a","b"]}));
        assert!(generated(py)
            .getattr("record_survivor_baseline")
            .unwrap()
            .call1((&campaign, receipt))
            .unwrap()
            .is(PyBool::new(py, false)));
        assert!(campaign
            .getattr("survivor_baseline_recorded")
            .unwrap()
            .is(PyBool::new(py, false)));
        let written: Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
        assert!(written.get("survivor_baseline").is_none());
    });
}
