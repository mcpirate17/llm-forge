#![cfg(feature = "python-compat-tests")]
//! Rust-owned assertions for claim decisions and the legacy Bash gate.

#[path = "python_contracts/crg_gate_support.rs"]
#[allow(dead_code)]
mod crg;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use crg::{bash_value, claim_result, decision, read_exposure_log, GateCase};
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyCFunction, PyDict, PyModule};
use serde_json::Value;
use support::{module, path, text, AttrPatch};

fn ownership<'py>(py: Python<'py>) -> Bound<'py, PyModule> {
    module(py, "conductor.candidate_review.ownership")
}

fn claims<'py>(py: Python<'py>, fixture: &GateCase) -> Bound<'py, PyAny> {
    ownership(py)
        .getattr("load_claims")
        .unwrap()
        .call1((path(py, &fixture.repo),))
        .unwrap()
        .get_item(0)
        .unwrap()
}

fn claim_for<'py>(py: Python<'py>, fixture: &GateCase, owner: &str) -> Bound<'py, PyAny> {
    claims(py, fixture)
        .try_iter()
        .unwrap()
        .map(Result::unwrap)
        .find(|claim| text(&claim.getattr("owner").unwrap()) == owner)
        .expect("fixture ownership claim")
}

fn activity<'py>(py: Python<'py>, fixture: &GateCase) -> Bound<'py, PyAny> {
    ownership(py)
        .getattr("load_activity")
        .unwrap()
        .call1((path(py, &fixture.repo),))
        .unwrap()
}

fn lapse_claim(py: Python<'_>, fixture: &GateCase, owner: &str) -> String {
    let claim = claim_for(py, fixture, owner);
    let id = text(&claim.getattr("claim_id").unwrap());
    let kwargs = PyDict::new(py);
    kwargs.set_item("days", 1).unwrap();
    let delta = PyModule::import(py, "datetime")
        .unwrap()
        .getattr("timedelta")
        .unwrap()
        .call((), Some(&kwargs))
        .unwrap();
    let before_creation = claim
        .getattr("creation")
        .unwrap()
        .call_method1("__sub__", (delta,))
        .unwrap();
    let touch_kwargs = PyDict::new(py);
    touch_kwargs.set_item("now", before_creation).unwrap();
    ownership(py)
        .getattr("touch_claim")
        .unwrap()
        .call((path(py, &fixture.repo), &id), Some(&touch_kwargs))
        .unwrap();
    id
}

fn bash_decision(fixture: &GateCase, command: &str, graph_used: bool) -> String {
    Python::attach(|py| {
        decision(
            py,
            &fixture.gate(py),
            "verify_bash",
            &bash_value(command, None),
            "claude",
            graph_used,
        )
    })
}

#[test]
fn owner_with_live_claim_is_allowed() {
    let fixture = GateCase::new();
    Python::attach(|py| {
        let (allowed, detail) = claim_result(&fixture.gate(py), "claude", "b.py");
        assert!(allowed);
        assert_eq!(detail.len(), 64);
    });
}

#[test]
fn denial_names_the_holder_and_expiry() {
    let fixture = GateCase::new();
    Python::attach(|py| {
        let (allowed, detail) = claim_result(&fixture.gate(py), "claude", "a.py");
        assert!(!allowed);
        assert!(detail.starts_with("path 'a.py' is held by codex-phase22 until "));
        for fragment in [
            "(claim-",
            "owner='claude' has no live claim",
            "coordinate via A2A",
        ] {
            assert!(detail.contains(fragment), "missing {fragment}: {detail}");
        }
    });
}

#[test]
fn denial_without_any_holder_keeps_plain_message() {
    let fixture = GateCase::new();
    Python::attach(|py| {
        let gate = fixture.gate(py);
        assert_eq!(
            claim_result(&gate, "claude", "c.py"),
            (
                false,
                "no live exact claim for owner='claude' path='c.py'".to_owned()
            )
        );
        let (allowed, detail) = claim_result(&gate, "", "a.py");
        assert!(!allowed);
        assert!(detail.contains("export GOVERNANCE_OWNER=<lane>"));
    });
}

#[test]
fn verify_bash_denies_a_write_to_another_owners_path() {
    let fixture = GateCase::new();
    assert!(bash_decision(&fixture, "echo x >> a.py", true)
        .starts_with("BLOCKED: path 'a.py' is held by codex-phase22"));
}

#[test]
fn verify_bash_allows_a_write_to_a_claimed_path() {
    let fixture = GateCase::new();
    assert_eq!(bash_decision(&fixture, "sed -i s/B/C/ b.py", true), "allow");
}

#[test]
fn verify_bash_allows_a_read_only_command() {
    let fixture = GateCase::new();
    assert_eq!(bash_decision(&fixture, "ls -la | head -20", true), "allow");
}

#[test]
fn verify_bash_denies_an_unresolvable_write() {
    let fixture = GateCase::new();
    let reason = bash_decision(
        &fixture,
        "python3 -c \"import shutil; shutil.rmtree(target)\"",
        true,
    );
    assert!(reason.contains("cannot be resolved"));
    assert!(reason.contains("Use Edit/Write"));
}

#[test]
fn verify_bash_requires_the_graph_call_before_a_write() {
    let fixture = GateCase::new();
    let mut payload = bash_value("echo x >> b.py", None);
    payload["session_id"] = Value::String("s2".to_owned());
    let reason = Python::attach(|py| {
        decision(
            py,
            &fixture.gate(py),
            "verify_bash",
            &payload,
            "claude",
            false,
        )
    });
    assert!(reason.contains("call a code-review-graph MCP tool"));
    assert!(reason.contains("b.py"));
}

#[test]
fn verify_bash_fails_open_when_the_resolver_raises() {
    let fixture = GateCase::new();
    Python::attach(|py| {
        let parser = crg::hook_module(py, "bash_write_targets");
        let boom = PyCFunction::new_closure(py, None, None, |_, _| -> PyResult<()> {
            Err(PyRuntimeError::new_err("resolver bug"))
        })
        .unwrap();
        let _patch = AttrPatch::replace(&parser, "repo_write_targets", boom.as_any());
        assert_eq!(
            decision(
                py,
                &fixture.gate(py),
                "verify_bash",
                &bash_value("echo x >> a.py", None),
                "claude",
                true
            ),
            "allow"
        );
    });
}

#[test]
fn an_allowed_write_stamps_the_claim() {
    let fixture = GateCase::new();
    Python::attach(|py| {
        assert_eq!(activity(py, &fixture).len().unwrap(), 0);
        let (allowed, _) = claim_result(&fixture.gate(py), "claude", "b.py");
        assert!(allowed);
        let id = text(
            &claim_for(py, &fixture, "claude")
                .getattr("claim_id")
                .unwrap(),
        );
        let stamps = activity(py, &fixture);
        assert!(stamps.contains(id.as_str()).unwrap());
        assert!(stamps.get_item(id).unwrap().is_truthy().unwrap());
    });
}

#[test]
fn a_lapsed_claim_of_our_own_says_so() {
    let fixture = GateCase::new();
    Python::attach(|py| {
        let id = lapse_claim(py, &fixture, "claude");
        let (allowed, detail) = claim_result(&fixture.gate(py), "claude", "b.py");
        assert!(!allowed);
        for fragment in ["no longer live", "re-claim", id.as_str()] {
            assert!(detail.contains(fragment), "missing {fragment}: {detail}");
        }
    });
}

#[test]
fn another_owners_lapsed_claim_does_not_hold_the_path() {
    let fixture = GateCase::new();
    Python::attach(|py| {
        lapse_claim(py, &fixture, "codex-phase22");
        let (allowed, detail) = claim_result(&fixture.gate(py), "claude", "a.py");
        assert!(!allowed);
        assert!(!detail.contains("is held by"));
    });
}

#[test]
fn a_lane_inherits_a_claim_written_before_lanes_had_names() {
    let fixture = GateCase::new();
    Python::attach(|py| {
        let (allowed, detail) = claim_result(&fixture.gate(py), "claude-branch-policy", "b.py");
        assert!(allowed);
        assert_eq!(detail.len(), 64);
    });
}

#[test]
fn inheriting_a_vendor_claim_is_logged_so_the_fallback_can_be_retired() {
    let fixture = GateCase::new();
    Python::attach(|py| {
        let (allowed, _) = claim_result(&fixture.gate(py), "claude-branch-policy", "b.py");
        assert!(allowed);
    });
    let last = read_exposure_log(&fixture)
        .lines()
        .last()
        .unwrap()
        .to_owned();
    let entry: Value = serde_json::from_str(&last).unwrap();
    assert_eq!(entry["owner"], "claude-branch-policy");
    assert_eq!(entry["tool"], "vendor-claim:claude");
}

#[test]
fn a_lane_does_not_inherit_another_vendors_claim() {
    let fixture = GateCase::new();
    Python::attach(|py| {
        let (allowed, detail) =
            claim_result(&fixture.gate(py), "codex-rust-hotpath-20260903", "b.py");
        assert!(!allowed);
        assert!(detail.contains("is held by claude until"));
    });
}
