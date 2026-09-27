#![cfg(feature = "python-compat-tests")]
//! Rust-owned assertions for session checkout and owner selection.

#[path = "python_contracts/crg_gate_support.rs"]
#[allow(dead_code)]
mod crg;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use crg::{decision, edit_value, payload, GateCase};
use pyo3::exceptions::PyOSError;
use pyo3::prelude::*;
use pyo3::types::{PyCFunction, PyDict, PyList, PyModule, PyTuple};
use serde_json::json;
use std::sync::{Arc, Mutex};
use support::{text, AttrPatch};

fn session_checkout(fixture: &GateCase, value: serde_json::Value) -> String {
    Python::attach(|py| {
        text(
            &fixture
                .gate(py)
                .getattr("session_checkout")
                .unwrap()
                .call1((payload(py, &value),))
                .unwrap(),
        )
    })
}

fn edit_decision(fixture: &GateCase, file: &std::path::Path) -> String {
    Python::attach(|py| {
        decision(
            py,
            &fixture.gate(py),
            "verify",
            &edit_value(file),
            "claude",
            true,
        )
    })
}

#[test]
fn session_checkout_is_the_worktree_the_session_runs_in() {
    let fixture = GateCase::with_worktree();
    assert_eq!(
        session_checkout(&fixture, json!({"cwd": fixture.linked()})),
        fixture.linked().to_str().unwrap()
    );
}

#[test]
fn session_checkout_falls_back_to_the_hook_process_cwd() {
    let fixture = GateCase::with_worktree();
    let _cwd = fixture.case.chdir("linked");
    assert_eq!(
        session_checkout(&fixture, json!({})),
        fixture.linked().to_str().unwrap()
    );
}

#[test]
fn session_checkout_ignores_a_cwd_in_another_repository() {
    let fixture = GateCase::with_worktree();
    let _cwd = fixture.case.chdir("stranger");
    assert_eq!(
        session_checkout(&fixture, json!({"cwd": fixture.stranger()})),
        fixture.repo.to_str().unwrap()
    );
}

#[test]
fn the_lane_is_named_for_the_session_not_for_the_hooks_root() {
    let fixture = GateCase::with_worktree();
    Python::attach(|py| {
        let gate = fixture.gate(py);
        let linked = gate
            .getattr("session_checkout")
            .unwrap()
            .call1((payload(py, &json!({"cwd": fixture.linked()})),))
            .unwrap();
        let root = gate
            .getattr("session_checkout")
            .unwrap()
            .call1((payload(py, &json!({"cwd": fixture.repo})),))
            .unwrap();
        let in_worktree = text(
            &gate
                .getattr("_default_owner")
                .unwrap()
                .call1((linked,))
                .unwrap(),
        );
        let at_root = text(
            &gate
                .getattr("_default_owner")
                .unwrap()
                .call1((root,))
                .unwrap(),
        );
        assert_eq!(in_worktree, "linked");
        assert_ne!(at_root, in_worktree);
    });
}

#[test]
fn a_denial_tells_the_lane_how_to_unblock_itself() {
    let fixture = GateCase::with_worktree();
    let reason = edit_decision(&fixture, &fixture.linked().join("a.py"));
    assert!(reason.contains("make governance-claim"));
    assert!(reason.contains("CRG_GATE_ENFORCE_WORKTREES=0"));
}

#[test]
fn a_local_denial_also_carries_the_remedy() {
    let fixture = GateCase::with_worktree();
    let reason = edit_decision(&fixture, &fixture.repo.join("a.py"));
    assert!(reason.starts_with("BLOCKED:"));
    assert!(reason.contains("make governance-claim"));
}

#[test]
fn a_hook_outside_a_checkout_names_the_root() {
    let fixture = GateCase::with_worktree();
    Python::attach(|py| {
        let none = py.None();
        let _patch = AttrPatch::replace(&fixture.gate(py), "REPO_COMMON_DIR", none.bind(py));
        assert_eq!(
            session_checkout(&fixture, json!({"cwd": fixture.linked()})),
            fixture.repo.to_str().unwrap()
        );
    });
}

#[test]
fn an_unresolvable_cwd_is_not_guessed_at() {
    let fixture = GateCase::with_worktree();
    Python::attach(|py| {
        let stale = PyCFunction::new_closure(py, None, None, |_, _| -> PyResult<()> {
            Err(PyOSError::new_err("stale file handle"))
        })
        .unwrap();
        let _patch = AttrPatch::replace(&fixture.gate(py), "_checkout_of", stale.as_any());
        assert_eq!(
            session_checkout(&fixture, json!({"cwd": fixture.linked()})),
            fixture.repo.to_str().unwrap()
        );
    });
}

fn owner_recorder<'py>(py: Python<'py>, seen: Arc<Mutex<String>>) -> Bound<'py, PyCFunction> {
    PyCFunction::new_closure(
        py,
        None,
        None,
        move |_args: &Bound<'_, PyTuple>, kwargs: Option<&Bound<'_, PyDict>>| -> PyResult<i32> {
            let owner: String = kwargs
                .expect("owner keyword")
                .get_item("owner")?
                .expect("owner value")
                .extract()?;
            *seen.lock().unwrap() = owner;
            Ok(0)
        },
    )
    .unwrap()
}

fn run_main(py: Python<'_>, gate: &Bound<'_, PyModule>, args: &[&str], input: &str) {
    let sys = PyModule::import(py, "sys").unwrap();
    let argv = PyList::new(py, args).unwrap();
    let stdin = PyModule::import(py, "io")
        .unwrap()
        .getattr("StringIO")
        .unwrap()
        .call1((input,))
        .unwrap();
    let _argv = AttrPatch::replace(&sys, "argv", argv.as_any());
    let _stdin = AttrPatch::replace(&sys, "stdin", &stdin);
    let result: i32 = gate
        .getattr("main")
        .unwrap()
        .call0()
        .unwrap()
        .extract()
        .unwrap();
    assert_eq!(result, 0);
}

#[test]
fn main_names_the_owner_from_the_sessions_checkout() {
    let fixture = GateCase::with_worktree();
    Python::attach(|py| {
        let gate = fixture.gate(py);
        let verify_seen = Arc::new(Mutex::new(String::new()));
        let bash_seen = Arc::new(Mutex::new(String::new()));
        let verify = owner_recorder(py, Arc::clone(&verify_seen));
        let bash = owner_recorder(py, Arc::clone(&bash_seen));
        let _verify = AttrPatch::replace(&gate, "verify", verify.as_any());
        let _bash = AttrPatch::replace(&gate, "verify_bash", bash.as_any());
        let input = json!({"cwd": fixture.linked(), "tool_name": "Edit"}).to_string();
        run_main(py, &gate, &["crg_gate", "verify"], &input);
        assert_eq!(*verify_seen.lock().unwrap(), "linked");
        run_main(
            py,
            &gate,
            &["crg_gate", "verify-bash", "--owner", "explicit"],
            &input,
        );
        assert_eq!(*bash_seen.lock().unwrap(), "explicit");
    });
}
