#![cfg(feature = "python-compat-tests")]
//! Dispatch telemetry contracts with Rust-owned fixtures and assertions.

#[path = "python_contracts/agent_comm_support.rs"]
#[allow(dead_code)]
mod comm_support;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use comm_support::{buffer_text, capture};
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyCFunction, PyDict, PyList, PyTuple};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use support::{module, path, AttrPatch, Case};

#[test]
fn session_id_reads_the_payload_and_tolerates_bad_json() {
    let _case = Case::new();
    Python::attach(|py| {
        let entry = module(py, "tooling.hooks.dispatch.__main__");
        for (payload, expected) in [
            (br#"{"session_id": "abc-123"}"#.as_slice(), "abc-123"),
            (b"not json".as_slice(), ""),
            (br#"{"session_id": 7}"#.as_slice(), ""),
            (b"[]".as_slice(), ""),
        ] {
            let actual = entry
                .getattr("_session_id")
                .unwrap()
                .call1((PyBytes::new(py, payload),))
                .unwrap();
            assert!(actual.eq(expected).unwrap());
        }
    });
}

#[test]
fn record_hook_timings_writes_one_event_per_outcome() {
    let case = Case::new();
    let events = case.root().join("events.jsonl");
    Python::attach(|py| {
        let telemetry = module(py, "conductor.context_telemetry");
        let _default = AttrPatch::replace(&telemetry, "DEFAULT_PATH", &path(py, &events));
        let outcome = module(py, "tooling.hooks.dispatch.merge")
            .getattr("HookOutcome")
            .unwrap();
        let first = PyDict::new(py);
        first.set_item("name", "pre-read-skeleton").unwrap();
        let output = PyDict::new(py);
        output.set_item("ok", true).unwrap();
        first.set_item("output", output).unwrap();
        first.set_item("elapsed_ms", 12.5).unwrap();
        let second = PyDict::new(py);
        second.set_item("name", "bash-guard").unwrap();
        second.set_item("output", py.None()).unwrap();
        second.set_item("error", "boom").unwrap();
        second.set_item("elapsed_ms", 3.0).unwrap();
        let outcomes = PyList::new(
            py,
            [
                outcome.call((), Some(&first)).unwrap(),
                outcome.call((), Some(&second)).unwrap(),
            ],
        )
        .unwrap();
        module(py, "tooling.hooks.dispatch.__main__")
            .getattr("_record_hook_timings")
            .unwrap()
            .call1(("PreToolUse", outcomes, "sess-1"))
            .unwrap();
    });
    let lines: Vec<Value> = fs::read_to_string(&events)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(lines.len(), 2);
    let by_tool: BTreeMap<_, _> = lines
        .iter()
        .map(|row| (row["tool"].as_str().unwrap(), row))
        .collect();
    assert_eq!(
        by_tool["pre-read-skeleton"]["elapsed_ms"].as_f64(),
        Some(12.5)
    );
    assert_eq!(by_tool["pre-read-skeleton"]["status"], "json");
    assert_eq!(by_tool["pre-read-skeleton"]["session_id"], "sess-1");
    assert_eq!(by_tool["bash-guard"]["status"], "boom");
    assert_eq!(by_tool["bash-guard"]["hook_event"], "PreToolUse");
}

#[test]
fn record_hook_timings_survives_a_telemetry_failure() {
    let _case = Case::new();
    Python::attach(|py| {
        let telemetry = module(py, "conductor.context_telemetry");
        let fail = PyCFunction::new_closure(
            py,
            None,
            None,
            |_args: &Bound<'_, PyTuple>, _kwargs| -> PyResult<()> {
                Err(pyo3::exceptions::PyOSError::new_err("disk full"))
            },
        )
        .unwrap();
        let _record = AttrPatch::replace(&telemetry, "record", fail.as_any());
        let kwargs = PyDict::new(py);
        kwargs.set_item("name", "pre-read-skeleton").unwrap();
        kwargs.set_item("output", PyDict::new(py)).unwrap();
        kwargs.set_item("elapsed_ms", 1.0).unwrap();
        let outcome = module(py, "tooling.hooks.dispatch.merge")
            .getattr("HookOutcome")
            .unwrap()
            .call((), Some(&kwargs))
            .unwrap();
        let outcomes = PyList::new(py, [outcome]).unwrap();
        let (stderr, _capture) = capture(py, "stderr");
        module(py, "tooling.hooks.dispatch.__main__")
            .getattr("_record_hook_timings")
            .unwrap()
            .call1(("PreToolUse", outcomes, ""))
            .unwrap();
        assert!(buffer_text(&stderr).contains("context telemetry unavailable"));
    });
}
