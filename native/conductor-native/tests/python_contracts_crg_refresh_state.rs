#![cfg(feature = "python-compat-tests")]
//! Real worker, lock, failure, and hook contracts for graph refresh state.

#[path = "python_contracts/crg_refresh_support.rs"]
#[allow(dead_code)]
mod refresh_support;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::exceptions::{PyRuntimeError, PyTypeError};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyCFunction, PyDict, PyList, PyModule, PyTuple};
use refresh_support::{
    batch_command, bind_signature, child_bin, json_obj, request, signature, RefreshCase,
};
use serde_json::json;
use std::fs::{self, FileTimes};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};
use support::{module, path, AttrPatch};

fn drain(
    py: Python<'_>,
    state: &Bound<'_, PyModule>,
    store: &Bound<'_, PyAny>,
    refresh: &Bound<'_, PyCFunction>,
    sleep: &Bound<'_, PyCFunction>,
    debounce: f64,
) -> usize {
    let options = PyDict::new(py);
    options.set_item("debounce", debounce).unwrap();
    options.set_item("sleep", sleep).unwrap();
    state
        .getattr("drain")
        .unwrap()
        .call((store, refresh), Some(&options))
        .unwrap()
        .extract()
        .unwrap()
}

fn no_sleep(py: Python<'_>) -> Bound<'_, PyCFunction> {
    let expected = signature(py, &["_s"], false);
    PyCFunction::new_closure(py, None, None, move |args, kwargs| -> PyResult<()> {
        bind_signature(&expected, args, kwargs)?;
        Ok(())
    })
    .unwrap()
}

fn body_store<'py>(
    py: Python<'py>,
    body: &Bound<'py, PyModule>,
    case: &RefreshCase,
) -> Bound<'py, PyAny> {
    body.getattr("store_for")
        .unwrap()
        .call1((path(py, &case.repo),))
        .unwrap()
}

fn patch_command<'py>(
    py: Python<'py>,
    body: &Bound<'py, PyModule>,
    mode: &'static str,
    extra: Option<&str>,
) -> AttrPatch {
    let callback = batch_command(py, mode, extra);
    AttrPatch::replace(body.as_any(), "_batch_command", callback.as_any())
}

#[test]
fn request_spawns_one_worker_then_queues() {
    let case = RefreshCase::new();
    Python::attach(|py| {
        let state = case.state(py);
        let store = case.store(py);
        let argv = vec![
            child_bin().to_owned(),
            "hold-lock".to_owned(),
            case.lock().to_str().unwrap().to_owned(),
            "2".to_owned(),
        ];
        assert_eq!(
            request(py, &state, &store, &["a.py"], &argv, case.case.root()),
            "spawned"
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        while !state
            .getattr("worker_alive")
            .unwrap()
            .call1((&store,))
            .unwrap()
            .extract::<bool>()
            .unwrap()
            && Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(state
            .getattr("worker_alive")
            .unwrap()
            .call1((&store,))
            .unwrap()
            .extract::<bool>()
            .unwrap());
        assert_eq!(
            request(
                py,
                &state,
                &store,
                &["b.py", "a.py"],
                &argv,
                case.case.root()
            ),
            "queued"
        );
        assert!(state
            .getattr("take_pending")
            .unwrap()
            .call1((&store,))
            .unwrap()
            .eq(PyList::new(py, ["a.py", "b.py"]).unwrap())
            .unwrap());
        assert!(!case.pending().exists());
    });
}

#[test]
fn drain_refuses_while_another_worker_holds_the_lock() {
    let case = RefreshCase::new();
    fs::write(case.pending(), "a.py\n").unwrap();
    let _lock = case.hold_lock();
    let calls = Arc::new(Mutex::new(Vec::<Vec<String>>::new()));
    Python::attach(|py| {
        let recorded = calls.clone();
        let refresh = PyCFunction::new_closure(
            py,
            None,
            None,
            move |args: &Bound<'_, PyTuple>, kwargs| -> PyResult<()> {
                if args.len() != 1 || kwargs.is_some_and(|kw: &Bound<'_, PyDict>| !kw.is_empty()) {
                    return Err(PyTypeError::new_err(
                        "list.append expects one positional argument",
                    ));
                }
                recorded.lock().unwrap().push(args.get_item(0)?.extract()?);
                Ok(())
            },
        )
        .unwrap();
        let state = case.state(py);
        assert_eq!(
            drain(py, &state, &case.store(py), &refresh, &no_sleep(py), 0.0),
            0
        );
    });
    assert!(calls.lock().unwrap().is_empty());
    assert!(case.pending().exists());
}

#[test]
fn drain_records_a_raising_refresh_and_keeps_going() {
    let case = RefreshCase::new();
    fs::write(case.pending(), "a.py\nb.py\n").unwrap();
    let seen = Arc::new(Mutex::new(Vec::<Vec<String>>::new()));
    Python::attach(|py| {
        let recorded = seen.clone();
        let refresh_signature = signature(py, &["paths"], false);
        let refresh = PyCFunction::new_closure(
            py,
            None,
            None,
            move |args: &Bound<'_, PyTuple>, kwargs| -> PyResult<()> {
                let bound = bind_signature(&refresh_signature, args, kwargs)?;
                let paths: Vec<String> =
                    bound.get_item("paths")?.expect("bound paths").extract()?;
                recorded.lock().unwrap().push(paths.clone());
                if paths.iter().any(|part| part == "b.py") {
                    return Err(PyRuntimeError::new_err("boom"));
                }
                Ok(())
            },
        )
        .unwrap();
        let observed = seen.clone();
        let pending = case.pending();
        let sleep_signature = signature(py, &["_seconds"], false);
        let sleep = PyCFunction::new_closure(py, None, None, move |args, kwargs| -> PyResult<()> {
            bind_signature(&sleep_signature, args, kwargs)?;
            if observed.lock().unwrap().len() == 1 {
                fs::write(&pending, "c.py\n").unwrap();
            }
            Ok(())
        })
        .unwrap();
        let state = case.state(py);
        let store = case.store(py);
        assert_eq!(drain(py, &state, &store, &refresh, &sleep, 0.0), 2);
        assert_eq!(*seen.lock().unwrap(), [vec!["a.py", "b.py"], vec!["c.py"]]);
        assert!(!state
            .getattr("worker_alive")
            .unwrap()
            .call1((&store,))
            .unwrap()
            .extract::<bool>()
            .unwrap());
        let notices = state
            .getattr("take_notices")
            .unwrap()
            .call1((&store,))
            .unwrap();
        let texts = PyList::empty(py);
        for item in notices.try_iter().unwrap() {
            texts
                .append(item.unwrap().get_item("text").unwrap())
                .unwrap();
        }
        assert!(texts
            .eq(PyList::new(py, ["RuntimeError: boom while refreshing ['a.py', 'b.py']"]).unwrap())
            .unwrap());
        assert!(state
            .getattr("take_notices")
            .unwrap()
            .call1((&store,))
            .unwrap()
            .eq(PyList::empty(py))
            .unwrap());
    });
}

#[test]
fn failure_output_surfaces_once_as_a_system_message() {
    let mut case = RefreshCase::new();
    case.setup_body();
    Python::attach(|py| {
        let (body, _patches) = case.body(py);
        let store = body_store(py, &body, &case);
        assert!(body
            .getattr("failure_output")
            .unwrap()
            .call1(("PreToolUse",))
            .unwrap()
            .is_none());
        let error = py
            .import("builtins")
            .unwrap()
            .getattr("ValueError")
            .unwrap()
            .call1(("bad db",))
            .unwrap();
        case.state(py)
            .getattr("record_failure")
            .unwrap()
            .call1((&store, vec!["pkg/mod.py"], error))
            .unwrap();
        let output = body
            .getattr("failure_output")
            .unwrap()
            .call1(("PreToolUse",))
            .unwrap();
        assert!(output
            .get_item("systemMessage")
            .unwrap()
            .extract::<String>()
            .unwrap()
            .contains("ValueError: bad db"));
        assert!(output
            .get_item("hookSpecificOutput")
            .unwrap()
            .get_item("additionalContext")
            .unwrap()
            .extract::<String>()
            .unwrap()
            .contains("pkg/mod.py"));
        assert!(body
            .getattr("failure_output")
            .unwrap()
            .call1(("PostToolUse",))
            .unwrap()
            .is_none());
    });
}

#[test]
fn drain_debounces_and_coalesces_into_one_refresh() {
    let case = RefreshCase::new();
    fs::write(case.pending(), "a.py\n").unwrap();
    let calls = Arc::new(Mutex::new(Vec::<Vec<String>>::new()));
    let sleeps = Arc::new(Mutex::new(Vec::<f64>::new()));
    Python::attach(|py| {
        let recorded = calls.clone();
        let refresh = PyCFunction::new_closure(
            py,
            None,
            None,
            move |args: &Bound<'_, PyTuple>, kwargs| -> PyResult<()> {
                if args.len() != 1 || kwargs.is_some_and(|kw: &Bound<'_, PyDict>| !kw.is_empty()) {
                    return Err(PyTypeError::new_err(
                        "list.append expects one positional argument",
                    ));
                }
                recorded.lock().unwrap().push(args.get_item(0)?.extract()?);
                Ok(())
            },
        )
        .unwrap();
        let delays = sleeps.clone();
        let pending = case.pending();
        let sleep_signature = signature(py, &["seconds"], false);
        let sleep = PyCFunction::new_closure(
            py,
            None,
            None,
            move |args: &Bound<'_, PyTuple>, kwargs| -> PyResult<()> {
                let bound = bind_signature(&sleep_signature, args, kwargs)?;
                let seconds: f64 = bound
                    .get_item("seconds")?
                    .expect("bound seconds")
                    .extract()?;
                let mut observed = delays.lock().unwrap();
                observed.push(seconds);
                if observed.len() == 1 {
                    use std::io::Write;
                    fs::OpenOptions::new()
                        .append(true)
                        .open(&pending)
                        .unwrap()
                        .write_all(b"b.py\na.py\n")
                        .unwrap();
                }
                Ok(())
            },
        )
        .unwrap();
        let state = case.state(py);
        assert_eq!(
            drain(py, &state, &case.store(py), &refresh, &sleep, 0.25),
            1
        );
    });
    assert_eq!(*calls.lock().unwrap(), [vec!["a.py", "b.py"]]);
    assert_eq!(*sleeps.lock().unwrap(), [0.25, 0.25]);
}

#[test]
fn hook_output_queues_only_graph_files() {
    let mut case = RefreshCase::new();
    case.setup_body();
    let queued = Arc::new(Mutex::new(Vec::<Vec<String>>::new()));
    Python::attach(|py| {
        let (body, _patches) = case.body(py);
        let recorded = queued.clone();
        let request_signature = signature(py, &["store", "paths"], true);
        let request = PyCFunction::new_closure(
            py,
            None,
            None,
            move |args: &Bound<'_, PyTuple>, kwargs| -> PyResult<()> {
                let bound = bind_signature(&request_signature, args, kwargs)?;
                recorded
                    .lock()
                    .unwrap()
                    .push(bound.get_item("paths")?.expect("bound paths").extract()?);
                Ok(())
            },
        )
        .unwrap();
        let _request = AttrPatch::replace(body.as_any(), "request", request.as_any());
        let shutil = body.getattr("shutil").unwrap();
        let which_signature = signature(py, &["name"], false);
        let installed =
            PyCFunction::new_closure(py, None, None, move |args, kwargs| -> PyResult<&str> {
                bind_signature(&which_signature, args, kwargs)?;
                Ok("/usr/bin/code-review-graph")
            })
            .unwrap();
        let _which = AttrPatch::replace(&shutil, "which", installed.as_any());
        let graph = json_obj(
            py,
            json!({"tool_name": "Edit", "tool_input": {"file_path": "pkg/mod.py"}}),
        );
        let note = json_obj(
            py,
            json!({"tool_name": "Edit", "tool_input": {"file_path": "notes.md"}}),
        );
        let expected = json_obj(
            py,
            json!({"hookSpecificOutput": {"hookEventName": "PostToolUse"}}),
        );
        assert!(body
            .getattr("hook_output")
            .unwrap()
            .call1((graph,))
            .unwrap()
            .eq(&expected)
            .unwrap());
        assert_eq!(*queued.lock().unwrap(), [vec!["pkg/mod.py"]]);
        assert!(body
            .getattr("hook_output")
            .unwrap()
            .call1((note,))
            .unwrap()
            .eq(&expected)
            .unwrap());
        assert_eq!(*queued.lock().unwrap(), [vec!["pkg/mod.py"]]);
        let which_signature = signature(py, &["name"], false);
        let missing = PyCFunction::new_closure(
            py,
            None,
            None,
            move |args, kwargs| -> PyResult<Option<String>> {
                bind_signature(&which_signature, args, kwargs)?;
                Ok(None)
            },
        )
        .unwrap();
        let _missing = AttrPatch::replace(&shutil, "which", missing.as_any());
        let result = body.getattr("full_update_output").unwrap().call0().unwrap();
        assert!(result
            .get_item("hookSpecificOutput")
            .unwrap()
            .get_item("additionalContext")
            .unwrap()
            .extract::<String>()
            .unwrap()
            .contains("not installed"));
    });
}

#[test]
fn status_and_doctor_report_a_stuck_lock() {
    let mut case = RefreshCase::new();
    case.case
        .set_env("CRG_DATA_DIR", case.store_root.to_str().unwrap());
    let _lock = case.hold_lock();
    Python::attach(|py| {
        let state = case.state(py);
        let store = case.store(py);
        let lock = store.getattr("lock").unwrap();
        let now: f64 = lock
            .call_method0("stat")
            .unwrap()
            .getattr("st_mtime")
            .unwrap()
            .extract()
            .unwrap();
        let options = PyDict::new(py);
        options.set_item("now", now).unwrap();
        assert!(state
            .getattr("status")
            .unwrap()
            .call((&store,), Some(&options))
            .unwrap()
            .get_item("stuck")
            .unwrap()
            .eq("")
            .unwrap());
        options.set_item("now", now + 301.0).unwrap();
        let stuck = state
            .getattr("status")
            .unwrap()
            .call((&store,), Some(&options))
            .unwrap();
        assert!(stuck
            .get_item("worker_alive")
            .unwrap()
            .extract::<bool>()
            .unwrap());
        assert!(stuck
            .get_item("stuck")
            .unwrap()
            .extract::<String>()
            .unwrap()
            .contains("held"));
        let doctor = module(py, "tooling.hooks.dispatch.doctor");
        let report = doctor
            .getattr("refresh_state_check")
            .unwrap()
            .call((path(py, case.case.root()),), Some(&options))
            .unwrap();
        assert!(report.getattr("status").unwrap().eq("DEAD").unwrap());
        assert!(report
            .getattr("problems")
            .unwrap()
            .get_item(0)
            .unwrap()
            .extract::<String>()
            .unwrap()
            .contains("stuck"));
        assert!(doctor
            .getattr("render_state")
            .unwrap()
            .call1((&report,))
            .unwrap()
            .extract::<String>()
            .unwrap()
            .starts_with("graph-refresh | DEAD"));
    });
}

#[test]
fn doctor_warns_on_a_waiting_failure_and_orphaned_pending() {
    let mut case = RefreshCase::new();
    case.case
        .set_env("CRG_DATA_DIR", case.store_root.to_str().unwrap());
    Python::attach(|py| {
        let state = case.state(py);
        let store = case.store(py);
        let doctor = module(py, "tooling.hooks.dispatch.doctor");
        let error = py
            .import("builtins")
            .unwrap()
            .getattr("OSError")
            .unwrap()
            .call1(("disk",))
            .unwrap();
        state
            .getattr("record_failure")
            .unwrap()
            .call1((&store, vec!["a.py"], error))
            .unwrap();
        let check = doctor.getattr("refresh_state_check").unwrap();
        assert!(check
            .call1((path(py, case.case.root()),))
            .unwrap()
            .getattr("status")
            .unwrap()
            .eq("WARN")
            .unwrap());
        fs::write(case.pending(), "a.py\n").unwrap();
        let old = SystemTime::now() - Duration::from_secs(60);
        fs::File::open(case.pending())
            .unwrap()
            .set_times(FileTimes::new().set_accessed(old).set_modified(old))
            .unwrap();
        let report = check.call1((path(py, case.case.root()),)).unwrap();
        assert!(report.getattr("status").unwrap().eq("DEAD").unwrap());
        assert!(report
            .getattr("problems")
            .unwrap()
            .get_item(0)
            .unwrap()
            .extract::<String>()
            .unwrap()
            .contains("no worker"));
    });
}

#[test]
fn detached_worker_drains_and_wait_sees_fresh() {
    let case = RefreshCase::new();
    let output = case.case.root().join("refreshed.txt");
    Python::attach(|py| {
        let state = case.state(py);
        let store = case.store(py);
        let argv = vec![
            child_bin().to_owned(),
            "drain".to_owned(),
            case.store_root.to_str().unwrap().to_owned(),
            output.to_str().unwrap().to_owned(),
        ];
        assert_eq!(
            request(py, &state, &store, &["a.py"], &argv, case.case.root()),
            "spawned"
        );
        request(py, &state, &store, &["b.py"], &argv, case.case.root());
        let options = PyDict::new(py);
        options.set_item("timeout", 10.0).unwrap();
        assert!(state
            .getattr("wait_for_fresh")
            .unwrap()
            .call((&store,), Some(&options))
            .unwrap()
            .eq("fresh")
            .unwrap());
        assert!(!case.pending().exists());
        assert!(!state
            .getattr("worker_alive")
            .unwrap()
            .call1((&store,))
            .unwrap()
            .extract::<bool>()
            .unwrap());
        let mut paths = fs::read_to_string(&output)
            .unwrap()
            .lines()
            .flat_map(|line| line.split(',').map(str::to_owned))
            .collect::<Vec<_>>();
        paths.sort();
        assert_eq!(paths, ["a.py", "b.py"]);
        let status = state.getattr("status").unwrap().call1((&store,)).unwrap();
        assert!(!status
            .get_item("failed_waiting")
            .unwrap()
            .extract::<bool>()
            .unwrap());
        let json = py.import("json").unwrap();
        let roundtrip = json
            .getattr("loads")
            .unwrap()
            .call1((json.getattr("dumps").unwrap().call1((status,)).unwrap(),))
            .unwrap();
        assert!(roundtrip.get_item("pending").unwrap().eq(0).unwrap());
    });
}

#[test]
fn hung_batch_child_is_killed_and_recorded() {
    let mut case = RefreshCase::new();
    case.setup_body();
    Python::attach(|py| {
        let (body, _patches) = case.body(py);
        let _command = patch_command(py, &body, "sleep", Some("5"));
        let timeout = json_obj(py, json!(0.3));
        let _timeout = AttrPatch::replace(body.as_any(), "BATCH_TIMEOUT_SECONDS", &timeout);
        let store = body_store(py, &body, &case);
        fs::write(case.pending(), "pkg/mod.py\n").unwrap();
        let started = Instant::now();
        let refresh = body.getattr("_refresh_batch").unwrap();
        let state = case.state(py);
        let options = PyDict::new(py);
        options.set_item("debounce", 0.0).unwrap();
        options.set_item("sleep", no_sleep(py)).unwrap();
        assert!(state
            .getattr("drain")
            .unwrap()
            .call((&store, refresh), Some(&options))
            .unwrap()
            .eq(1)
            .unwrap());
        assert!(started.elapsed() < Duration::from_secs(3));
        assert!(!state
            .getattr("worker_alive")
            .unwrap()
            .call1((&store,))
            .unwrap()
            .extract::<bool>()
            .unwrap());
        let notices = state
            .getattr("take_notices")
            .unwrap()
            .call1((&store,))
            .unwrap();
        assert_eq!(notices.len().unwrap(), 1);
        let notice = notices.get_item(0).unwrap();
        assert!(notice.get_item("kind").unwrap().eq("failure").unwrap());
        assert!(notice
            .get_item("text")
            .unwrap()
            .extract::<String>()
            .unwrap()
            .contains("was killed"));
    });
}

#[test]
fn batch_child_warning_reaches_the_next_hook_event() {
    let mut case = RefreshCase::new();
    case.setup_body();
    Python::attach(|py| {
        let (body, _patches) = case.body(py);
        let _command = patch_command(py, &body, "warn", None);
        body.getattr("_refresh_batch")
            .unwrap()
            .call1((vec!["pkg/mod.py"],))
            .unwrap();
        let output = body
            .getattr("failure_output")
            .unwrap()
            .call1(("PostToolUse",))
            .unwrap();
        let message: String = output.get_item("systemMessage").unwrap().extract().unwrap();
        assert!(message.contains("semantic search is stale"));
        assert!(!message.contains("FAILED"));
        assert!(body
            .getattr("failure_output")
            .unwrap()
            .call1(("PostToolUse",))
            .unwrap()
            .is_none());
    });
}

#[test]
fn batch_child_failure_is_recorded_not_lost() {
    let mut case = RefreshCase::new();
    case.setup_body();
    Python::attach(|py| {
        let (body, _patches) = case.body(py);
        let _command = patch_command(py, &body, "fail", None);
        let store = body_store(py, &body, &case);
        fs::write(case.pending(), "pkg/mod.py\n").unwrap();
        let state = case.state(py);
        let options = PyDict::new(py);
        options.set_item("debounce", 0.0).unwrap();
        options.set_item("sleep", no_sleep(py)).unwrap();
        assert!(state
            .getattr("drain")
            .unwrap()
            .call(
                (&store, body.getattr("_refresh_batch").unwrap()),
                Some(&options)
            )
            .unwrap()
            .eq(1)
            .unwrap());
        let output = body
            .getattr("failure_output")
            .unwrap()
            .call1(("PreToolUse",))
            .unwrap();
        let message: String = output.get_item("systemMessage").unwrap().extract().unwrap();
        assert!(message.contains("not installed"));
        assert!(message.contains("STALE"));
    });
}

#[test]
fn legacy_wiring_refreshes_synchronously() {
    let mut case = RefreshCase::new();
    case.setup_body();
    let marker = case.case.root().join("refreshed");
    Python::attach(|py| {
        let (body, _patches) = case.body(py);
        let _command = patch_command(py, &body, "write-marker", marker.to_str());
        let shutil = body.getattr("shutil").unwrap();
        let which_signature = signature(py, &["name"], false);
        let missing = PyCFunction::new_closure(
            py,
            None,
            None,
            move |args, kwargs| -> PyResult<Option<String>> {
                bind_signature(&which_signature, args, kwargs)?;
                Ok(None)
            },
        )
        .unwrap();
        let _which = AttrPatch::replace(&shutil, "which", missing.as_any());
        let sys = py.import("sys").unwrap();
        let _argv = AttrPatch::replace(
            &sys,
            "argv",
            PyList::new(py, ["crg_graph_refresh.py"]).unwrap().as_any(),
        );
        let payload = json!({"tool_name": "Edit", "tool_input": {"file_path": "pkg/mod.py"}});
        let io = py.import("io").unwrap();
        let input = io
            .getattr("StringIO")
            .unwrap()
            .call1((payload.to_string(),))
            .unwrap();
        let _stdin = AttrPatch::replace(&sys, "stdin", &input);
        let output = io.getattr("StringIO").unwrap().call0().unwrap();
        let _stdout = AttrPatch::replace(&sys, "stdout", &output);
        assert!(body
            .getattr("main")
            .unwrap()
            .call0()
            .unwrap()
            .eq(0)
            .unwrap());
        assert!(marker.is_file());
        assert!(!case.pending().exists());
        let captured: String = output.call_method0("getvalue").unwrap().extract().unwrap();
        let parsed = py
            .import("json")
            .unwrap()
            .getattr("loads")
            .unwrap()
            .call1((captured,))
            .unwrap();
        assert!(parsed
            .eq(json_obj(
                py,
                json!({"hookSpecificOutput": {"hookEventName": "PostToolUse"}})
            ))
            .unwrap());
        drop(_command);
        let _failed = patch_command(py, &body, "fail", None);
        let context = body
            .getattr("sync_output")
            .unwrap()
            .call1((json_obj(py, payload),))
            .unwrap()
            .get_item("hookSpecificOutput")
            .unwrap()
            .get_item("additionalContext")
            .unwrap()
            .extract::<String>()
            .unwrap();
        assert!(context.contains("FAILED") && context.contains("not installed"));
    });
}
