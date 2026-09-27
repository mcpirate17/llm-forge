#![cfg(feature = "python-compat-tests")]
//! Rust-owned PyO3 contracts for the native branch-policy adapter boundary.

#[path = "python_contracts/branch_policy_native_support.rs"]
mod branch_support;
#[path = "python_contracts/agent_comm_support.rs"]
#[allow(dead_code)]
mod comm_support;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use branch_support::{binding, bp, minus, store, store_patch, store_path, utc_now};
use comm_support::{bind_signature, py_json, signature};
use pyo3::exceptions::{PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyBool, PyCFunction, PyDict, PyList, PyString, PyTuple};
use serde_json::json;
use std::fs;
use support::{assert_error, path, AttrPatch, Case};

fn policy_class(py: Python<'_>) -> Bound<'_, PyAny> {
    bp(py).getattr("BranchPolicyError").unwrap()
}

fn name_callback<F>(py: Python<'_>, callback: F) -> Bound<'_, PyCFunction>
where
    F: Fn(&Bound<'_, PyAny>, &Bound<'_, PyAny>) -> PyResult<Py<PyAny>> + Send + Sync + 'static,
{
    let expected = signature(py, &["name", "today"], &[]);
    PyCFunction::new_closure(py, None, None, move |args, kwargs| {
        let bound = bind_signature(&expected, args, kwargs)?;
        let values = bound.getattr("arguments")?;
        callback(&values.get_item("name")?, &values.get_item("today")?)
    })
    .unwrap()
}

#[test]
fn valueerror_from_rust_maps_to_policy_error() {
    let _case = Case::new();
    Python::attach(|py| {
        let callback = name_callback(py, |_name, _today| -> PyResult<Py<PyAny>> {
            Err(PyValueError::new_err("no 'agent/' segment"))
        });
        let _patch = AttrPatch::replace(
            bp(py).as_any(),
            "branch_policy_validate_name_native",
            callback.as_any(),
        );
        let error = bp(py)
            .getattr("validate_branch_name")
            .unwrap()
            .call1(("nope",))
            .unwrap_err();
        assert_error(py, error, &policy_class(py), "no 'agent/' segment");
    });
}

#[test]
fn fields_returned_by_rust_become_parsed_branch() {
    let _case = Case::new();
    Python::attach(|py| {
        let callback = name_callback(py, |name, _today| -> PyResult<Py<PyAny>> {
            let py = name.py();
            Ok(PyTuple::new(
                py,
                [
                    name.clone().unbind(),
                    PyString::new(py, "agent").into_any().unbind(),
                    PyString::new(py, "topic").into_any().unbind(),
                    PyString::new(py, "20260101").into_any().unbind(),
                ],
            )?
            .into_any()
            .unbind())
        });
        let _patch = AttrPatch::replace(
            bp(py).as_any(),
            "branch_policy_validate_name_native",
            callback.as_any(),
        );
        let actual = bp(py)
            .getattr("validate_branch_name")
            .unwrap()
            .call1(("agent/topic-20260101",))
            .unwrap();
        let expected = bp(py)
            .getattr("ParsedBranch")
            .unwrap()
            .call1(("agent/topic-20260101", "agent", "topic", "20260101"))
            .unwrap();
        assert!(actual.eq(expected).unwrap());
    });
}

#[test]
fn today_flows_from_clock_to_rust() {
    let _case = Case::new();
    Python::attach(|py| {
        let seen = PyList::empty(py);
        let captured = seen.clone().unbind();
        let callback = name_callback(py, move |name, today| -> PyResult<Py<PyAny>> {
            captured.bind(name.py()).append(today)?;
            let py = name.py();
            Ok(PyTuple::new(
                py,
                [
                    name.clone().unbind(),
                    PyString::new(py, "a").into_any().unbind(),
                    PyString::new(py, "t").into_any().unbind(),
                    today.clone().unbind(),
                ],
            )?
            .into_any()
            .unbind())
        });
        let _patch = AttrPatch::replace(
            bp(py).as_any(),
            "branch_policy_validate_name_native",
            callback.as_any(),
        );
        bp(py)
            .getattr("validate_branch_name")
            .unwrap()
            .call1(("a/t-20260101",))
            .unwrap();
        assert_eq!(seen.len(), 1);
        let today: String = utc_now(py)
            .call_method1("strftime", ("%Y%m%d",))
            .unwrap()
            .extract()
            .unwrap();
        assert!(seen.get_item(0).unwrap().eq(today).unwrap());
    });
}

fn stale<'py>(
    py: Python<'py>,
    created: &str,
    pushed: Option<&str>,
    now: &Bound<'py, PyAny>,
) -> PyResult<Bound<'py, PyAny>> {
    let kwargs = PyDict::new(py);
    kwargs.set_item("now", now).unwrap();
    bp(py)
        .getattr("binding_is_stale")
        .unwrap()
        .call((binding(py, created, pushed),), Some(&kwargs))
}

#[test]
fn exactly_stale_push_hours_is_not_stale() {
    let _case = Case::new();
    Python::attach(|py| {
        let now = utc_now(py);
        let created: String = minus(py, &now, &[("hours", 6)])
            .call_method0("isoformat")
            .unwrap()
            .extract()
            .unwrap();
        assert!(stale(py, &created, None, &now)
            .unwrap()
            .is(PyBool::new(py, false)));
    });
}

#[test]
fn one_microsecond_past_the_boundary_is_stale() {
    let _case = Case::new();
    Python::attach(|py| {
        let now = utc_now(py);
        let created: String = minus(py, &now, &[("hours", 6), ("microseconds", 1)])
            .call_method0("isoformat")
            .unwrap()
            .extract()
            .unwrap();
        assert!(stale(py, &created, None, &now)
            .unwrap()
            .is(PyBool::new(py, true)));
    });
}

#[test]
fn recent_push_rescues_an_old_binding() {
    let _case = Case::new();
    Python::attach(|py| {
        let now = utc_now(py);
        let created: String = minus(py, &now, &[("hours", 48)])
            .call_method0("isoformat")
            .unwrap()
            .extract()
            .unwrap();
        let pushed: String = minus(py, &now, &[("minutes", 5)])
            .call_method0("isoformat")
            .unwrap()
            .extract()
            .unwrap();
        assert!(stale(py, &created, Some(&pushed), &now)
            .unwrap()
            .is(PyBool::new(py, false)));
    });
}

#[test]
fn naive_aware_mix_propagates_typeerror() {
    let _case = Case::new();
    Python::attach(|py| {
        let now = utc_now(py);
        let error = stale(py, "2026-01-01T00:00:00", None, &now).unwrap_err();
        assert!(error.matches(py, &py.get_type::<PyTypeError>()).unwrap());
    });
}

#[test]
fn roundtrip_preserves_every_field() {
    let case = Case::new();
    Python::attach(|py| {
        let _patch = store_patch(py, case.root());
        let now: String = utc_now(py)
            .call_method0("isoformat")
            .unwrap()
            .extract()
            .unwrap();
        store(
            py,
            case.root(),
            json!({"schema_version":1,"bindings":[{
                "branch":"codex/topic-20260101","claim_id":"c9","owner":"codex",
                "created_at":now,"last_push_at":now,"pr_number":42
            }]}),
        );
        let loaded = bp(py)
            .getattr("load_bindings")
            .unwrap()
            .call1((path(py, case.root()),))
            .unwrap();
        let expected = bp(py)
            .getattr("BranchBinding")
            .unwrap()
            .call1(("codex/topic-20260101", "c9", "codex", &now, &now, 42))
            .unwrap();
        assert!(loaded.eq((expected,).into_pyobject(py).unwrap()).unwrap());
    });
}

#[test]
fn missing_store_is_empty_tuple() {
    let case = Case::new();
    Python::attach(|py| {
        let _patch = store_patch(py, case.root());
        let loaded = bp(py)
            .getattr("load_bindings")
            .unwrap()
            .call1((path(py, case.root()),))
            .unwrap();
        assert!(loaded.eq(().into_pyobject(py).unwrap()).unwrap());
    });
}

fn duplicate_row() -> serde_json::Value {
    json!({"branch":"claude/topic-20260101","claim_id":"c1","owner":"claude",
        "created_at":"2026-01-01T00:00:00+00:00","last_push_at":null,"pr_number":null})
}

#[test]
fn duplicate_branches_refused() {
    let case = Case::new();
    Python::attach(|py| {
        let _patch = store_patch(py, case.root());
        let row = duplicate_row();
        store(
            py,
            case.root(),
            json!({"schema_version":1,"bindings":[row,row]}),
        );
        let error = bp(py)
            .getattr("load_bindings")
            .unwrap()
            .call1((path(py, case.root()),))
            .unwrap_err();
        assert_error(py, error, &policy_class(py), "duplicate branches");
    });
}

#[test]
fn wrong_schema_version_refused() {
    let case = Case::new();
    Python::attach(|py| {
        let _patch = store_patch(py, case.root());
        store(py, case.root(), json!({"schema_version":2,"bindings":[]}));
        let error = bp(py)
            .getattr("load_bindings")
            .unwrap()
            .call1((path(py, case.root()),))
            .unwrap_err();
        assert_error(py, error, &policy_class(py), "schema version");
    });
}

#[test]
fn unreadable_bytes_refused_as_unreadable() {
    let case = Case::new();
    Python::attach(|py| {
        let _patch = store_patch(py, case.root());
        let location = store_path(py, case.root());
        fs::create_dir_all(location.parent().unwrap()).unwrap();
        fs::write(location, b"\xff\xfe\x9c").unwrap();
        let error = bp(py)
            .getattr("load_bindings")
            .unwrap()
            .call1((path(py, case.root()),))
            .unwrap_err();
        assert_error(py, error, &policy_class(py), "unreadable");
    });
}

#[test]
fn bind_conflict_facts_passed_to_rust() {
    let case = Case::new();
    Python::attach(|py| {
        let _store = store_patch(py, case.root());
        let bp = bp(py);
        let seen = PyDict::new(py);
        let captured = seen.clone().unbind();
        let conflict_signature = signature(py, &["live", "store_text", "branch", "claim_id"], &[]);
        let conflict =
            PyCFunction::new_closure(py, None, None, move |args, kwargs| -> PyResult<String> {
                let bound = bind_signature(&conflict_signature, args, kwargs)?;
                let values = bound.getattr("arguments")?;
                let seen = captured.bind(args.py());
                for key in ["live", "store_text", "branch", "claim_id"] {
                    seen.set_item(key, values.get_item(key)?)?;
                }
                Ok("conflict!".to_owned())
            })
            .unwrap();
        let _conflict = AttrPatch::replace(
            bp.as_any(),
            "branch_policy_second_branch_conflict_native",
            conflict.as_any(),
        );
        let refs_signature = signature(py, &["repo", "pattern"], &[]);
        let refs = PyCFunction::new_closure(
            py,
            None,
            None,
            move |args, kwargs| -> PyResult<Vec<String>> {
                bind_signature(&refs_signature, args, kwargs)?;
                Ok(vec!["refs/heads/a/b-1".to_owned()])
            },
        )
        .unwrap();
        let _refs = AttrPatch::replace(bp.as_any(), "_refs", refs.as_any());
        let kwargs = PyDict::new(py);
        kwargs.set_item("branch", "claude/t-20260101").unwrap();
        kwargs.set_item("claim_id", "c1").unwrap();
        kwargs.set_item("owner", "claude").unwrap();
        let error = bp
            .getattr("bind_branch")
            .unwrap()
            .call((path(py, case.root()),), Some(&kwargs))
            .unwrap_err();
        assert_error(py, error, &policy_class(py), "conflict!");
        let expected = py_json(
            py,
            json!({"live":["a/b-1"],"store_text":null,
            "branch":"claude/t-20260101","claim_id":"c1"}),
        );
        assert!(seen.eq(expected).unwrap());
    });
}
