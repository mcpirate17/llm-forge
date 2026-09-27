#![cfg(feature = "python-compat-tests")]
//! Rust-owned contracts for bounded handoff validation, durable append, and CLI exit.

#[path = "python_contracts/agent_comm_support.rs"]
#[allow(dead_code)]
mod comm_support;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use comm_support::{bind_signature, buffer_text, capture, signature};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyCFunction, PyDict, PyTuple};
use std::fs;
use std::sync::{Arc, Mutex};
use support::{assert_error, module, path, AttrPatch, Case};

fn entry_args() -> Vec<String> {
    ["append", "--owner", "claude", "--title", "t", "--body", "b"]
        .into_iter()
        .map(str::to_owned)
        .collect()
}

fn append_with<'py>(
    py: Python<'py>,
    handoff: &Bound<'py, pyo3::types::PyModule>,
    file: &std::path::Path,
    refresh: &Bound<'py, PyCFunction>,
    report: Option<&Bound<'py, PyCFunction>>,
) -> Bound<'py, PyAny> {
    let kw = PyDict::new(py);
    kw.set_item("path", path(py, file)).unwrap();
    kw.set_item("refresh_state", refresh).unwrap();
    if let Some(report) = report {
        kw.set_item("on_refresh_error", report).unwrap();
    }
    handoff
        .getattr("append_status")
        .unwrap()
        .call(("claude", "title", "body"), Some(&kw))
        .unwrap()
}

fn appended_message(args: &Bound<'_, PyTuple>, kw: Option<&Bound<'_, PyDict>>) -> PyResult<String> {
    assert_eq!(args.len(), 1);
    assert!(kw.is_none_or(|kwargs| kwargs.is_empty()));
    args.get_item(0)?.extract()
}

#[test]
fn rejects_oversized_body_for_both_parameter_rows() {
    let _case = Case::new();
    Python::attach(|py| {
        let handoff = module(py, "conductor.handoff");
        let max: usize = handoff
            .getattr("MAX_BODY_LINES")
            .unwrap()
            .extract()
            .unwrap();
        let validate = handoff.getattr("validate_entry").unwrap();
        let class = handoff.getattr("HandoffError").unwrap();
        for (line_count, should_raise) in [(max, false), (max + 1, true)] {
            let body = (0..line_count)
                .map(|i| format!("line {i}"))
                .collect::<Vec<_>>()
                .join("\n");
            if !should_raise {
                validate.call1(("grok", "exactly at limit", body)).unwrap();
            } else {
                let explicit = (0..13)
                    .map(|i| format!("line {i}"))
                    .collect::<Vec<_>>()
                    .join("\n");
                assert_error(
                    py,
                    validate.call1(("grok", "too long", explicit)).unwrap_err(),
                    &class,
                    "12 lines",
                );
            }
        }
    });
}

#[test]
fn rejects_empty_owner() {
    let _case = Case::new();
    Python::attach(|py| {
        let handoff = module(py, "conductor.handoff");
        assert_error(
            py,
            handoff
                .getattr("validate_entry")
                .unwrap()
                .call1(("  ", "title", "body"))
                .unwrap_err(),
            &handoff.getattr("HandoffError").unwrap(),
            "owner",
        );
    });
}

#[test]
fn append_inserts_newest_first() {
    let case = Case::new();
    let file = case.write(
        ".current_work.md",
        "# Active Coordination\n\n## Old heading — 2026-08-22, glm-5.3\n\nOld body.\n",
    );
    Python::attach(|py| {
        let handoff = module(py, "conductor.handoff");
        let _file = AttrPatch::replace(handoff.as_any(), "CURRENT_WORK_PATH", &path(py, &file));
        let shape = signature(py, &[], &[]);
        let refresh = PyCFunction::new_closure(py, None, None, move |args, kw| {
            bind_signature(&shape, args, kw)?;
            Ok::<_, PyErr>(())
        })
        .unwrap();
        let kw = PyDict::new(py);
        kw.set_item("path", path(py, &file)).unwrap();
        kw.set_item("refresh_state", &refresh).unwrap();
        let entry: String = handoff
            .getattr("append_status")
            .unwrap()
            .call(
                (
                    "grok",
                    "Dump cap landed",
                    "Pre-edit now denies research dumps. Use this helper.",
                ),
                Some(&kw),
            )
            .unwrap()
            .extract()
            .unwrap();
        let text = fs::read_to_string(&file).unwrap();
        assert!(text.starts_with("# Active Coordination\n"));
        assert!(text.contains(entry.lines().next().unwrap()));
        assert!(text.find("Dump cap landed").unwrap() < text.find("Old heading").unwrap());
    });
}

#[test]
fn failed_state_refresh_is_reported_without_losing_append() {
    let case = Case::new();
    let file = case.root().join("log.md");
    Python::attach(|py| {
        let handoff = module(py, "conductor.handoff");
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let shape = signature(py, &[], &[]);
        let explode = PyCFunction::new_closure(py, None, None, move |args, kw| -> PyResult<()> {
            bind_signature(&shape, args, kw)?;
            Err(pyo3::exceptions::PyOSError::new_err(
                "state file is read-only",
            ))
        })
        .unwrap();
        let report = PyCFunction::new_closure(py, None, None, {
            let seen = Arc::clone(&seen);
            move |args: &Bound<'_, PyTuple>, kw| -> PyResult<()> {
                seen.lock().unwrap().push(appended_message(args, kw)?);
                Ok(())
            }
        })
        .unwrap();
        let entry = append_with(py, &handoff, &file, &explode, Some(&report));
        let entry: String = entry.extract().unwrap();
        assert!(entry.contains("title"));
        assert!(fs::read_to_string(&file).unwrap().contains("title"));
        let messages = seen.lock().unwrap();
        assert!(!messages.is_empty());
        assert!(messages[0].contains("state file is read-only"));
        assert!(messages[0].contains("OSError"));
    });
}

#[test]
fn successful_refresh_reports_nothing() {
    let case = Case::new();
    let file = case.root().join("log.md");
    Python::attach(|py| {
        let handoff = module(py, "conductor.handoff");
        let calls = Arc::new(Mutex::new(Vec::<i32>::new()));
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let shape = signature(py, &[], &[]);
        let refresh = PyCFunction::new_closure(py, None, None, {
            let calls = Arc::clone(&calls);
            move |args, kw| -> PyResult<()> {
                bind_signature(&shape, args, kw)?;
                calls.lock().unwrap().push(1);
                Ok(())
            }
        })
        .unwrap();
        let report = PyCFunction::new_closure(py, None, None, {
            let seen = Arc::clone(&seen);
            move |args: &Bound<'_, PyTuple>, kw| -> PyResult<()> {
                seen.lock().unwrap().push(appended_message(args, kw)?);
                Ok(())
            }
        })
        .unwrap();
        append_with(py, &handoff, &file, &refresh, Some(&report));
        assert_eq!(*calls.lock().unwrap(), [1]);
        assert!(seen.lock().unwrap().is_empty());
    });
}

fn patched_append<'py>(
    py: Python<'py>,
    real: &Bound<'py, PyAny>,
    file: &std::path::Path,
    refresh: &Bound<'py, PyCFunction>,
) -> Bound<'py, PyCFunction> {
    let real = real.clone().unbind();
    let file = file.to_path_buf();
    let refresh = refresh.clone().unbind();
    PyCFunction::new_closure(
        py,
        None,
        None,
        move |args: &Bound<'_, PyTuple>, kw: Option<&Bound<'_, PyDict>>| -> PyResult<Py<PyAny>> {
            let py = args.py();
            let merged = kw
                .map(|kwargs| kwargs.copy())
                .transpose()?
                .unwrap_or_else(|| PyDict::new(py));
            merged.set_item("path", path(py, &file))?;
            merged.set_item("refresh_state", refresh.bind(py))?;
            real.bind(py).call(args, Some(&merged)).map(Bound::unbind)
        },
    )
    .unwrap()
}

#[test]
fn cli_separates_stale_state_from_clean_append() {
    let case = Case::new();
    let file = case.root().join("log.md");
    Python::attach(|py| {
        let handoff = module(py, "conductor.handoff");
        let real = handoff.getattr("append_status").unwrap();
        let shape = signature(py, &[], &[]);
        let fail_refresh =
            PyCFunction::new_closure(py, None, None, move |args, kw| -> PyResult<()> {
                bind_signature(&shape, args, kw)?;
                Err(pyo3::exceptions::PyRuntimeError::new_err("no"))
            })
            .unwrap();
        let fail = patched_append(py, &real, &file, &fail_refresh);
        let (stdout, _out) = capture(py, "stdout");
        let (stderr, _err) = capture(py, "stderr");
        {
            let _patch = AttrPatch::replace(handoff.as_any(), "append_status", fail.as_any());
            let code: i32 = handoff
                .getattr("main")
                .unwrap()
                .call1((entry_args(),))
                .unwrap()
                .extract()
                .unwrap();
            let stale: i32 = handoff
                .getattr("EXIT_STALE_STATE")
                .unwrap()
                .extract()
                .unwrap();
            assert_eq!(code, stale);
        }
        assert!(buffer_text(&stdout).starts_with("## t"));
        assert!(buffer_text(&stderr).contains("refresh failed"));
        let shape = signature(py, &[], &[]);
        let clean_refresh = PyCFunction::new_closure(py, None, None, move |args, kw| {
            bind_signature(&shape, args, kw)?;
            Ok::<_, PyErr>(())
        })
        .unwrap();
        let clean = patched_append(py, &real, &file, &clean_refresh);
        let _patch = AttrPatch::replace(handoff.as_any(), "append_status", clean.as_any());
        let code: i32 = handoff
            .getattr("main")
            .unwrap()
            .call1((entry_args(),))
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(code, 0);
        let stale: i32 = handoff
            .getattr("EXIT_STALE_STATE")
            .unwrap()
            .extract()
            .unwrap();
        assert_ne!(stale, 0);
        assert_ne!(stale, 2);
    });
}
