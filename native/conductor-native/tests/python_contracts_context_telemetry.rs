#![cfg(feature = "python-compat-tests")]
//! Rust-owned contract cases for the context telemetry Python/native boundary.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::prelude::*;
use pyo3::types::{PyAny, PyCFunction, PyDict, PyModule, PyTuple};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::thread;
use support::{assert_error, module, path, text, AttrPatch, Case};

fn py_json<'py>(py: Python<'py>, value: &Value) -> Bound<'py, PyAny> {
    py.import("json")
        .unwrap()
        .getattr("loads")
        .unwrap()
        .call1((value.to_string(),))
        .unwrap()
}

fn json_value(value: &Bound<'_, PyAny>) -> Value {
    let py = value.py();
    let encoded: String = py
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

fn call_json(py: Python<'_>, telemetry: &Bound<'_, PyModule>, name: &str, payload: Value) -> Value {
    json_value(
        &telemetry
            .getattr(name)
            .unwrap()
            .call1((py_json(py, &payload),))
            .unwrap(),
    )
}

fn hook_context(
    py: Python<'_>,
    telemetry: &Bound<'_, PyModule>,
    hook: &str,
    payload: Value,
    kwargs: &[(&str, &str)],
) -> Value {
    let options = PyDict::new(py);
    for (key, value) in kwargs {
        options.set_item(key, value).unwrap();
    }
    json_value(
        &telemetry
            .getattr("hook_context_event")
            .unwrap()
            .call((hook, py_json(py, &payload)), Some(&options))
            .unwrap(),
    )
}

fn append(py: Python<'_>, telemetry: &Bound<'_, PyModule>, value: Value, dest: &Path) -> bool {
    telemetry
        .getattr("append")
        .unwrap()
        .call1((py_json(py, &value), path(py, dest)))
        .unwrap()
        .extract()
        .unwrap()
}

fn record(py: Python<'_>, telemetry: &Bound<'_, PyModule>, value: Value, dest: &Path) {
    telemetry
        .getattr("record")
        .unwrap()
        .call1((py_json(py, &value), path(py, dest)))
        .unwrap();
}

fn jsonl(dest: &Path) -> Vec<Value> {
    fs::read_to_string(dest)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn usage_is_attributed_only_to_top_level_native_source() {
    let _case = Case::new();
    Python::attach(|py| {
        let telemetry = module(py, "conductor.context_telemetry");
        let record = call_json(
            py,
            &telemetry,
            "event",
            json!({"usage":{"prompt_tokens":7,"completion_tokens":3},
                "tool_output":{"usage":{"prompt_tokens":900,"completion_tokens":800}}}),
        );
        assert_eq!(record["usage_source"], "native");
        assert_eq!(record["native_usage_path"], "usage");
        assert_eq!(record["input_tokens"], 7);
        assert_eq!(record["output_tokens"], 3);
        let downstream = call_json(
            py,
            &telemetry,
            "event",
            json!({"tool_output":{"usage":{"prompt_tokens":900}}}),
        );
        assert_eq!(downstream["usage_source"], "none");
        assert!(downstream["native_usage_path"].is_null());
        assert!(downstream["input_tokens"].is_null());
    });
}

#[test]
fn append_accepts_exact_utf8_cap_and_refuses_overflow() {
    let case = Case::new();
    let dest = case.root().join("events.jsonl");
    let encoded = b"{\"id\":1,\"value\":\"\xc3\xa9\"}\n";
    Python::attach(|py| {
        let telemetry = module(py, "conductor.context_telemetry");
        let cap_value = encoded.len();
        let _patch = AttrPatch::replace(
            &telemetry,
            "MAX_LOG_BYTES",
            cap_value.into_pyobject(py).unwrap().as_any(),
        );
        assert!(append(py, &telemetry, json!({"id":1,"value":"é"}), &dest));
        assert_eq!(fs::read(&dest).unwrap(), encoded);
        assert!(!append(py, &telemetry, json!({"id":2}), &dest));
        assert_eq!(fs::read(&dest).unwrap(), encoded);
    });
}

#[test]
fn concurrent_appends_preserve_all_complete_records() {
    let case = Case::new();
    let dest = Arc::new(case.root().join("events.jsonl"));
    Python::attach(|py| {
        module(py, "conductor.context_telemetry");
    });
    let handles: Vec<_> = (0..64)
        .map(|id| {
            let dest = Arc::clone(&dest);
            thread::spawn(move || {
                Python::attach(|py| {
                    let telemetry = module(py, "conductor.context_telemetry");
                    append(
                        py,
                        &telemetry,
                        json!({"id":id,"value":format!("event-{id}")}),
                        &dest,
                    )
                })
            })
        })
        .collect();
    assert!(handles.into_iter().all(|handle| handle.join().unwrap()));
    let decoded = jsonl(&dest);
    assert_eq!(decoded.len(), 64);
    let mut ids: Vec<u64> = decoded
        .iter()
        .map(|row| row["id"].as_u64().unwrap())
        .collect();
    ids.sort_unstable();
    assert_eq!(ids, (0..64).collect::<Vec<_>>());
}

#[test]
fn unserializable_payloads_are_not_retained_and_bounding_requires_marker() {
    let _case = Case::new();
    Python::attach(|py| {
        let telemetry = module(py, "conductor.context_telemetry");
        let payload = PyDict::new(py);
        let input = PyDict::new(py);
        let output = PyDict::new(py);
        let set = py
            .import("builtins")
            .unwrap()
            .getattr("set")
            .unwrap()
            .call1(((1, 2),))
            .unwrap();
        input.set_item("bad", set).unwrap();
        output
            .set_item(
                "bad",
                py.import("builtins")
                    .unwrap()
                    .getattr("object")
                    .unwrap()
                    .call0()
                    .unwrap(),
            )
            .unwrap();
        payload.set_item("tool_input", input).unwrap();
        payload.set_item("tool_output", output).unwrap();
        let result = json_value(
            &telemetry
                .getattr("event")
                .unwrap()
                .call1((payload,))
                .unwrap(),
        );
        for key in [
            "input_bytes",
            "output_bytes",
            "tool_input_tokens_estimate",
            "output_tokens_estimate",
        ] {
            assert_eq!(result[key], 0, "{key}");
        }
        let literal = call_json(
            py,
            &telemetry,
            "event",
            json!({"tool_output":{"message":"the word elided"}}),
        );
        let structured = call_json(
            py,
            &telemetry,
            "event",
            json!({"tool_output":{"truncated":true}}),
        );
        assert_eq!(literal["output_bounded"], false);
        assert_eq!(structured["output_bounded"], true);
    });
}

#[test]
fn context_counts_only_injected_text_and_deny_reason() {
    let _case = Case::new();
    Python::attach(|py| {
        let telemetry = module(py, "conductor.context_telemetry");
        let record = hook_context(
            py,
            &telemetry,
            "session-start",
            json!({"hookSpecificOutput":{
            "hookEventName":"SessionStart","additionalContext":"héllo"}}),
            &[],
        );
        assert_eq!(record["event"], "HookContext");
        assert_eq!(record["tool"], "session-start");
        assert_eq!(record["hook_event"], "SessionStart");
        assert_eq!(record["output_bytes"], 6);
        assert_eq!(record["output_tokens_estimate"], 2);
        let quiet = hook_context(
            py,
            &telemetry,
            "pre-read-skeleton",
            json!({"hookSpecificOutput":{"hookEventName":"PreToolUse"}}),
            &[("event_name", "Override")],
        );
        assert_eq!(quiet["output_bytes"], 0);
        assert_eq!(quiet["hook_event"], "Override");
        let inferred = hook_context(
            py,
            &telemetry,
            "x",
            json!({"hookSpecificOutput":{"hookEventName":"PreToolUse"}}),
            &[],
        );
        assert_eq!(inferred["hook_event"], "PreToolUse");
        let non_json = telemetry
            .getattr("hook_context_event")
            .unwrap()
            .call(
                ("x", "not json"),
                Some(&{
                    let kwargs = PyDict::new(py);
                    kwargs.set_item("event_name", "E").unwrap();
                    kwargs
                }),
            )
            .unwrap();
        assert_eq!(json_value(&non_json)["hook_event"], "E");
        let denied = hook_context(
            py,
            &telemetry,
            "pre-read-skeleton",
            json!({"hookSpecificOutput":{
            "hookEventName":"PreToolUse","permissionDecision":"deny",
            "permissionDecisionReason":"PRE-READ DENIED: 812 lines"}}),
            &[],
        );
        assert_eq!(denied["output_bytes"], "PRE-READ DENIED: 812 lines".len());
    });
}

#[test]
fn rotation_preserves_full_log_and_prunes_oldest() {
    let case = Case::new();
    let dest = case.root().join("events.jsonl");
    Python::attach(|py| {
        let telemetry = module(py, "conductor.context_telemetry");
        let cap = b"{\"id\":0}\n".len();
        let _patch = AttrPatch::replace(
            &telemetry,
            "MAX_LOG_BYTES",
            cap.into_pyobject(py).unwrap().as_any(),
        );
        record(py, &telemetry, json!({"id":0}), &dest);
        record(py, &telemetry, json!({"id":1}), &dest);
        let rotated: Vec<_> = fs::read_dir(case.root())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|entry| entry != &dest)
            .collect();
        assert_eq!(rotated.len(), 1);
        assert_eq!(fs::read(&rotated[0]).unwrap(), b"{\"id\":0}\n");
        assert_eq!(jsonl(&dest), vec![json!({"id":1})]);

        for id in 2..=6 {
            let kwargs = PyDict::new(py);
            kwargs.set_item("keep_rotated", 2).unwrap();
            telemetry
                .getattr("record")
                .unwrap()
                .call(
                    (py_json(py, &json!({"id":id})), path(py, &dest)),
                    Some(&kwargs),
                )
                .unwrap();
        }
        let mut rotated_ids: Vec<u64> = fs::read_dir(case.root())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|entry| entry != &dest)
            .map(|entry| jsonl(&entry)[0]["id"].as_u64().unwrap())
            .collect();
        rotated_ids.sort_unstable();
        assert_eq!(rotated_ids, [4, 5]);
        assert_eq!(jsonl(&dest), vec![json!({"id":6})]);
    });
}

#[test]
fn failed_sink_disables_future_records_and_reports_reason_once() {
    let case = Case::new();
    let dest = case.root().join("locked/events.jsonl");
    Python::attach(|py| {
        let telemetry = module(py, "conductor.context_telemetry");
        let sys = py.import("sys").unwrap();
        let io = py.import("io").unwrap();
        let stderr = io.getattr("StringIO").unwrap().call0().unwrap();
        let _stderr_patch = AttrPatch::replace(&sys, "stderr", &stderr);
        let disabled_false = false.into_pyobject(py).unwrap();
        let _disabled_patch = AttrPatch::replace(&telemetry, "_disabled", disabled_false.as_any());
        let failed = PyCFunction::new_closure(
            py,
            None,
            None,
            |_args: &Bound<'_, PyTuple>, _kwargs: Option<&Bound<'_, PyDict>>| {
                Err::<bool, _>(pyo3::exceptions::PyOSError::new_err(
                    "fixture sink unwritable",
                ))
            },
        )
        .unwrap();
        let _append_patch = AttrPatch::replace(&telemetry, "_append_encoded", failed.as_any());
        record(py, &telemetry, json!({"id":1}), &dest);
        assert!(telemetry
            .getattr("_disabled")
            .unwrap()
            .extract::<bool>()
            .unwrap());
        let warning = text(&stderr.call_method0("getvalue").unwrap());
        assert!(warning.contains("context telemetry disabled for this process"));
        assert!(warning.contains(dest.to_str().unwrap()));
        record(py, &telemetry, json!({"id":2}), &dest);
        assert_eq!(text(&stderr.call_method0("getvalue").unwrap()), warning);
        assert!(!dest.exists());
    });
}

#[test]
fn summarize_groups_events_and_ignores_malformed_lines() {
    let case = Case::new();
    let dest = case.root().join("events.jsonl");
    Python::attach(|py| {
        let telemetry = module(py, "conductor.context_telemetry");
        for payload in [
            json!({"tool_name":"Bash","tool_response":"x".repeat(30)}),
            json!({"tool_name":"Bash","tool_response":"x".repeat(8)}),
            json!({"tool_name":"Read","tool_response":"y".repeat(10)}),
        ] {
            let item = call_json(py, &telemetry, "event", payload);
            assert!(append(py, &telemetry, item, &dest));
        }
        let hook = hook_context(
            py,
            &telemetry,
            "session-start",
            json!({"hookSpecificOutput":{
            "hookEventName":"SessionStart","additionalContext":"abcd"}}),
            &[],
        );
        assert!(append(py, &telemetry, hook, &dest));
        use std::io::Write;
        fs::OpenOptions::new()
            .append(true)
            .open(&dest)
            .unwrap()
            .write_all(b"not json\n")
            .unwrap();
        let kwargs = PyDict::new(py);
        kwargs.set_item("bound_bytes", 12).unwrap();
        let summary = json_value(
            &telemetry
                .getattr("summarize")
                .unwrap()
                .call((vec![path(py, &dest)],), Some(&kwargs))
                .unwrap(),
        );
        assert_eq!(summary["events"], 4);
        assert_eq!(summary["hook_context_bytes"], 4);
        let rows = summary["rows"].as_array().unwrap();
        let bash = rows
            .iter()
            .find(|row| row["event"] == "PostToolUse" && row["tool"] == "Bash")
            .unwrap();
        assert_eq!(bash["count"], 2);
        assert_eq!(bash["output_bytes"], 42);
        assert_eq!(bash["over_bound"], 1);
        assert_eq!(bash["over_bound_bytes"], 20);
        assert_eq!(rows[0]["tool"], "Bash");
        let read = rows.iter().find(|row| row["tool"] == "Read").unwrap();
        assert_eq!(read["over_bound"], 0);
        let share: f64 = rows.iter().map(|row| row["share"].as_f64().unwrap()).sum();
        assert!((share - 1.0).abs() < 1e-3);
    });
}

#[test]
fn edit_and_write_count_agent_visible_projection() {
    let _case = Case::new();
    Python::attach(|py| {
        let telemetry = module(py, "conductor.context_telemetry");
        let big = "x".repeat(10_000);
        let edit = call_json(
            py,
            &telemetry,
            "event",
            json!({"tool_name":"Edit","tool_response":{
            "filePath":"a.py","oldString":"a","newString":"b","originalFile":big,
            "structuredPatch":[{"lines":["-a","+b"]}],"userModified":false}}),
        );
        let write = call_json(
            py,
            &telemetry,
            "event",
            json!({"tool_name":"Write","tool_response":{
            "type":"create","filePath":"a.py","content":big}}),
        );
        let bash = call_json(
            py,
            &telemetry,
            "event",
            json!({"tool_name":"Bash","tool_response":{"stdout":big}}),
        );
        assert!(edit["output_bytes"].as_u64().unwrap() < 200);
        assert!(write["output_bytes"].as_u64().unwrap() < 100);
        assert!(bash["output_bytes"].as_u64().unwrap() > 10_000);
        let native = module(py, "conductor._native");
        assert_eq!(
            text(
                &native
                    .getattr("context_telemetry_model_visible_output_native")
                    .unwrap()
                    .call1(("Edit", "not a mapping"))
                    .unwrap()
            ),
            "not a mapping"
        );
    });
}

#[test]
fn hook_timing_and_instruction_hash_contract() {
    let _case = Case::new();
    Python::attach(|py| {
        let telemetry = module(py, "conductor.context_telemetry");
        let kwargs = PyDict::new(py);
        kwargs.set_item("session_id", "abc").unwrap();
        let item = json_value(
            &telemetry
                .getattr("hook_timing_event")
                .unwrap()
                .call(
                    ("PreToolUse", "pre-read-skeleton", 12.3456, "json"),
                    Some(&kwargs),
                )
                .unwrap(),
        );
        assert_eq!(item["event"], "HookTiming");
        assert_eq!(item["tool"], "pre-read-skeleton");
        assert_eq!(item["hook_event"], "PreToolUse");
        assert_eq!(item["elapsed_ms"], 12.346);
        assert_eq!(item["status"], "json");
        assert_eq!(item["session_id"], "abc");
        assert_eq!(item["output_bytes"], 0);
        let quiet = json_value(
            &telemetry
                .getattr("hook_timing_event")
                .unwrap()
                .call1(("PostToolUse", "noop", 0.0, "quiet"))
                .unwrap(),
        );
        assert!(quiet.get("session_id").is_none());

        let input = json!({"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":"same content"}});
        let first = hook_context(
            py,
            &telemetry,
            "session-start",
            input.clone(),
            &[("category", "instructions"), ("session_id", "sess-1")],
        );
        let second = hook_context(
            py,
            &telemetry,
            "session-start",
            input,
            &[("category", "instructions"), ("session_id", "sess-2")],
        );
        let expected_hash = format!("{:x}", Sha256::digest(b"same content"));
        assert_eq!(first["category"], "instructions");
        assert_eq!(first["session_id"], "sess-1");
        assert_eq!(first["content_hash"], &expected_hash[..16]);
        assert_eq!(first["content_hash"], second["content_hash"]);
        let quiet = hook_context(
            py,
            &telemetry,
            "pre-read-skeleton",
            json!({"hookSpecificOutput":{"hookEventName":"PreToolUse"}}),
            &[],
        );
        assert!(quiet.get("category").is_none());
        assert!(quiet.get("content_hash").is_none());
    });
}

#[test]
fn report_counts_sessions_timings_and_repeat_instruction_content() {
    let case = Case::new();
    let dest = case.root().join("events.jsonl");
    Python::attach(|py| {
        let telemetry = module(py, "conductor.context_telemetry");
        let input = json!({"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":"same context"}});
        for _ in 0..2 {
            let item = hook_context(
                py,
                &telemetry,
                "session-start",
                input.clone(),
                &[("category", "instructions"), ("session_id", "sess-1")],
            );
            assert!(append(py, &telemetry, item, &dest));
        }
        for elapsed in [10.0, 30.0] {
            let kwargs = PyDict::new(py);
            kwargs.set_item("session_id", "sess-1").unwrap();
            let item = json_value(
                &telemetry
                    .getattr("hook_timing_event")
                    .unwrap()
                    .call(
                        ("PreToolUse", "pre-read-skeleton", elapsed, "json"),
                        Some(&kwargs),
                    )
                    .unwrap(),
            );
            assert!(append(py, &telemetry, item, &dest));
        }
        let report = json_value(
            &telemetry
                .getattr("summarize_report")
                .unwrap()
                .call1((vec![path(py, &dest)],))
                .unwrap(),
        );
        assert_eq!(report["sessions"]["sess-1"]["events"], 4);
        let stats = &report["hook_ms"]["by_hook"]["pre-read-skeleton"];
        assert_eq!(stats["count"], 2);
        assert_eq!(stats["total_ms"], 40.0);
        assert_eq!(stats["p50_ms"], 20.0);
        assert_eq!(report["instructions"]["resends"], 2);
        assert_eq!(report["instructions"]["distinct_content"], 1);
        assert_eq!(report["instructions"]["repeat_resends"], 1);
        assert_eq!(report["top_message_templates"][0]["hook"], "session-start");
        assert_eq!(report["since"], "all-time");
    });
}

#[test]
fn report_since_filters_old_events_and_rejects_bad_duration() {
    let case = Case::new();
    let dest = case.root().join("events.jsonl");
    Python::attach(|py| {
        let telemetry = module(py, "conductor.context_telemetry");
        let datetime = py.import("datetime").unwrap();
        let now = datetime
            .getattr("datetime")
            .unwrap()
            .call_method1("now", (datetime.getattr("UTC").unwrap(),))
            .unwrap();
        let old = now
            .call_method1(
                "__sub__",
                (datetime
                    .getattr("timedelta")
                    .unwrap()
                    .call1((0, 7200))
                    .unwrap(),),
            )
            .unwrap();
        let old_stamp = text(&old.call_method1("isoformat", ()).unwrap());
        let recent_stamp = text(&now.call_method1("isoformat", ()).unwrap());
        for (stamp, size) in [(old_stamp, 100), (recent_stamp, 50)] {
            assert!(append(
                py,
                &telemetry,
                json!({"timestamp":stamp,"event":"HookContext",
                "tool":"session-start","hook_event":"SessionStart","output_bytes":size,
                "category":"instructions"}),
                &dest
            ));
        }
        let kwargs = PyDict::new(py);
        kwargs.set_item("since", "30m").unwrap();
        let report = json_value(
            &telemetry
                .getattr("summarize_report")
                .unwrap()
                .call((vec![path(py, &dest)],), Some(&kwargs))
                .unwrap(),
        );
        assert_eq!(report["since"], "30m");
        assert_eq!(report["events"], 1);
        assert_eq!(report["instructions"]["bytes"], 50);
        let error = telemetry
            .getattr("_parse_since")
            .unwrap()
            .call1(("nonsense",))
            .unwrap_err();
        assert_error(
            py,
            error,
            &py.get_type::<pyo3::exceptions::PyValueError>().into_any(),
            "",
        );
    });
}
