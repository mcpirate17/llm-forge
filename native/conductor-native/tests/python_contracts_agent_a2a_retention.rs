#![cfg(feature = "python-compat-tests")]
//! A2A retention command boundary contracts from test_a2a_retention.py.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyModule};
use serde_json::json;
use std::process::Command;
use support::{assert_error, module, path, text, AttrPatch, Case};

fn mock<'py>(py: Python<'py>, result: &Bound<'py, PyAny>) -> Bound<'py, PyAny> {
    let kwargs = PyDict::new(py);
    kwargs.set_item("return_value", result).unwrap();
    py.import("unittest.mock")
        .unwrap()
        .getattr("MagicMock")
        .unwrap()
        .call((), Some(&kwargs))
        .unwrap()
}

fn response<'py>(
    py: Python<'py>,
    store: &std::path::Path,
    code: i32,
    stdout: String,
    stderr: &str,
) -> Bound<'py, PyAny> {
    let _ = store;
    py.import("subprocess")
        .unwrap()
        .getattr("CompletedProcess")
        .unwrap()
        .call1((vec!["forge"], code, stdout, stderr))
        .unwrap()
}

fn valid_output(store: &std::path::Path) -> String {
    json!({
        "schema_version":1,"authority":"deterministic-a2a-retention",
        "automatic":false,"mode":"preview","results":[{
            "store":store.to_string_lossy(),"mode":"preview","eligible":1,
            "compacted":0,"original_content_bytes":21,"tombstone_bytes":53,
            "logical_bytes_removed":0,"evidence_files":0,
            "evidence_protected":0,"evidence_snapshot_sha256":"a".repeat(64),
            "manifest_sha256":["b".repeat(64)],"event_sha256":["c".repeat(64)]
        }]
    })
    .to_string()
}

fn setup<'py>(
    py: Python<'py>,
    retention: &Bound<'py, PyModule>,
    output: &Bound<'py, PyAny>,
) -> (AttrPatch, AttrPatch, Bound<'py, PyAny>) {
    let binary = mock(py, path(py, std::path::Path::new("/fake/forge")).as_any());
    let binary_patch = AttrPatch::replace(retention.as_any(), "_forge_binary", &binary);
    let runner = mock(py, output);
    let subprocess = retention.getattr("subprocess").unwrap();
    let run_patch = AttrPatch::replace(&subprocess, "run", &runner);
    (binary_patch, run_patch, runner)
}

#[test]
fn public_api_forwards_exact_scope_and_time_to_native() {
    let case = Case::new();
    Python::attach(|py| {
        let retention = module(py, "conductor.a2a_retention");
        let store = case.root().join("a2a/worker/store.sqlite");
        let evidence = case.root().join("evidence");
        let completed = response(py, &store, 0, valid_output(&store), "");
        let (_binary_patch, _run_patch, runner) = setup(py, &retention, &completed);
        let datetime = py.import("datetime").unwrap();
        let now_args = PyDict::new(py);
        now_args
            .set_item("tzinfo", datetime.getattr("UTC").unwrap())
            .unwrap();
        let now = datetime
            .getattr("datetime")
            .unwrap()
            .call((2026, 8, 30, 12), Some(&now_args))
            .unwrap();
        let grace_args = PyDict::new(py);
        grace_args.set_item("hours", 3).unwrap();
        let grace = datetime
            .getattr("timedelta")
            .unwrap()
            .call((), Some(&grace_args))
            .unwrap();
        let kwargs = PyDict::new(py);
        kwargs.set_item("actor", "worker").unwrap();
        kwargs
            .set_item("evidence_root", path(py, &evidence))
            .unwrap();
        kwargs.set_item("now", now).unwrap();
        kwargs.set_item("grace", grace).unwrap();
        kwargs.set_item("limit", 7).unwrap();
        kwargs.set_item("apply", true).unwrap();
        let result = retention
            .getattr("compact_resolved_messages")
            .unwrap()
            .call((path(py, &store),), Some(&kwargs))
            .unwrap();
        assert_eq!(
            text(&result.getattr("store").unwrap()),
            store.to_string_lossy()
        );
        let hashes: Vec<String> = result
            .getattr("manifest_sha256")
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(hashes, vec!["b".repeat(64)]);
        let command: Vec<String> = runner
            .getattr("call_args")
            .unwrap()
            .get_item(0)
            .unwrap()
            .get_item(0)
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(
            command,
            vec![
                "/fake/forge",
                "mailbox",
                "--state-dir",
                &store.parent().unwrap().parent().unwrap().to_string_lossy(),
                "retention",
                "--evidence-root",
                &evidence.to_string_lossy(),
                "--grace-hours",
                "3.0",
                "--limit",
                "7",
                "--store",
                "worker",
                "--as-name",
                "worker",
                "--now",
                "2026-08-30T12:00:00+00:00",
                "--apply"
            ]
        );
        let call_kwargs = runner.getattr("call_args").unwrap().get_item(1).unwrap();
        for (key, expected) in [("capture_output", true), ("text", true), ("check", false)] {
            assert_eq!(
                call_kwargs
                    .get_item(key)
                    .unwrap()
                    .extract::<bool>()
                    .unwrap(),
                expected,
                "{key}"
            );
        }
    });
}

#[test]
fn invalid_time_grace_and_native_authority_fail_closed() {
    let case = Case::new();
    Python::attach(|py| {
        let retention = module(py, "conductor.a2a_retention");
        let wrong = json!({"authority":"wrong","results":[]}).to_string();
        let completed = response(py, case.root(), 0, wrong, "");
        let (_binary_patch, _run_patch, runner) = setup(py, &retention, &completed);
        let error = module(py, "conductor.a2a_registry")
            .getattr("A2aError")
            .unwrap();
        let datetime = py.import("datetime").unwrap();
        let naive = datetime
            .getattr("datetime")
            .unwrap()
            .call1((2026, 8, 30, 12))
            .unwrap();
        let kwargs = PyDict::new(py);
        kwargs.set_item("now", naive).unwrap();
        let sweep = retention.getattr("sweep").unwrap();
        assert_error(
            py,
            sweep
                .call((path(py, case.root()),), Some(&kwargs))
                .unwrap_err(),
            &error,
            "timezone-aware",
        );
        let hours = PyDict::new(py);
        hours.set_item("minutes", 59).unwrap();
        let grace = datetime
            .getattr("timedelta")
            .unwrap()
            .call((), Some(&hours))
            .unwrap();
        let kwargs = PyDict::new(py);
        kwargs.set_item("grace", grace).unwrap();
        assert_error(
            py,
            sweep
                .call((path(py, case.root()),), Some(&kwargs))
                .unwrap_err(),
            &error,
            "retention grace",
        );
        assert_eq!(
            runner
                .getattr("call_count")
                .unwrap()
                .extract::<usize>()
                .unwrap(),
            0
        );
        assert_error(
            py,
            sweep.call1((path(py, case.root()),)).unwrap_err(),
            &error,
            "wrong native retention authority",
        );
        assert_eq!(
            runner
                .getattr("call_count")
                .unwrap()
                .extract::<usize>()
                .unwrap(),
            1
        );
    });
}

#[test]
fn cli_previews_by_default_and_preserves_native_error() {
    let case = Case::new();
    Python::attach(|py| {
        let retention = module(py, "conductor.a2a_retention");
        let store = case.root().join("worker/store.sqlite");
        let completed = response(py, &store, 0, valid_output(&store), "");
        let (_binary_patch, _run_patch, runner) = setup(py, &retention, &completed);
        let output = py
            .import("io")
            .unwrap()
            .getattr("StringIO")
            .unwrap()
            .call0()
            .unwrap();
        let sys = py.import("sys").unwrap();
        let _stdout = AttrPatch::replace(sys.as_any(), "stdout", &output);
        let args = vec![
            "--state-dir",
            case.root().to_str().unwrap(),
            "--store",
            "worker",
        ];
        let code: i32 = retention
            .getattr("main")
            .unwrap()
            .call1((args,))
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(code, 0);
        let data: serde_json::Value =
            serde_json::from_str(&text(&output.call_method0("getvalue").unwrap())).unwrap();
        assert_eq!(data["mode"], "preview");
        let command: Vec<String> = runner
            .getattr("call_args")
            .unwrap()
            .get_item(0)
            .unwrap()
            .get_item(0)
            .unwrap()
            .extract()
            .unwrap();
        assert!(!command.contains(&"--apply".to_owned()));

        let failed = response(py, &store, 2, String::new(), "exact store required");
        runner.setattr("return_value", failed).unwrap();
        let error_output = py
            .import("io")
            .unwrap()
            .getattr("StringIO")
            .unwrap()
            .call0()
            .unwrap();
        let _stderr = AttrPatch::replace(sys.as_any(), "stderr", &error_output);
        let args = vec!["--state-dir", case.root().to_str().unwrap(), "--apply"];
        let code: i32 = retention
            .getattr("main")
            .unwrap()
            .call1((args,))
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(code, 2);
        assert!(
            text(&error_output.call_method0("getvalue").unwrap()).contains("exact store required")
        );
    });
}

#[test]
fn import_does_not_open_store_or_start_scheduler() {
    let case = Case::new();
    let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../src");
    let state = case.root().join("must-not-exist");
    let script = "import sqlite3; calls=[]; sqlite3.connect=lambda *a, **k: calls.append(True); import conductor.a2a_retention; print(len(calls))";
    let python = std::env::var_os("PYO3_PYTHON")
        .or_else(|| std::env::var_os("PYTHON"))
        .unwrap_or_else(|| "python3".into());
    let mut import_paths = vec![source.clone()];
    if let Some(paths) = std::env::var_os("PYTHONPATH") {
        import_paths.extend(std::env::split_paths(&paths));
    }
    let output = Command::new(python)
        .arg("-c")
        .arg(script)
        .current_dir(case.root())
        .env("PYTHONPATH", std::env::join_paths(import_paths).unwrap())
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .env("A2A_STATE_DIR", &state)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().lines().last(),
        Some("0")
    );
    assert!(!state.exists());
    let agent = std::fs::read_to_string(source.join("conductor/agent_a2a.py")).unwrap();
    assert!(!agent.contains("a2a_retention"));
    assert!(!agent.contains("compact_resolved_messages"));
}
