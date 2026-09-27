#![cfg(feature = "python-compat-tests")]
//! Public Python adapter and native report binding agree on the same log.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::prelude::*;
use pyo3::types::PyDict;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::fs;
use support::{module, path, text, Case};

#[test]
fn rich_python_wrapper_matches_native_report_and_human_format() {
    let case = Case::new();
    let log = case.root().join("events.jsonl");
    fs::write(
        &log,
        concat!(
            "{\"timestamp\":\"2026-09-27T11:55:00+00:00\",\"event\":\"HookContext\",",
            "\"tool\":\"session-start\",\"hook_event\":\"SessionStart\",",
            "\"output_bytes\":12,\"category\":\"instructions\",\"content_hash\":\"same\"}\n",
            "{\"timestamp\":\"2026-09-27T11:56:00+00:00\",\"event\":\"HookTiming\",",
            "\"tool\":\"session-start\",\"elapsed_ms\":2.5,\"output_bytes\":0}\n",
        ),
    )
    .unwrap();
    Python::attach(|py| {
        let telemetry = module(py, "conductor.context_telemetry");
        let native = module(py, "conductor._native");
        let report = telemetry
            .getattr("summarize_report")
            .unwrap()
            .call1((vec![path(py, &log)],))
            .unwrap();
        let json_module = py.import("json").unwrap();
        let encoded: String = json_module
            .getattr("dumps")
            .unwrap()
            .call1((&report,))
            .unwrap()
            .extract()
            .unwrap();
        let value: Value = serde_json::from_str(&encoded).unwrap();
        let label = value["files"][0].as_str().unwrap();

        let native_json: String = native
            .getattr("context_telemetry_report_native")
            .unwrap()
            .call1((
                vec![log.to_string_lossy().into_owned()],
                8000,
                py.None(),
                10,
                "2026-09-27T12:00:00+00:00",
                label,
            ))
            .unwrap()
            .extract()
            .unwrap();
        let direct: Value = serde_json::from_str(&native_json).unwrap();
        assert_eq!(direct, value);
        assert_eq!(
            value["instructions"],
            json!({
                "resends":1,"bytes":12,"distinct_content":1,"repeat_resends":0
            })
        );

        let formatted = telemetry
            .getattr("_format_rich_summary")
            .unwrap()
            .call1((&report,))
            .unwrap();
        let direct_text = native
            .getattr("context_telemetry_format_summary_native")
            .unwrap()
            .call1((encoded, true))
            .unwrap();
        assert_eq!(text(&formatted), text(&direct_text));
        assert!(text(&formatted).contains("hook_ms total=2.5"));
        assert!(text(&formatted).contains("instructions resends=1 bytes=12"));
    });
}

#[test]
fn private_duration_adapter_keeps_datetime_return_type_and_validation() {
    let _case = Case::new();
    Python::attach(|py| {
        let telemetry = module(py, "conductor.context_telemetry");
        let datetime = py.import("datetime").unwrap();
        let cutoff = telemetry
            .getattr("_parse_since")
            .unwrap()
            .call1(("30m",))
            .unwrap();
        assert!(cutoff
            .is_instance(&datetime.getattr("datetime").unwrap())
            .unwrap());
        let error = telemetry
            .getattr("_parse_since")
            .unwrap()
            .call1(("bad",))
            .unwrap_err();
        assert!(error.is_instance_of::<pyo3::exceptions::PyValueError>(py));
        assert!(error
            .to_string()
            .contains("--since must look like 30m, 2h or 1d"));

        let kwargs = PyDict::new(py);
        kwargs.set_item("since", "bad").unwrap();
        let rich_error = telemetry
            .getattr("summarize_report")
            .unwrap()
            .call((Vec::<String>::new(),), Some(&kwargs))
            .unwrap_err();
        assert!(rich_error.is_instance_of::<pyo3::exceptions::PyValueError>(py));
    });
}

#[test]
fn strict_hook_hash_keeps_text_rules_separate_from_permissive_byte_count() {
    let _case = Case::new();
    Python::attach(|py| {
        let telemetry = module(py, "conductor.context_telemetry");
        let native = module(py, "conductor._native");
        let payload = PyDict::new(py);
        let specific = PyDict::new(py);
        specific.set_item("additionalContext", 7).unwrap();
        specific
            .set_item("permissionDecisionReason", "dénied")
            .unwrap();
        payload.set_item("hookSpecificOutput", &specific).unwrap();
        let selected = telemetry
            .getattr("_injected_context_text")
            .unwrap()
            .call1((&payload,))
            .unwrap();
        assert_eq!(text(&selected), "dénied");
        let item = telemetry
            .getattr("hook_context_event")
            .unwrap()
            .call1(("hook", &payload))
            .unwrap();
        let record = item.cast::<PyDict>().unwrap();
        assert_eq!(
            record
                .get_item("output_bytes")
                .unwrap()
                .unwrap()
                .extract::<usize>()
                .unwrap(),
            1
        );
        let digest = format!("{:x}", Sha256::digest("dénied".as_bytes()));
        assert_eq!(
            text(&record.get_item("content_hash").unwrap().unwrap()),
            &digest[..16]
        );

        let old = native
            .getattr("context_telemetry_hook_event_native")
            .unwrap()
            .call1(("hook", &payload, "", "2026-09-27T12:00:00Z"))
            .unwrap();
        assert!(old
            .cast::<PyDict>()
            .unwrap()
            .get_item("content_hash")
            .unwrap()
            .is_none());

        let unicode = PyDict::new(py);
        let unicode_specific = PyDict::new(py);
        unicode_specific
            .set_item("additionalContext", "héllo")
            .unwrap();
        unicode
            .set_item("hookSpecificOutput", unicode_specific)
            .unwrap();
        let selected = telemetry
            .getattr("_injected_context_text")
            .unwrap()
            .call1((&unicode,))
            .unwrap();
        assert_eq!(text(&selected), "héllo");
        let event = telemetry
            .getattr("hook_context_event")
            .unwrap()
            .call1(("hook", &unicode))
            .unwrap();
        let record = event.cast::<PyDict>().unwrap();
        assert_eq!(
            record
                .get_item("output_bytes")
                .unwrap()
                .unwrap()
                .extract::<usize>()
                .unwrap(),
            6
        );
        let digest = format!("{:x}", Sha256::digest("héllo".as_bytes()));
        assert_eq!(
            text(&record.get_item("content_hash").unwrap().unwrap()),
            &digest[..16]
        );

        let mapping = py
            .import("collections")
            .unwrap()
            .getattr("UserDict")
            .unwrap()
            .call1((specific,))
            .unwrap();
        let mapped_payload = PyDict::new(py);
        mapped_payload
            .set_item("hookSpecificOutput", mapping)
            .unwrap();
        assert_eq!(
            text(
                &telemetry
                    .getattr("_injected_context_text")
                    .unwrap()
                    .call1((&mapped_payload,))
                    .unwrap()
            ),
            ""
        );
        let mapped = telemetry
            .getattr("hook_context_event")
            .unwrap()
            .call1(("hook", mapped_payload))
            .unwrap();
        let mapped = mapped.cast::<PyDict>().unwrap();
        assert_eq!(
            mapped
                .get_item("output_bytes")
                .unwrap()
                .unwrap()
                .extract::<usize>()
                .unwrap(),
            1
        );
        assert!(mapped.get_item("content_hash").unwrap().is_none());
    });
}

#[test]
fn hook_hash_follows_optional_labels_in_serialized_key_order() {
    let _case = Case::new();
    Python::attach(|py| {
        let telemetry = module(py, "conductor.context_telemetry");
        let native = module(py, "conductor._native");
        let payload = PyDict::new(py);
        let specific = PyDict::new(py);
        specific
            .set_item("additionalContext", "visible text")
            .unwrap();
        payload.set_item("hookSpecificOutput", specific).unwrap();

        let kwargs = PyDict::new(py);
        kwargs.set_item("category", "instructions").unwrap();
        kwargs.set_item("session_id", "s-9").unwrap();
        let event = telemetry
            .getattr("hook_context_event")
            .unwrap()
            .call(("hook", &payload), Some(&kwargs))
            .unwrap();
        let event = event.cast::<PyDict>().unwrap();
        let keys: Vec<String> = event
            .keys()
            .iter()
            .map(|key| key.extract().unwrap())
            .collect();
        assert_eq!(
            &keys[keys.len() - 3..],
            ["category", "session_id", "content_hash"]
        );
        let json: String = py
            .import("json")
            .unwrap()
            .getattr("dumps")
            .unwrap()
            .call1((event,))
            .unwrap()
            .extract()
            .unwrap();
        assert!(json.find(r#""category""#).unwrap() < json.find(r#""session_id""#).unwrap());
        assert!(json.find(r#""session_id""#).unwrap() < json.find(r#""content_hash""#).unwrap());

        let direct = native
            .getattr("context_telemetry_hook_event_with_hash_native")
            .unwrap()
            .call1(("hook", &payload, "", "2026-09-27T12:00:00Z"))
            .unwrap();
        let direct = direct.cast::<PyDict>().unwrap();
        assert!(direct.get_item("category").unwrap().is_none());
        assert!(direct.get_item("session_id").unwrap().is_none());
        assert!(direct.get_item("content_hash").unwrap().is_some());

        let falsey = native
            .getattr("context_telemetry_hook_event_with_hash_native")
            .unwrap()
            .call1(("hook", &payload, "", "2026-09-27T12:00:00Z", 0, ""))
            .unwrap();
        let falsey = falsey.cast::<PyDict>().unwrap();
        assert!(falsey.get_item("category").unwrap().is_none());
        assert!(falsey.get_item("session_id").unwrap().is_none());

        let label = PyDict::new(py);
        label.set_item("kind", "instructions").unwrap();
        let truthy = native
            .getattr("context_telemetry_hook_event_with_hash_native")
            .unwrap()
            .call1(("hook", &payload, "", "2026-09-27T12:00:00Z", &label, 7))
            .unwrap();
        let truthy = truthy.cast::<PyDict>().unwrap();
        assert!(truthy.get_item("category").unwrap().unwrap().is(&label));
        assert_eq!(
            truthy
                .get_item("session_id")
                .unwrap()
                .unwrap()
                .extract::<i64>()
                .unwrap(),
            7
        );
    });
}
