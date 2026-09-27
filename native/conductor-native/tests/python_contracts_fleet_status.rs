#![cfg(feature = "python-compat-tests")]
//! Rust-owned contract cases for the fleet status Python/native boundary.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::prelude::*;
use pyo3::types::{PyAny, PyCFunction, PyDict, PyModule, PyTuple};
use serde_json::{json, Value};
use std::process::Command;
use support::{assert_error, module, text, AttrPatch, Case};

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

fn patch_json<'py>(
    py: Python<'py>,
    module: &Bound<'py, PyModule>,
    name: &str,
    data: Value,
) -> AttrPatch {
    let callback = PyCFunction::new_closure(
        py,
        None,
        None,
        move |args: &Bound<'_, PyTuple>,
              _kwargs: Option<&Bound<'_, PyDict>>|
              -> PyResult<Py<PyAny>> { Ok(py_json(args.py(), &data).unbind()) },
    )
    .unwrap();
    AttrPatch::replace(module, name, callback.as_any())
}

fn sources<'py>(
    py: Python<'py>,
    fs: &Bound<'py, PyModule>,
    peers: Value,
    heard: Value,
    state: Value,
    procs: Value,
) -> Vec<AttrPatch> {
    vec![
        patch_json(py, fs, "read_peers", peers),
        patch_json(py, fs, "read_last_heard", heard),
        patch_json(py, fs, "read_state", state),
        patch_json(py, fs, "read_worktree_procs", procs),
    ]
}

fn default_state() -> Value {
    json!({"active_claims":[],"active_headings":[]})
}

fn report(fs: &Bound<'_, PyModule>) -> Value {
    json_value(&fs.getattr("build_report").unwrap().call0().unwrap())
}

fn render(py: Python<'_>, fs: &Bound<'_, PyModule>, value: Value) -> String {
    text(
        &fs.getattr("render")
            .unwrap()
            .call1((py_json(py, &value),))
            .unwrap(),
    )
}

#[test]
fn run_reports_nonzero_status_and_returns_stdout() {
    let _case = Case::new();
    Python::attach(|py| {
        let fs = module(py, "conductor.fleet_status");
        let failure = fs
            .getattr("_run")
            .unwrap()
            .call1((vec!["/bin/sh", "-c", "printf 'boom boom' >&2; exit 3"],))
            .unwrap_err();
        assert!(failure
            .matches(py, &fs.getattr("FleetStatusError").unwrap())
            .unwrap());
        assert!(failure.to_string().contains("exited 3"));
        assert!(failure.to_string().contains("boom boom"));
        let output = fs
            .getattr("_run")
            .unwrap()
            .call1((vec!["/bin/sh", "-c", "printf 'hello-fleet\\n'"],))
            .unwrap();
        assert_eq!(text(&output).trim(), "hello-fleet");
    });
}

#[test]
fn newest_inbox_message_wins_and_summary_is_capped() {
    let _case = Case::new();
    Python::attach(|py| {
        let fs = module(py, "conductor.fleet_status");
        let inbox = json!({"messages":[
            {"from":"seat-a","at":"2026-09-01T02:00:00+00:00","summary":"older words"},
            {"from":"seat-a","at":"2026-09-01T03:00:00+00:00","summary":"newest words from a"},
            {"from":"seat-b","at":"2026-09-01T01:00:00+00:00","summary":"b".repeat(200)}
        ]});
        let response = inbox.to_string();
        let run = PyCFunction::new_closure(
            py,
            None,
            None,
            move |args: &Bound<'_, PyTuple>,
                  _kwargs: Option<&Bound<'_, PyDict>>|
                  -> PyResult<String> {
                let argv = args.get_item(0)?;
                assert!(argv
                    .call_method1("__contains__", ("--json",))?
                    .extract::<bool>()?);
                assert!(argv
                    .call_method1("__contains__", ("--compact",))?
                    .extract::<bool>()?);
                assert!(!argv
                    .call_method1("__contains__", ("--full",))?
                    .extract::<bool>()?);
                Ok(response.clone())
            },
        )
        .unwrap();
        let _patch = AttrPatch::replace(&fs, "_run", run.as_any());
        let heard = json_value(&fs.getattr("read_last_heard").unwrap().call0().unwrap());
        assert_eq!(heard["seat-a"]["at"], "2026-09-01T03:00:00+00:00");
        assert_eq!(heard["seat-a"]["said"], "newest words from a");
        assert_eq!(heard["seat-b"]["said"], "b".repeat(160));
    });
}

#[test]
fn invalid_compact_inbox_shapes_raise_fleet_error() {
    let _case = Case::new();
    Python::attach(|py| {
        let fs = module(py, "conductor.fleet_status");
        let error_class = fs.getattr("FleetStatusError").unwrap();
        for payload in ["garbled", "{\"messages\":null}", "{\"messages\":[{}]}"] {
            let response = payload.to_owned();
            let run = PyCFunction::new_closure(
                py,
                None,
                None,
                move |_args: &Bound<'_, PyTuple>,
                      _kwargs: Option<&Bound<'_, PyDict>>|
                      -> PyResult<String> { Ok(response.clone()) },
            )
            .unwrap();
            let patch = AttrPatch::replace(&fs, "_run", run.as_any());
            let error = fs.getattr("read_last_heard").unwrap().call0().unwrap_err();
            assert_error(py, error, &error_class, "invalid compact A2A inbox");
            drop(patch);
        }
    });
}

#[test]
fn heading_seat_and_report_join_all_name_sources() {
    let _case = Case::new();
    Python::attach(|py| {
        let fs = module(py, "conductor.fleet_status");
        let heading =
            "Frozen candidate ready — 2026-09-01 ~02:45 UTC, codex-rust-architecture-learning";
        assert_eq!(
            text(
                &fs.getattr("_heading_seat")
                    .unwrap()
                    .call1((heading,))
                    .unwrap()
            ),
            "codex-rust-architecture-learning"
        );
        assert_eq!(
            text(
                &fs.getattr("_heading_seat")
                    .unwrap()
                    .call1(("no trailing seat marker here",))
                    .unwrap()
            ),
            ""
        );
        let _patches = sources(
            py,
            &fs,
            json!({"peer-seat":{"status":"up","port":7001}}),
            json!({"heard-seat":{"at":"2026-09-01T00:00:00","said":"hi"}}),
            json!({"active_claims":[{"owner":"claim-seat","paths":["a.py"],"expires_at":"2026-09-02T00:00:00+00:00"}],
                "active_headings":["did a thing — 2026-09-01, heading-seat"]}),
            json!({}),
        );
        let seats = report(&fs)["seats"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        let mut seats = seats;
        seats.sort();
        assert_eq!(
            seats,
            ["claim-seat", "heading-seat", "heard-seat", "peer-seat"]
        );
    });
}

#[test]
fn report_labels_a2a_presence_and_aggregates_claims() {
    let _case = Case::new();
    Python::attach(|py| {
        let fs = module(py, "conductor.fleet_status");
        let patches = sources(
            py,
            &fs,
            json!({"up-seat":{"status":"up","port":7002},"down-seat":{"status":"down","port":7003}}),
            json!({"ghost-seat":{"at":"t","said":"s"}}),
            default_state(),
            json!({}),
        );
        let seats = report(&fs)["seats"].clone();
        assert_eq!(seats["up-seat"]["a2a"], "up:7002");
        assert_eq!(seats["down-seat"]["a2a"], "down");
        assert_eq!(seats["ghost-seat"]["a2a"], "NO IDENTITY");
        drop(patches);
        let claims = json!([
            {"owner":"seat-x","paths":["b.py","a.py"],"expires_at":"2026-09-03T00:00:00+00:00"},
            {"owner":"seat-x","paths":["c.py"],"expires_at":"2026-09-02T00:00:00+00:00"}
        ]);
        let _patches = sources(
            py,
            &fs,
            json!({}),
            json!({}),
            json!({"active_claims":claims,"active_headings":[]}),
            json!({}),
        );
        let seat = report(&fs)["seats"]["seat-x"].clone();
        assert_eq!(seat["claims"], 2);
        assert_eq!(seat["claim_paths"], json!(["a.py", "b.py", "c.py"]));
        assert_eq!(seat["soonest_expiry"], "2026-09-02T00:00:00+00:00");
    });
}

#[test]
fn render_truncates_claim_paths_and_classifies_worktree_processes() {
    let _case = Case::new();
    Python::attach(|py| {
        let fs = module(py, "conductor.fleet_status");
        let report = json!({"generated_at":"2026-09-01T00:00:00+00:00","root":"/r",
            "seats":{"seat-y":{"a2a":"down","last_heard":null,"headings":[],"claims":1,
                "claim_paths":["p1","p2","p3","p4","p5","p6"],"soonest_expiry":null}},
            "worktree_processes":{}});
        let output = render(py, &fs, report);
        assert!(output.contains("p1, p2, p3, p4 (+2 more)"));
        assert!(!output.contains("p5"));
        let processes = json!({"generated_at":"t","root":"/r","seats":{},"worktree_processes":{
            "/tmp/llm-scratch":["proc1","proc2","proc3","proc4","proc5"],
            "/home/tim/Projects/LLM":["daemon1","daemon2"]}});
        let output = render(py, &fs, processes);
        assert!(output.contains("/tmp/llm-scratch: 5"));
        assert!(output.contains("proc1") && output.contains("proc3"));
        assert!(!output.contains("proc4"));
        assert!(output.contains("+2 more"));
        assert!(output.contains("/home/tim/Projects/LLM: 2"));
        assert!(!output.contains("daemon1"));
    });
}

#[test]
fn cli_emits_json_or_human_report_and_module_help() {
    let _case = Case::new();
    Python::attach(|py| {
        let fs = module(py, "conductor.fleet_status");
        let sys = py.import("sys").unwrap();
        let stdout = py
            .import("io")
            .unwrap()
            .getattr("StringIO")
            .unwrap()
            .call0()
            .unwrap();
        let _stdout_patch = AttrPatch::replace(&sys, "stdout", &stdout);
        let value = json!({"generated_at":"TSTAMP","root":"/r","seats":{},"worktree_processes":{}});
        let _report_patch = patch_json(py, &fs, "build_report", value.clone());
        assert_eq!(
            fs.getattr("main")
                .unwrap()
                .call1((vec!["--json"],))
                .unwrap()
                .extract::<i32>()
                .unwrap(),
            0
        );
        assert_eq!(
            serde_json::from_str::<Value>(&text(&stdout.call_method0("getvalue").unwrap()))
                .unwrap(),
            value
        );
        stdout.call_method1("seek", (0,)).unwrap();
        stdout.call_method1("truncate", (0,)).unwrap();
        assert_eq!(
            fs.getattr("main")
                .unwrap()
                .call1((Vec::<String>::new(),))
                .unwrap()
                .extract::<i32>()
                .unwrap(),
            0
        );
        assert!(text(&stdout.call_method0("getvalue").unwrap()).starts_with("FLEET STATUS  TSTAMP"));
    });
    let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../src");
    let python =
        Python::attach(|py| text(&py.import("sys").unwrap().getattr("executable").unwrap()));
    let inherited = std::env::var_os("PYTHONPATH").unwrap_or_default();
    let mut paths = vec![source];
    paths.extend(std::env::split_paths(&inherited));
    let python_path = std::env::join_paths(paths).unwrap();
    let output = Command::new(python)
        .args(["-m", "conductor.fleet_status", "--help"])
        .env("PYTHONPATH", python_path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout)
        .to_lowercase()
        .contains("fleet status"));
}

#[test]
fn worktree_pattern_comes_from_project_paths() {
    let _case = Case::new();
    Python::attach(|py| {
        let fs = module(py, "conductor.fleet_status");
        let pp = module(py, "conductor.project_paths");
        let actual = fs.getattr("_WORKTREE").unwrap();
        let patterns: Vec<String> = pp
            .getattr("worktree_patterns")
            .unwrap()
            .call1((fs.getattr("ROOT").unwrap(),))
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(
            text(&actual.getattr("pattern").unwrap()),
            format!("({})", patterns.join("|"))
        );
        for sample in ["/tmp/llm-scratch/foo", "/home/tim/Projects/LLM/bar"] {
            assert!(!actual.call_method1("search", (sample,)).unwrap().is_none());
        }
        assert!(actual
            .call_method1("search", ("/var/nope",))
            .unwrap()
            .is_none());
    });
}
