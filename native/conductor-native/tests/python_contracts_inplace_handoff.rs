#![cfg(feature = "python-compat-tests")]
//! Rust-owned contracts for durable, bounded in-place handoff envelopes.

#[path = "python_contracts/inplace_handoff_support.rs"]
mod inplace_support;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use inplace_support::{now, prepare, stage, state};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyModule};
use serde_json::Value;
use std::fs;
use std::path::Path;
use support::{assert_error, module, path, text, Case};

fn activate<'py>(
    py: Python<'py>,
    handoff: &Bound<'py, PyModule>,
    file: &Path,
    heading: &str,
    at: &Bound<'py, PyAny>,
) -> PyResult<Bound<'py, PyAny>> {
    let kwargs = PyDict::new(py);
    kwargs.set_item("state", state(py, heading))?;
    kwargs.set_item("now", at)?;
    handoff
        .getattr("activate_handoff")?
        .call((path(py, file),), Some(&kwargs))
}

fn hook<'py>(
    py: Python<'py>,
    handoff: &Bound<'py, PyModule>,
    root: &Path,
    heading: &str,
    identity: &str,
) -> Bound<'py, PyAny> {
    let kwargs = PyDict::new(py);
    kwargs.set_item("root", path(py, root)).unwrap();
    kwargs.set_item("state", state(py, heading)).unwrap();
    kwargs.set_item("now", now(py)).unwrap();
    handoff
        .getattr("hook_context")
        .unwrap()
        .call((identity,), Some(&kwargs))
        .unwrap()
}

#[test]
fn prepare_save_and_activate_is_idempotent_without_releasing_claims() {
    let case = Case::new();
    let file = case.root().join("handoff.json");
    Python::attach(|py| {
        let handoff = module(py, "conductor.inplace_handoff");
        handoff
            .getattr("save_handoff")
            .unwrap()
            .call1((prepare(py, &handoff), path(py, &file)))
            .unwrap();
        let first = activate(py, &handoff, &file, "controlled task", &now(py)).unwrap();
        let delta = module(py, "datetime")
            .getattr("timedelta")
            .unwrap()
            .call1((0, 60))
            .unwrap();
        let later = now(py).call_method1("__add__", (delta,)).unwrap();
        let second = activate(py, &handoff, &file, "controlled task", &later).unwrap();
        assert_eq!(
            text(
                &first
                    .getattr("envelope")
                    .unwrap()
                    .getattr("status")
                    .unwrap()
            ),
            "active"
        );
        assert!(!first
            .getattr("already_active")
            .unwrap()
            .extract::<bool>()
            .unwrap());
        assert!(second
            .getattr("already_active")
            .unwrap()
            .extract::<bool>()
            .unwrap());
        assert!(second
            .getattr("envelope")
            .unwrap()
            .getattr("activated_at")
            .unwrap()
            .eq(first
                .getattr("envelope")
                .unwrap()
                .getattr("activated_at")
                .unwrap())
            .unwrap());
        assert!(
            text(&first.getattr("context").unwrap()).contains("WORKING CONTEXT (non-authoritative")
        );
        let saved = handoff
            .getattr("load_handoff")
            .unwrap()
            .call1((path(py, &file),))
            .unwrap();
        let claims = saved
            .getattr("active_state")
            .unwrap()
            .get_item("active_claims")
            .unwrap();
        let original = state(py, "controlled task")
            .call_method0("to_dict")
            .unwrap()
            .get_item("active_claims")
            .unwrap();
        assert!(claims.eq(original).unwrap());
    });
}

#[test]
fn activate_rejects_tampered_envelope() {
    let case = Case::new();
    let file = case.root().join("handoff.json");
    Python::attach(|py| {
        let handoff = module(py, "conductor.inplace_handoff");
        handoff
            .getattr("save_handoff")
            .unwrap()
            .call1((prepare(py, &handoff), path(py, &file)))
            .unwrap();
        let mut raw: Value = serde_json::from_str(&fs::read_to_string(&file).unwrap()).unwrap();
        raw["task"] = Value::String("tampered".into());
        fs::write(&file, serde_json::to_string(&raw).unwrap()).unwrap();
        assert_error(
            py,
            activate(py, &handoff, &file, "controlled task", &now(py)).unwrap_err(),
            &handoff.getattr("HandoffError").unwrap(),
            "integrity",
        );
    });
}

#[test]
fn activate_rejects_stale_live_governance_state() {
    let case = Case::new();
    let file = case.root().join("handoff.json");
    Python::attach(|py| {
        let handoff = module(py, "conductor.inplace_handoff");
        handoff
            .getattr("save_handoff")
            .unwrap()
            .call1((prepare(py, &handoff), path(py, &file)))
            .unwrap();
        assert_error(
            py,
            activate(py, &handoff, &file, "changed coordination", &now(py)).unwrap_err(),
            &handoff.getattr("HandoffError").unwrap(),
            "stale",
        );
    });
}

#[test]
fn context_is_bounded_and_redacts_secret_like_values() {
    let _case = Case::new();
    Python::attach(|py| {
        let handoff = module(py, "conductor.inplace_handoff");
        let kwargs = PyDict::new(py);
        kwargs.set_item("task", "bounded projection").unwrap();
        kwargs
            .set_item(
                "context",
                format!(
                    "api_key=should-not-leak sk-abcdefghijklmnop\n{}",
                    "x".repeat(5_000)
                ),
            )
            .unwrap();
        kwargs
            .set_item("state", state(py, "controlled task"))
            .unwrap();
        kwargs.set_item("now", now(py)).unwrap();
        let envelope = handoff
            .getattr("prepare_handoff")
            .unwrap()
            .call((), Some(&kwargs))
            .unwrap();
        let context: String = envelope.getattr("context").unwrap().extract().unwrap();
        assert!(context.chars().count() <= 3_500);
        assert!(!context.contains("should-not-leak"));
        assert!(!context.contains("sk-abcdefghijklmnop"));
        assert!(context.contains("[REDACTED]"));
    });
}

#[test]
fn prepare_rejects_unsafe_paths() {
    let _case = Case::new();
    Python::attach(|py| {
        let handoff = module(py, "conductor.inplace_handoff");
        let kwargs = PyDict::new(py);
        kwargs.set_item("task", "unsafe").unwrap();
        kwargs.set_item("paths", ("../outside",)).unwrap();
        kwargs
            .set_item("state", state(py, "controlled task"))
            .unwrap();
        kwargs.set_item("now", now(py)).unwrap();
        assert_error(
            py,
            handoff
                .getattr("prepare_handoff")
                .unwrap()
                .call((), Some(&kwargs))
                .unwrap_err(),
            &handoff.getattr("HandoffError").unwrap(),
            "relative path",
        );
    });
}

#[test]
fn resolve_identity_prefers_explicit_then_env_then_checkout_default() {
    let case = Case::new();
    case.write(".agents/a2a/default_identity", "fable-5\n");
    Python::attach(|py| {
        let handoff = module(py, "conductor.inplace_handoff");
        let resolve = handoff.getattr("resolve_identity").unwrap();
        let kwargs = PyDict::new(py);
        kwargs
            .set_item(
                "environ",
                [("A2A_AGENT_NAME", "helm")]
                    .into_iter()
                    .collect::<std::collections::BTreeMap<_, _>>(),
            )
            .unwrap();
        kwargs.set_item("root", path(py, case.root())).unwrap();
        assert_eq!(text(&resolve.call(("sol",), Some(&kwargs)).unwrap()), "sol");
        assert_eq!(text(&resolve.call((), Some(&kwargs)).unwrap()), "helm");
        kwargs.set_item("environ", PyDict::new(py)).unwrap();
        assert_eq!(text(&resolve.call((), Some(&kwargs)).unwrap()), "fable-5");
        kwargs
            .set_item("root", path(py, &case.root().join("missing")))
            .unwrap();
        assert_eq!(text(&resolve.call((), Some(&kwargs)).unwrap()), "claude");
        kwargs.set_item("root", path(py, case.root())).unwrap();
        assert_error(
            py,
            resolve.call(("bad name",), Some(&kwargs)).unwrap_err(),
            &handoff.getattr("HandoffError").unwrap(),
            "invalid handoff identity",
        );
    });
}

#[test]
fn stage_then_hook_injects_once_and_chains_lineage() {
    let case = Case::new();
    Python::attach(|py| {
        let handoff = module(py, "conductor.inplace_handoff");
        let first = stage(py, &handoff, case.root(), "first", "c1");
        let envelope = first.get_item(0).unwrap();
        let expected = case.root().join(".agents/handoff/fable-5/pending.json");
        let staged = first.get_item(1).unwrap();
        assert!(staged
            .is_instance(&module(py, "pathlib").getattr("Path").unwrap())
            .unwrap());
        assert!(staged.eq(path(py, &expected)).unwrap());
        let context = text(&hook(
            py,
            &handoff,
            case.root(),
            "controlled task",
            "fable-5",
        ));
        assert!(context.starts_with(&format!(
            "HANDOFF {}: first",
            text(&envelope.getattr("handoff_id").unwrap())
        )));
        assert!(context.contains("c1"));
        assert_eq!(
            text(&hook(
                py,
                &handoff,
                case.root(),
                "controlled task",
                "fable-5"
            )),
            ""
        );
        assert_eq!(
            text(&hook(
                py,
                &handoff,
                case.root(),
                "controlled task",
                "nobody"
            )),
            ""
        );
        let second = stage(py, &handoff, case.root(), "second", "c2");
        assert!(second
            .get_item(0)
            .unwrap()
            .getattr("parent_handoff_id")
            .unwrap()
            .eq(envelope.getattr("handoff_id").unwrap())
            .unwrap());
        let empty = handoff
            .getattr("hook_output")
            .unwrap()
            .call1(("",))
            .unwrap();
        let expected_output = PyDict::new(py);
        expected_output
            .set_item("hookEventName", "SessionStart")
            .unwrap();
        assert!(empty
            .get_item("hookSpecificOutput")
            .unwrap()
            .eq(expected_output)
            .unwrap());
        let populated = handoff
            .getattr("hook_output")
            .unwrap()
            .call1(("x",))
            .unwrap();
        assert_eq!(
            text(
                &populated
                    .get_item("hookSpecificOutput")
                    .unwrap()
                    .get_item("additionalContext")
                    .unwrap()
            ),
            "x"
        );
    });
}

#[test]
fn hook_reports_a_stale_envelope_instead_of_dropping_it() {
    let case = Case::new();
    Python::attach(|py| {
        let handoff = module(py, "conductor.inplace_handoff");
        stage(py, &handoff, case.root(), "first", "c1");
        let stale = text(&hook(
            py,
            &handoff,
            case.root(),
            "changed coordination",
            "fable-5",
        ));
        assert!(stale.starts_with("HANDOFF NOT ACTIVATED for fable-5: handoff is stale"));
        assert!(text(&hook(
            py,
            &handoff,
            case.root(),
            "controlled task",
            "fable-5"
        ))
        .starts_with("HANDOFF "));
    });
}
