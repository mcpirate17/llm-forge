#![cfg(feature = "python-compat-tests")]
//! Bounded A2A supervisor host contracts from test_a2a_supervisor.py.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::prelude::*;
use pyo3::types::{PyAny, PyCFunction, PyDict, PyModule};
use serde_json::{json, Value};
use std::fs;
use std::sync::{Arc, Mutex};
use support::{assert_error, module, path, AttrPatch, Case};

fn setup<'py>(
    py: Python<'py>,
    root: &std::path::Path,
    supervisor: &Bound<'py, PyModule>,
) -> (Bound<'py, PyAny>, Vec<AttrPatch>, Arc<Mutex<f64>>) {
    let registry = module(py, "conductor.a2a_registry");
    let kwargs = PyDict::new(py);
    kwargs.set_item("name", "tester").unwrap();
    kwargs.set_item("port", 7442).unwrap();
    registry
        .getattr("init_registry")
        .unwrap()
        .call((path(py, root),), Some(&kwargs))
        .unwrap();
    let clock = Arc::new(Mutex::new(0.0));
    let monotonic_clock = Arc::clone(&clock);
    let monotonic = PyCFunction::new_closure(py, None, None, move |_, _| {
        Ok::<f64, PyErr>(*monotonic_clock.lock().unwrap())
    })
    .unwrap();
    let sleep_clock = Arc::clone(&clock);
    let sleep = PyCFunction::new_closure(py, None, None, move |args, _| {
        *sleep_clock.lock().unwrap() += args.get_item(0)?.extract::<f64>()?;
        Ok::<(), PyErr>(())
    })
    .unwrap();
    let time = supervisor.getattr("time").unwrap();
    let patches = vec![
        AttrPatch::replace(&time, "monotonic", monotonic.as_any()),
        AttrPatch::replace(&time, "sleep", sleep.as_any()),
    ];
    let kwargs = PyDict::new(py);
    kwargs.set_item("duration", 2).unwrap();
    kwargs.set_item("interval", 1).unwrap();
    let config = supervisor
        .getattr("SupervisorConfig")
        .unwrap()
        .call((path(py, root), "tester"), Some(&kwargs))
        .unwrap();
    (config, patches, clock)
}

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

fn replace<'py>(
    py: Python<'py>,
    config: &Bound<'py, PyAny>,
    key: &str,
    value: &Bound<'py, PyAny>,
) -> Bound<'py, PyAny> {
    let kwargs = PyDict::new(py);
    kwargs.set_item(key, value).unwrap();
    py.import("dataclasses")
        .unwrap()
        .getattr("replace")
        .unwrap()
        .call((config,), Some(&kwargs))
        .unwrap()
}

fn state(root: &std::path::Path) -> Value {
    serde_json::from_str(&fs::read_to_string(root.join("tester/supervisor.json")).unwrap()).unwrap()
}

#[test]
fn supervisor_finishes_at_budget_with_durable_state() {
    let case = Case::new();
    Python::attach(|py| {
        let supervisor = module(py, "conductor.a2a_supervisor");
        let (config, _clock_patches, _) = setup(py, case.root(), &supervisor);
        let flush_result = py
            .import("json")
            .unwrap()
            .getattr("loads")
            .unwrap()
            .call1((json!({"status":"complete","counts":{"queued":1}}).to_string(),))
            .unwrap();
        let flush_mock = mock(py, &flush_result);
        let _patch = AttrPatch::replace(supervisor.as_any(), "_flush", &flush_mock);
        let result = supervisor
            .getattr("supervise")
            .unwrap()
            .call1((&config,))
            .unwrap();
        assert_eq!(
            result
                .get_item("cycles")
                .unwrap()
                .extract::<usize>()
                .unwrap(),
            2
        );
        assert_eq!(
            result
                .get_item("status")
                .unwrap()
                .extract::<String>()
                .unwrap(),
            "completed"
        );
        let times: Vec<f64> = flush_mock
            .getattr("call_args_list")
            .unwrap()
            .try_iter()
            .unwrap()
            .map(|entry| {
                entry
                    .unwrap()
                    .get_item(0)
                    .unwrap()
                    .get_item(1)
                    .unwrap()
                    .extract()
                    .unwrap()
            })
            .collect();
        assert_eq!(times, vec![2.0, 1.0]);
        let encoded: String = py
            .import("json")
            .unwrap()
            .getattr("dumps")
            .unwrap()
            .call1((&result,))
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(
            state(case.root()),
            serde_json::from_str::<Value>(&encoded).unwrap()
        );
    });
}

#[test]
fn supervisor_never_stops_an_existing_endpoint() {
    let case = Case::new();
    Python::attach(|py| {
        let supervisor = module(py, "conductor.a2a_supervisor");
        let (config, _clock_patches, _) = setup(py, case.root(), &supervisor);
        let config = replace(py, &config, "serve", &true.into_pyobject(py).unwrap());
        let valid = mock(py, &true.into_pyobject(py).unwrap().to_owned().into_any());
        let empty = PyDict::new(py);
        empty.set_item("status", "complete").unwrap();
        let flush = mock(py, empty.as_any());
        let noop = mock(py, &py.None().into_bound(py));
        let _patches = [
            AttrPatch::replace(supervisor.as_any(), "_card_is_valid", &valid),
            AttrPatch::replace(supervisor.as_any(), "_flush", &flush),
            AttrPatch::replace(supervisor.as_any(), "_stop_process", &noop),
        ];
        supervisor
            .getattr("supervise")
            .unwrap()
            .call1((config,))
            .unwrap();
        assert_eq!(
            noop.getattr("call_count")
                .unwrap()
                .extract::<usize>()
                .unwrap(),
            0
        );
    });
}

#[test]
fn owned_endpoint_is_stopped_and_failure_state_is_durable() {
    let case = Case::new();
    Python::attach(|py| {
        let supervisor = module(py, "conductor.a2a_supervisor");
        let (config, _clock_patches, _) = setup(py, case.root(), &supervisor);
        let config = replace(py, &config, "serve", &true.into_pyobject(py).unwrap());
        let invalid = mock(py, &false.into_pyobject(py).unwrap().to_owned().into_any());
        let child = py
            .import("types")
            .unwrap()
            .getattr("SimpleNamespace")
            .unwrap();
        let poll = mock(py, &py.None().into_bound(py));
        let kwargs = PyDict::new(py);
        kwargs.set_item("pid", 992).unwrap();
        kwargs.set_item("poll", poll).unwrap();
        let child = child.call((), Some(&kwargs)).unwrap();
        let start = mock(py, &child);
        let stop = mock(py, &py.None().into_bound(py));
        let error = module(py, "conductor.a2a_registry")
            .getattr("A2aError")
            .unwrap();
        let failure = py
            .import("unittest.mock")
            .unwrap()
            .getattr("MagicMock")
            .unwrap();
        let kwargs = PyDict::new(py);
        kwargs
            .set_item("side_effect", error.call1(("bad flush",)).unwrap())
            .unwrap();
        let failure = failure.call((), Some(&kwargs)).unwrap();
        let _patches = [
            AttrPatch::replace(supervisor.as_any(), "_card_is_valid", &invalid),
            AttrPatch::replace(supervisor.as_any(), "_start_endpoint", &start),
            AttrPatch::replace(supervisor.as_any(), "_stop_process", &stop),
            AttrPatch::replace(supervisor.as_any(), "_flush", &failure),
        ];
        assert_error(
            py,
            supervisor
                .getattr("supervise")
                .unwrap()
                .call1((config,))
                .unwrap_err(),
            &error,
            "bad flush",
        );
        assert_eq!(
            stop.getattr("call_count")
                .unwrap()
                .extract::<usize>()
                .unwrap(),
            1
        );
        assert!(stop
            .getattr("call_args")
            .unwrap()
            .get_item(0)
            .unwrap()
            .get_item(0)
            .unwrap()
            .is(&child));
        let result = state(case.root());
        assert_eq!(result["status"], "failed");
        assert!(result["endpoint_pid"].is_null());
    });
}

#[test]
fn flush_timeout_is_bounded_and_preserves_retry_evidence() {
    let case = Case::new();
    Python::attach(|py| {
        let supervisor = module(py, "conductor.a2a_supervisor");
        let (config, _clock_patches, _) = setup(py, case.root(), &supervisor);
        let subprocess = supervisor.getattr("subprocess").unwrap();
        let timeout = subprocess
            .getattr("TimeoutExpired")
            .unwrap()
            .call1((vec!["forge"], 0.5))
            .unwrap();
        let kwargs = PyDict::new(py);
        kwargs.set_item("side_effect", timeout).unwrap();
        let run = py
            .import("unittest.mock")
            .unwrap()
            .getattr("MagicMock")
            .unwrap()
            .call((), Some(&kwargs))
            .unwrap();
        let _patch = AttrPatch::replace(&subprocess, "run", &run);
        let result = supervisor
            .getattr("_flush")
            .unwrap()
            .call1((config, 0.5))
            .unwrap();
        assert_eq!(
            result
                .get_item("status")
                .unwrap()
                .extract::<String>()
                .unwrap(),
            "timeout"
        );
        let call_args = run.getattr("call_args").unwrap();
        let command: Vec<String> = call_args
            .get_item(0)
            .unwrap()
            .get_item(0)
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(&command[command.len() - 2..], ["--max-messages", "100"]);
        let budget: f64 = call_args
            .get_item(1)
            .unwrap()
            .get_item("timeout")
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(budget, 0.5);
    });
}

#[test]
fn duplicate_lease_and_invalid_duration_fail_before_work() {
    let case = Case::new();
    Python::attach(|py| {
        let supervisor = module(py, "conductor.a2a_supervisor");
        let (config, _clock_patches, _) = setup(py, case.root(), &supervisor);
        let directory = case.root().join("tester");
        let lease = supervisor
            .getattr("_lease")
            .unwrap()
            .call1((path(py, &directory),))
            .unwrap();
        lease.call_method0("__enter__").unwrap();
        let error = module(py, "conductor.a2a_registry")
            .getattr("A2aError")
            .unwrap();
        assert_error(
            py,
            supervisor
                .getattr("supervise")
                .unwrap()
                .call1((&config,))
                .unwrap_err(),
            &error,
            "already active",
        );
        lease
            .call_method1("__exit__", (py.None(), py.None(), py.None()))
            .unwrap();
        let nan = f64::NAN.into_pyobject(py).unwrap().into_any();
        let invalid = replace(py, &config, "duration", &nan);
        assert_error(
            py,
            supervisor
                .getattr("supervise")
                .unwrap()
                .call1((invalid,))
                .unwrap_err(),
            &error,
            "duration",
        );
    });
}
