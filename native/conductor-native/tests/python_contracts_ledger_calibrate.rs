#![cfg(feature = "python-compat-tests")]
//! Rust-owned contracts for Python ledger calibration and its recorded API boundary.

#[path = "python_contracts/agent_comm_support.rs"]
#[allow(dead_code)]
mod comm_support;
#[path = "python_contracts/cost_contract_support.rs"]
#[allow(dead_code)]
mod fixture;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use comm_support::{buffer_text, capture, py_json};
use fixture::{assistant_line, equal, full_window, sample_row, tiny_transcript, window, TokenStub};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyBool, PyDict, PyList, PyTuple};
use serde_json::{json, Value};
use std::fs;
use std::path::Path;
use support::{module, path, Case};

fn calibrate<'py>(py: Python<'py>) -> Bound<'py, PyAny> {
    module(py, "conductor.ledger_calibrate").into_any()
}

fn sample<'py>(py: Python<'py>, uuid: &str, billed: i64) -> Bound<'py, PyAny> {
    let subject = calibrate(py);
    subject
        .getattr("SampleRow")
        .unwrap()
        .call_method1(
            "model_validate",
            (py_json(py, sample_row(uuid, billed, "s")),),
        )
        .unwrap()
}

fn jsonl_line(value: Value) -> String {
    format!("{value}\n")
}

fn call_main(py: Python<'_>, args: &[String]) -> i64 {
    calibrate(py)
        .getattr("main")
        .unwrap()
        .call1((PyList::new(py, args).unwrap(),))
        .unwrap()
        .extract()
        .unwrap()
}

fn field_f64(value: &Bound<'_, PyAny>, name: &str) -> f64 {
    value.getattr(name).unwrap().extract().unwrap()
}

fn dict_f64(value: &Bound<'_, PyAny>, name: &str) -> f64 {
    value.get_item(name).unwrap().extract().unwrap()
}

fn near(actual: f64, expected: f64) {
    assert!(
        (actual - expected).abs() <= 1e-12,
        "actual={actual}, expected={expected}"
    );
}

fn exact(actual: f64, expected: f64) {
    assert_eq!(actual, expected);
}

#[test]
fn test_block_type_and_chars_mirrors_the_rust_reader_rules() {
    let _case = Case::new();
    Python::attach(|py| {
        let subject = calibrate(py);
        let function = subject.getattr("_block_type_and_chars").unwrap();
        let cases = [
            (json!({"type":"text","text":"héllo"}), "text", 5),
            (
                json!({"type":"tool_result","content":"abcd"}),
                "tool_result",
                4,
            ),
            (
                json!({"type":"tool_result","content":[{"type":"text","text":"ab"},{"type":"image","source":{"type":"url"}},{"type":"text","text":"c"}]}),
                "tool_result",
                3,
            ),
            (json!({"type":"tool_result","content":7}), "tool_result", 0),
            (
                json!({"type":"tool_use","name":"Bash","input":{"a":1}}),
                "tool_use",
                11,
            ),
            (json!({"type":"tool_use","name":"Read"}), "tool_use", 4),
            (
                json!({"type":"image","source":{"type":"base64","data":"QUJD"}}),
                "image",
                4,
            ),
            (
                json!({"type":"image","source":{"type":"url","url":"https://x/y.png"}}),
                "image",
                0,
            ),
            (json!({"type":"thinking","thinking":"long"}), "thinking", 0),
            (json!({"type":"document","x":1}), "other", 25),
        ];
        for (block, kind, chars) in cases {
            let input = py_json(py, block.clone());
            let actual = function.call1((&input,)).unwrap();
            let kind_object: Bound<'_, PyAny> = kind.into_pyobject(py).unwrap().into_any();
            let char_object: Bound<'_, PyAny> =
                (chars as i64).into_pyobject(py).unwrap().into_any();
            let expected = PyTuple::new(py, [kind_object, char_object]).unwrap();
            equal(&actual, expected.as_any());
        }
    });
}

#[test]
fn test_window_starts_at_the_last_compaction_marker() {
    let case = Case::new();
    let mut transcript = String::new();
    transcript.push_str(&jsonl_line(json!({"type":"user","message":{"role":"user","content":[{"type":"text","text":"old world"}]}})));
    transcript.push_str(&format!("{}\n", assistant_line("a1", "claude-sonnet-5")));
    transcript.push_str(&jsonl_line(json!({"uuid":"m1","isCompactSummary":true,"message":{"role":"user","content":[{"type":"text","text":"SUM"}]}})));
    transcript.push_str(&jsonl_line(json!({"type":"user","message":{"role":"user","content":[{"type":"text","text":"hello"},{"type":"tool_result","content":"ok"}]}})));
    transcript.push_str(&format!("{}\n", assistant_line("a2", "claude-sonnet-5")));
    transcript.push_str(&format!("{}\n", assistant_line("a3", "claude-sonnet-5")));
    let source = case.write("t.jsonl", &transcript);
    Python::attach(|py| {
        let subject = calibrate(py);
        let rows = PyList::new(py, [sample(py, "a2", 20)]).unwrap();
        let paths = PyList::new(py, [path(py, &source)]).unwrap();
        let windows = subject
            .getattr("resolve_windows")
            .unwrap()
            .call1((rows, paths))
            .unwrap();
        let windows = windows.cast::<PyList>().unwrap();
        assert_eq!(windows.len(), 1);
        let row = windows.get_item(0).unwrap();
        equal(
            &row.getattr("chars_by_block_type").unwrap(),
            &py_json(
                py,
                json!({"text":8,"tool_result":2,"tool_use":0,"image":0,"other":0,"thinking":0}),
            ),
        );
        assert!(row.getattr("model").unwrap().eq("claude-sonnet-5").unwrap());
        let text = row.getattr("payloads").unwrap().get_item("text").unwrap();
        assert!(text.cast::<PyList>().is_ok());
        equal(
            &text,
            &py_json(
                py,
                json!([{"type":"text","text":"SUM"},{"type":"text","text":"hello"}]),
            ),
        );
    });
}

#[test]
fn test_a_turn_missing_from_every_transcript_fails_loud() {
    let case = Case::new();
    let source = tiny_transcript(&case);
    Python::attach(|py| {
        let subject = calibrate(py);
        let rows = PyList::new(py, [sample(py, "nope", 10)]).unwrap();
        let paths = PyList::new(py, [path(py, &source)]).unwrap();
        let error = subject
            .getattr("resolve_windows")
            .unwrap()
            .call1((rows, paths))
            .unwrap_err();
        assert!(error
            .matches(py, py.get_type::<pyo3::exceptions::PySystemExit>())
            .unwrap());
        assert!(error.to_string().contains("none of the transcripts"));
    });
}

#[test]
fn test_nearest_rank_percentiles_match_the_rust_reader() {
    let _case = Case::new();
    Python::attach(|py| {
        let subject = calibrate(py);
        let percentile = subject.getattr("_percentile").unwrap();
        let values = PyList::new(py, [5.0, 1.0, 4.0, 2.0, 3.0]).unwrap();
        for (p, expected) in [(0.5, 3.0), (0.10, 1.0), (0.90, 5.0)] {
            exact(
                percentile.call1((&values, p)).unwrap().extract().unwrap(),
                expected,
            );
        }
        let even = PyList::new(py, [1.0, 2.0, 3.0, 4.0]).unwrap();
        exact(
            percentile.call1((even, 0.5)).unwrap().extract().unwrap(),
            2.0,
        );
        let error = subject
            .getattr("cpt_stats")
            .unwrap()
            .call1((PyList::empty(py),))
            .unwrap_err();
        assert!(error
            .matches(py, py.get_type::<pyo3::exceptions::PySystemExit>())
            .unwrap());
    });
}

#[test]
fn test_offline_stats_group_per_session_and_overall() {
    let _case = Case::new();
    Python::attach(|py| {
        let subject = calibrate(py);
        let rows = PyList::new(
            py,
            [
                window(py, "a", 10, 5),
                window(py, "a", 10, 7),
                window(py, "b", 4, 2),
            ],
        )
        .unwrap();
        let stats = subject
            .getattr("offline_stats")
            .unwrap()
            .call1((rows,))
            .unwrap();
        let stats = stats.cast::<PyDict>().unwrap();
        let keys: std::collections::BTreeSet<String> = stats
            .keys()
            .iter()
            .map(|key| key.extract().unwrap())
            .collect();
        assert_eq!(
            keys,
            ["a", "b", "overall"]
                .into_iter()
                .map(str::to_owned)
                .collect()
        );
        let a = stats.get_item("a").unwrap().unwrap();
        exact(field_f64(&a, "median"), 0.5);
        exact(field_f64(&a, "p10"), 0.5);
        exact(field_f64(&a, "p90"), 0.7);
        let b = stats.get_item("b").unwrap().unwrap();
        exact(field_f64(&b, "median"), 0.5);
        let overall = stats.get_item("overall").unwrap().unwrap();
        exact(field_f64(&overall, "median"), 0.5);
        exact(field_f64(&overall, "p90"), 0.7);
        let empty = PyList::new(py, [window(py, "a", 10, 0)]).unwrap();
        let error = subject
            .getattr("offline_stats")
            .unwrap()
            .call1((empty,))
            .unwrap_err();
        assert!(error
            .matches(py, py.get_type::<pyo3::exceptions::PySystemExit>())
            .unwrap());
    });
}

#[test]
fn test_main_without_a_key_and_without_offline_exits_2() {
    let mut case = Case::new();
    let sample_path = case.write("sample.jsonl", &jsonl_line(sample_row("t1", 10, "s")));
    let transcript = tiny_transcript(&case);
    case.remove_env("ANTHROPIC_API_KEY");
    let out = case.root().join("f.json");
    Python::attach(|py| {
        let (stdout, _capture) = capture(py, "stdout");
        let args = [
            sample_path.display().to_string(),
            transcript.display().to_string(),
            "--out".to_owned(),
            out.display().to_string(),
        ];
        assert_eq!(call_main(py, &args), 2);
        assert!(buffer_text(&stdout).contains("ANTHROPIC_API_KEY is not set"));
        assert!(!out.exists());
    });
}

#[test]
fn test_main_offline_writes_the_null_bound_fixture() {
    let mut case = Case::new();
    let sample_path = case.write("sample.jsonl", &jsonl_line(sample_row("t1", 10, "s")));
    let transcript = tiny_transcript(&case);
    case.remove_env("ANTHROPIC_API_KEY");
    let out = case.root().join("f.json");
    Python::attach(|py| {
        let (stdout, _capture) = capture(py, "stdout");
        let args = [
            sample_path.display().to_string(),
            transcript.display().to_string(),
            "--offline".to_owned(),
            "--out".to_owned(),
            out.display().to_string(),
        ];
        assert_eq!(call_main(py, &args), 0);
        assert!(buffer_text(&stdout).contains("offline mode"));
        let fixture = py
            .import("json")
            .unwrap()
            .getattr("loads")
            .unwrap()
            .call1((fs::read_to_string(&out).unwrap(),))
            .unwrap();
        assert!(fixture.get_item("per_block_type").unwrap().is_none());
        assert!(fixture.get_item("whole_input_mape").unwrap().is_none());
        assert!(fixture
            .get_item("model")
            .unwrap()
            .eq("claude-sonnet-5")
            .unwrap());
        assert!(fixture.get_item("n_turns").unwrap().eq(1).unwrap());
        exact(
            fixture
                .get_item("chars_per_token")
                .unwrap()
                .get_item("median")
                .unwrap()
                .extract()
                .unwrap(),
            0.5,
        );
        let generated: String = fixture
            .get_item("generated_utc")
            .unwrap()
            .extract()
            .unwrap();
        assert!(generated.len() >= 10 && generated.chars().nth(4) == Some('-'));
    });
}

#[test]
fn test_online_measure_reports_mape_against_recorded_responses() {
    let mut case = Case::new();
    Python::attach(|py| {
        let stub = TokenStub::new(&[50, 80, 90]);
        let (_patch, _client) = stub.install(py, &mut case);
        let subject = calibrate(py);
        let windows = PyList::new(py, [full_window(py)]).unwrap();
        let cache = path(py, &case.root().join("cache.json"));
        let measured = subject
            .getattr("online_measure")
            .unwrap()
            .call1((windows, "m", cache, 0))
            .unwrap();
        assert!(measured.get_item("api_calls").unwrap().eq(3).unwrap());
        assert!(measured.get_item("api_tokens").unwrap().eq(220).unwrap());
        let per = measured.get_item("per_block_type").unwrap();
        let text = per.get_item("text").unwrap();
        assert_eq!(text.cast::<PyDict>().unwrap().len(), 4);
        near(dict_f64(&text, "mape"), (60.0 - 50.0) / 50.0);
        assert!(text.get_item("n").unwrap().eq(1).unwrap());
        exact(dict_f64(&text, "mean_est"), 60.0);
        exact(dict_f64(&text, "mean_actual"), 50.0);
        near(dict_f64(&per.get_item("tool_result").unwrap(), "mape"), 0.5);
        assert!(per.get_item("tool_use").is_err() || per.get_item("tool_use").unwrap().is_none());
        near(
            measured
                .get_item("whole_input_mape")
                .unwrap()
                .extract()
                .unwrap(),
            10.0 / 90.0,
        );
        let calls = stub.calls().lock().unwrap();
        assert_eq!(calls.len(), 3);
        let roles = calls
            .iter()
            .map(|(_model, messages)| {
                let messages = messages.bind(py).cast::<PyList>().unwrap();
                let first = messages.get_item(0).unwrap();
                first.get_item("role").unwrap().extract::<String>().unwrap()
            })
            .collect::<Vec<_>>();
        assert_eq!(roles, ["user", "user", "user"]);
    });
}

#[test]
fn test_group_isolation_roles_match_the_transcript() {
    let _case = Case::new();
    Python::attach(|py| {
        let subject = calibrate(py);
        let group = subject.getattr("_messages_for_group").unwrap();
        let tool_use = py_json(py, json!([{"type":"tool_use","name":"Bash","input":{}}]));
        let actual = group.call1(("tool_use", &tool_use)).unwrap();
        let expected = py_json(
            py,
            json!([{"role":"assistant","content":[{"type":"tool_use","name":"Bash","input":{}}]}]),
        );
        equal(&actual, &expected);
        let text = py_json(py, json!([{"type":"text","text":"x"}]));
        let actual = group.call1(("text", &text)).unwrap();
        let expected = py_json(
            py,
            json!([{"role":"user","content":[{"type":"text","text":"x"}]}]),
        );
        equal(&actual, &expected);
    });
}

fn cached<'py>(
    py: Python<'py>,
    client: &Bound<'py, PyAny>,
    messages: &Bound<'py, PyAny>,
    cache: &Path,
) -> Bound<'py, PyAny> {
    calibrate(py)
        .getattr("count_tokens_cached")
        .unwrap()
        .call1((client, "m", messages, path(py, cache), 0))
        .unwrap()
}

fn assert_cache_result(result: &Bound<'_, PyAny>, tokens: i64, hit: bool) {
    let row = result.cast::<PyTuple>().unwrap();
    assert_eq!(row.len(), 2);
    assert!(row.get_item(0).unwrap().eq(tokens).unwrap());
    let actual_hit = row.get_item(1).unwrap();
    assert!(actual_hit.cast::<PyBool>().is_ok());
    assert_eq!(actual_hit.extract::<bool>().unwrap(), hit);
}

#[test]
fn test_count_tokens_cache_makes_a_rerun_cost_zero_calls() {
    let mut case = Case::new();
    Python::attach(|py| {
        let cache = case.root().join("cache.json");
        let messages = py_json(
            py,
            json!([{"role":"user","content":[{"type":"text","text":"x"}]}]),
        );
        let first = TokenStub::new(&[50]);
        let (_patch1, client1) = first.install(py, &mut case);
        let result = cached(py, client1.bind(py), &messages, &cache);
        assert_cache_result(&result, 50, false);
        assert_eq!(first.calls().lock().unwrap().len(), 1);
        let second = TokenStub::new(&[999]);
        let (_patch2, client2) = second.install(py, &mut case);
        let result = cached(py, client2.bind(py), &messages, &cache);
        assert_cache_result(&result, 50, true);
        assert!(second.calls().lock().unwrap().is_empty());
        let other = py_json(
            py,
            json!([{"role":"user","content":[{"type":"text","text":"y"}]}]),
        );
        let third = TokenStub::new(&[7]);
        let (_patch3, client3) = third.install(py, &mut case);
        let result = cached(py, client3.bind(py), &other, &cache);
        assert_cache_result(&result, 7, false);
    });
}

#[test]
fn test_summarize_excludes_zero_actuals() {
    let _case = Case::new();
    Python::attach(|py| {
        let subject = calibrate(py);
        let summarize = subject.getattr("summarize").unwrap();
        let pairs = PyList::new(
            py,
            [
                PyTuple::new(py, [10.0, 0.0]).unwrap(),
                PyTuple::new(py, [60.0, 50.0]).unwrap(),
            ],
        )
        .unwrap();
        let result = summarize.call1((pairs,)).unwrap();
        let result = result.cast::<PyDict>().unwrap();
        assert_eq!(result.len(), 4);
        near(dict_f64(result.as_any(), "mape"), 0.2);
        assert!(result.get_item("n").unwrap().unwrap().eq(1).unwrap());
        exact(dict_f64(result.as_any(), "mean_est"), 60.0);
        exact(dict_f64(result.as_any(), "mean_actual"), 50.0);
        let empty = summarize.call1((PyList::empty(py),)).unwrap();
        equal(
            &empty,
            &py_json(
                py,
                json!({"mape":0.0,"n":0,"mean_est":0.0,"mean_actual":0.0}),
            ),
        );
    });
}
