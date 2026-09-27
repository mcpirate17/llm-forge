#![cfg(feature = "python-compat-tests")]
//! Rust-owned assertions for claim enforcement across isolated fixture worktrees.

#[path = "python_contracts/crg_gate_support.rs"]
#[allow(dead_code)]
mod crg;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use crg::{bash_value, decision, edit_value, payload, read_exposure_log, GateCase};
use pyo3::prelude::*;
use serde_json::{json, Value};
use std::fs;
use support::AttrPatch;

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
fn a_sibling_worktree_path_is_claim_relevant() {
    let fixture = GateCase::with_worktree();
    Python::attach(|py| {
        let value = json!({"tool_input": {"file_path": fixture.linked().join("a.py")}});
        let (local, sibling): (Vec<String>, Vec<(String, String)>) = fixture
            .gate(py)
            .getattr("_classify_targets")
            .unwrap()
            .call1((payload(py, &value),))
            .unwrap()
            .extract()
            .unwrap();
        assert!(local.is_empty());
        assert_eq!(
            sibling,
            [(
                fixture.linked().to_str().unwrap().to_owned(),
                "a.py".to_owned()
            )]
        );
    });
}

#[test]
fn scratchpad_and_foreign_repos_remain_unclaimable() {
    let fixture = GateCase::with_worktree();
    Python::attach(|py| {
        for file in [
            "/tmp/loose.txt".to_owned(),
            fixture.stranger().join("a.py").to_str().unwrap().to_owned(),
        ] {
            let value = json!({"tool_input": {"file_path": file}});
            let (local, sibling): (Vec<String>, Vec<(String, String)>) = fixture
                .gate(py)
                .getattr("_classify_targets")
                .unwrap()
                .call1((payload(py, &value),))
                .unwrap()
                .extract()
                .unwrap();
            assert!(local.is_empty(), "local targets for {file}");
            assert!(sibling.is_empty(), "sibling targets for {file}");
        }
    });
}

#[test]
fn a_denied_sibling_worktree_write_is_still_recorded() {
    let fixture = GateCase::with_worktree();
    assert!(edit_decision(&fixture, &fixture.linked().join("a.py")).starts_with("BLOCKED:"));
    let entry: Value = serde_json::from_str(read_exposure_log(&fixture).trim()).unwrap();
    assert_eq!(entry["path"], "a.py");
    assert_eq!(entry["owner"], "claude");
    assert_eq!(entry["checkout"], fixture.linked().to_str().unwrap());
}

#[test]
fn sibling_worktree_write_is_denied_under_enforcement() {
    let mut fixture = GateCase::with_worktree();
    fixture.case.set_env("CRG_GATE_ENFORCE_WORKTREES", "1");
    let reason = edit_decision(&fixture, &fixture.linked().join("a.py"));
    assert!(reason.starts_with("BLOCKED: path 'a.py' is held by codex-phase22"));
    assert!(reason.contains(fixture.linked().to_str().unwrap()));
}

#[test]
fn a_claim_spans_every_worktree() {
    let mut fixture = GateCase::with_worktree();
    fixture.case.set_env("CRG_GATE_ENFORCE_WORKTREES", "1");
    assert_eq!(
        edit_decision(&fixture, &fixture.linked().join("b.py")),
        "allow"
    );
}

#[test]
fn bash_resolves_targets_against_the_worktree_it_runs_in() {
    let mut fixture = GateCase::with_worktree();
    fixture.case.set_env("CRG_GATE_ENFORCE_WORKTREES", "1");
    let command = format!("sed -i s/A/C/ {}", fixture.linked().join("a.py").display());
    Python::attach(|py| {
        let reason = decision(
            py,
            &fixture.gate(py),
            "verify_bash",
            &bash_value(&command, Some(fixture.linked())),
            "claude",
            true,
        );
        assert!(reason.starts_with("BLOCKED: path 'a.py' is held by codex-phase22"));
    });
}

#[test]
fn bash_in_a_worktree_is_denied_by_default() {
    let fixture = GateCase::with_worktree();
    Python::attach(|py| {
        let reason = decision(
            py,
            &fixture.gate(py),
            "verify_bash",
            &bash_value("sed -i s/A/C/ a.py", Some(fixture.linked())),
            "claude",
            true,
        );
        assert!(reason.starts_with("BLOCKED: path 'a.py' is held by codex-phase22"));
    });
}

#[test]
fn enforcement_is_off_only_when_it_is_explicitly_turned_off() {
    let mut fixture = GateCase::with_worktree();
    for value in ["0", ""] {
        fixture.case.set_env("CRG_GATE_ENFORCE_WORKTREES", value);
        Python::attach(|py| {
            let enforced: bool = fixture
                .gate(py)
                .getattr("_enforce_worktrees")
                .unwrap()
                .call0()
                .unwrap()
                .extract()
                .unwrap();
            assert!(!enforced);
        });
        assert_eq!(
            edit_decision(&fixture, &fixture.linked().join("a.py")),
            "allow"
        );
    }
    fixture.case.remove_env("CRG_GATE_ENFORCE_WORKTREES");
    Python::attach(|py| {
        let enforced: bool = fixture
            .gate(py)
            .getattr("_enforce_worktrees")
            .unwrap()
            .call0()
            .unwrap()
            .extract()
            .unwrap();
        assert!(enforced);
    });
}

#[test]
fn exposure_log_stops_at_its_cap() {
    let mut fixture = GateCase::with_worktree();
    fixture.case.set_env("CRG_GATE_ENFORCE_WORKTREES", "0");
    let log = fixture.exposure_log();
    fs::create_dir_all(log.parent().unwrap()).unwrap();
    fs::write(&log, "{\"x\": 1}\n").unwrap();
    Python::attach(|py| {
        let cap = 1i64.into_pyobject(py).unwrap().into_any();
        let _patch = AttrPatch::replace(&fixture.gate(py), "EXPOSURE_CAP_BYTES", &cap);
        assert_eq!(
            decision(
                py,
                &fixture.gate(py),
                "verify",
                &edit_value(&fixture.linked().join("a.py")),
                "claude",
                true
            ),
            "allow"
        );
    });
    assert_eq!(fs::read_to_string(log).unwrap(), "{\"x\": 1}\n");
}
