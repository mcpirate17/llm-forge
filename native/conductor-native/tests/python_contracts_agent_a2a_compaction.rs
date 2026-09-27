#![cfg(feature = "python-compat-tests")]
//! A2A compaction Python boundary contracts retired from test_a2a_compaction.py.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict};
use serde_json::{json, Value};
use support::{assert_error, module, text, Case};

fn py_json<'py>(py: Python<'py>, value: &Value) -> Bound<'py, PyAny> {
    py.import("json")
        .unwrap()
        .getattr("loads")
        .unwrap()
        .call1((value.to_string(),))
        .unwrap()
}

fn json_value(value: &Bound<'_, PyAny>) -> Value {
    let encoded: String = value
        .py()
        .import("json")
        .unwrap()
        .getattr("dumps")
        .unwrap()
        .call1((value,))
        .unwrap()
        .extract()
        .unwrap();
    serde_json::from_str(&encoded).unwrap()
}

fn row() -> Value {
    json!({
        "message_id":"m1","direction":"inbound","sender":"alice",
        "recipient":"bob","body":"Please inspect the attached result.",
        "data_json": json!({"kind":"coordination-v2","thread_id":"thread-1",
            "summary":"Review the bounded evidence","status":"open",
            "requires_response":false,"supersedes":[]}).to_string(),
        "created_at":"2026-08-30T12:00:00+00:00",
        "received_at":"2026-08-30T12:00:01+00:00",
        "delivery_status":"delivered","status_reason":null,"read_at":null
    })
}

#[test]
fn mapping_payload_normalizes_and_compaction_receipt_is_order_independent() {
    let _case = Case::new();
    Python::attach(|py| {
        let compaction = module(py, "conductor.a2a_compaction");
        let coordination = json_value(
            &compaction
                .getattr("validate_coordination_v2")
                .unwrap()
                .call1((py_json(
                    py,
                    &json!({"kind":"coordination-v2",
                "summary":"  one\n  two  "}),
                ),))
                .unwrap(),
        );
        assert_eq!(
            coordination,
            json!({"kind":"coordination-v2",
            "thread_id":null,"summary":"one two","status":null,
            "requires_response":null,"supersedes":[]})
        );

        let source = py_json(py, &row());
        let compact = compaction.getattr("compact_message").unwrap();
        let receipt = json_value(&compact.call1((&source,)).unwrap());
        let reversed = PyDict::new(py);
        let original = source.cast::<PyDict>().unwrap();
        let entries: Vec<_> = original.iter().collect();
        for (key, value) in entries.into_iter().rev() {
            reversed.set_item(key, value).unwrap();
        }
        assert_eq!(json_value(&compact.call1((reversed,)).unwrap()), receipt);
        assert_eq!(receipt["message_id"], "m1");
        assert_eq!(receipt["summary"], "Review the bounded evidence");
        assert_eq!(receipt["actionable"], true);
        assert_eq!(receipt["receipt_sha256"].as_str().unwrap().len(), 64);
    });
}

#[test]
fn boundary_rejects_nonstring_keys_and_non_json_values() {
    let _case = Case::new();
    Python::attach(|py| {
        let compaction = module(py, "conductor.a2a_compaction");
        let error = compaction.getattr("CompactionError").unwrap();
        let validate = compaction.getattr("validate_coordination_v2").unwrap();
        let bad_key = PyDict::new(py);
        bad_key.set_item(1, "not-a-JSON-key").unwrap();
        assert_error(
            py,
            validate.call1((bad_key,)).unwrap_err(),
            &error,
            "keys must be strings",
        );
        let bad_value = PyDict::new(py);
        bad_value.set_item("kind", "coordination-v2").unwrap();
        bad_value
            .set_item(
                "summary",
                py.import("builtins")
                    .unwrap()
                    .getattr("object")
                    .unwrap()
                    .call0()
                    .unwrap(),
            )
            .unwrap();
        assert_error(
            py,
            validate.call1((bad_value,)).unwrap_err(),
            &error,
            "not JSON-compatible",
        );
    });
}

#[test]
fn native_errors_map_to_compaction_error_without_losing_reason() {
    let _case = Case::new();
    Python::attach(|py| {
        let compaction = module(py, "conductor.a2a_compaction");
        let error = compaction.getattr("CompactionError").unwrap();
        let invalid = json!({"kind":"coordination-v2","summary":"   "});
        assert_error(
            py,
            compaction
                .getattr("validate_coordination_v2")
                .unwrap()
                .call1((py_json(py, &invalid),))
                .unwrap_err(),
            &error,
            "must not be empty",
        );
        let mut large = row();
        large["sender"] = json!("é".repeat(512));
        assert_error(
            py,
            compaction
                .getattr("compact_message")
                .unwrap()
                .call1((py_json(py, &large),))
                .unwrap_err(),
            &error,
            "sender",
        );
        assert!(text(&error).contains("CompactionError"));
    });
}
