#![cfg(feature = "python-compat-tests")]
//! Rust-owned contracts for AVO receipt cards and stagnation state.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyModule};
use serde_json::{json, Value};
use std::fs;
use std::path::Path;
use support::{assert_error, attr_bool, module, path, text, Case};

fn receipts<'py>(
    py: Python<'py>,
    cards: &Bound<'py, PyModule>,
    directory: &Path,
) -> Bound<'py, PyAny> {
    cards
        .getattr("load_receipts")
        .unwrap()
        .call1((path(py, directory),))
        .unwrap()
}

fn supervisor<'py>(py: Python<'py>, avo: &Bound<'py, PyModule>, state: &Path) -> Bound<'py, PyAny> {
    let kwargs = PyDict::new(py);
    kwargs.set_item("patience", 4).unwrap();
    kwargs.set_item("state_path", path(py, state)).unwrap();
    avo.getattr("StagnationSupervisor")
        .unwrap()
        .call((), Some(&kwargs))
        .unwrap()
}

fn record_step<'py>(
    py: Python<'py>,
    supervisor: &Bound<'py, PyAny>,
    improved: bool,
    lane: Option<&str>,
) -> Bound<'py, PyAny> {
    let kwargs = PyDict::new(py);
    kwargs.set_item("improved", improved).unwrap();
    if let Some(lane) = lane {
        kwargs.set_item("lane", lane).unwrap();
    }
    supervisor
        .call_method("record_step", (), Some(&kwargs))
        .unwrap()
}

fn rejection_count(supervisor: &Bound<'_, PyAny>) -> i32 {
    supervisor
        .call_method0("load_rejection_count")
        .unwrap()
        .extract()
        .unwrap()
}

#[test]
fn load_empty_dir() {
    let case = Case::new();
    Python::attach(|py| {
        let cards = module(py, "conductor.avo_cards");
        assert_eq!(receipts(py, &cards, case.root()).len().unwrap(), 0);
    });
}

#[test]
fn render_and_write_pass_receipt() {
    let case = Case::new();
    let directory = case.mkdir("receipts");
    let payload = json!({
        "status": "PASS", "is_valid": true, "score": 167.0, "mode": "paired",
        "metrics": {"provenance": {"config": "research/tools/battery.py",
            "fingerprint": "abc123def456", "compile_mode": "default"}},
        "diagnostics": []
    });
    case.write("receipts/r1.json", &payload.to_string());
    Python::attach(|py| {
        let cards = module(py, "conductor.avo_cards");
        let rows = receipts(py, &cards, &directory);
        assert_eq!(rows.len().unwrap(), 1);
        assert_eq!(
            text(&rows.get_item(0).unwrap().get_item("status").unwrap()),
            "PASS"
        );
        let card = case.root().join("kb_avo_receipts.md");
        let kwargs = PyDict::new(py);
        kwargs.set_item("path", path(py, &card)).unwrap();
        cards
            .getattr("write_card")
            .unwrap()
            .call((rows,), Some(&kwargs))
            .unwrap();
        let output = fs::read_to_string(card).unwrap();
        for expected in ["KB-AVO-RECEIPTS-01", "PASS", "battery.py"] {
            assert!(output.contains(expected), "missing {expected:?} from card");
        }
    });
}

#[test]
fn rejects_non_receipt() {
    let case = Case::new();
    case.write("junk.json", &json!({"hello": 1}).to_string());
    Python::attach(|py| {
        let cards = module(py, "conductor.avo_cards");
        let error = cards
            .getattr("load_receipts")
            .unwrap()
            .call1((path(py, case.root()),))
            .unwrap_err();
        assert_error(
            py,
            error,
            &cards.getattr("AvoCardsError").unwrap(),
            "not an avo_eval receipt",
        );
    });
}

#[test]
fn rejects_invalid_pass_receipt() {
    let case = Case::new();
    case.write(
        "bad-pass.json",
        &json!({"status": "PASS", "is_valid": false, "metrics": {}}).to_string(),
    );
    Python::attach(|py| {
        let cards = module(py, "conductor.avo_cards");
        let error = cards
            .getattr("load_receipts")
            .unwrap()
            .call1((path(py, case.root()),))
            .unwrap_err();
        assert_error(
            py,
            error,
            &cards.getattr("AvoCardsError").unwrap(),
            "not an avo_eval receipt",
        );
    });
}

#[test]
fn supervisor_records_improved() {
    let case = Case::new();
    let state = case.write(
        "active_state.json",
        &json!({"stagnation_counter": 3}).to_string(),
    );
    Python::attach(|py| {
        let avo = module(py, "conductor.avo_supervisor");
        let supervisor = supervisor(py, &avo, &state);
        let report = record_step(py, &supervisor, true, None);
        assert!(!attr_bool(&report, "stagnated"));
        assert_eq!(
            report
                .getattr("consecutive_rejections")
                .unwrap()
                .extract::<i32>()
                .unwrap(),
            0
        );
        assert_eq!(rejection_count(&supervisor), 0);
    });
}

#[test]
fn supervisor_triggers_stagnation_alert() {
    let case = Case::new();
    let state = case.write(
        "active_state.json",
        &json!({"stagnation_counter": 3}).to_string(),
    );
    Python::attach(|py| {
        let avo = module(py, "conductor.avo_supervisor");
        let supervisor = supervisor(py, &avo, &state);
        let report = record_step(py, &supervisor, false, Some("test_lane"));
        assert!(attr_bool(&report, "stagnated"));
        assert_eq!(
            report
                .getattr("consecutive_rejections")
                .unwrap()
                .extract::<i32>()
                .unwrap(),
            4
        );
        let hint = report.getattr("strategy_hint").unwrap();
        assert!(!hint.is_none());
        assert!(text(&hint).contains("PIVOT"));
        let alert = report.getattr("alert_payload").unwrap();
        assert!(!alert.is_none());
        assert_eq!(text(&alert.get_item("kind").unwrap()), "stagnation-alert");
    });
}

#[test]
fn supervisor_creates_runtime_state_atomically() {
    let case = Case::new();
    let state = case.root().join("avo_runtime_state.json");
    Python::attach(|py| {
        let avo = module(py, "conductor.avo_supervisor");
        let supervisor = supervisor(py, &avo, &state);
        let report = record_step(py, &supervisor, false, None);
        assert_eq!(
            report
                .getattr("consecutive_rejections")
                .unwrap()
                .extract::<i32>()
                .unwrap(),
            1
        );
        let persisted: Value = serde_json::from_str(&fs::read_to_string(&state).unwrap()).unwrap();
        assert_eq!(persisted["stagnation_counter"], 1);
        let temporary_files: Vec<_> = fs::read_dir(case.root())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(".avo_runtime_state.json.") && name.ends_with(".tmp"))
            .collect();
        assert!(
            temporary_files.is_empty(),
            "left temp files: {temporary_files:?}"
        );
    });
}
