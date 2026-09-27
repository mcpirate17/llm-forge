#![cfg(feature = "python-compat-tests")]
//! Dispatch runner execution contracts. Fixtures and assertions live in Rust.

#[path = "python_contracts/agent_comm_support.rs"]
#[allow(dead_code)]
mod comm_support;
#[path = "python_contracts/dispatch_runner_support.rs"]
#[allow(dead_code)]
mod runner_support;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::exceptions::{PyRuntimeError, PySystemExit};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyList, PyTuple};
use runner_support::{
    adapters, callback, case, context, no_args, output, runner, spec, standard_context, IoRestore,
    OptionalPatch,
};
use serde_json::json;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use support::{module, path};

fn run_one<'py>(
    py: Python<'py>,
    name: &str,
    root: &std::path::Path,
    timeout: i32,
    argv: &[&str],
) -> Bound<'py, PyAny> {
    runner(py)
        .getattr("run_one")
        .unwrap()
        .call1((
            spec(
                py,
                name,
                if argv.is_empty() { Some(name) } else { None },
                argv,
                timeout,
            ),
            standard_context(py, root),
        ))
        .unwrap()
}

fn equals(value: &Bound<'_, PyAny>, expected: &Bound<'_, PyAny>) {
    assert!(value.eq(expected).unwrap());
}
fn field<'py>(value: &Bound<'py, PyAny>, name: &str) -> Bound<'py, PyAny> {
    value.getattr(name).unwrap()
}

#[test]
fn raising_adapter_is_a_visible_error() {
    let case = case();
    Python::attach(|py| {
        let _io = IoRestore::new(py);
        let boom = callback(py, "ctx", |_py, _ctx| Err(PyRuntimeError::new_err("kaput")));
        let _patch = OptionalPatch::new(adapters(py).as_any(), "boom", boom.as_any());
        let result = run_one(py, "boom", case.root(), 5, &[]);
        assert!(field(&result, "output").is_none());
        assert_eq!(
            field(&result, "error").extract::<String>().unwrap(),
            "RuntimeError: kaput"
        );
    });
}

#[test]
fn print_from_unbound_helper_thread_raises() {
    let case = case();
    let seen = Arc::new(Mutex::new(Vec::<(String, String)>::new()));
    Python::attach(|py| {
        let _io = IoRestore::new(py);
        let captured = Arc::clone(&seen);
        let helper = no_args(py, move |py| {
            let print = py.import("builtins")?.getattr("print")?;
            if let Err(error) = print.call1(("lost json",)) {
                let class: String = error.get_type(py).getattr("__name__")?.extract()?;
                let message = error.value(py).str()?.to_str()?.to_owned();
                captured.lock().unwrap().push((class, message));
            }
            Ok(py.None())
        });
        let helper = helper.unbind();
        let spawn = callback(py, "ctx", move |py, _ctx| {
            let threading = py.import("threading")?;
            let kwargs = pyo3::types::PyDict::new(py);
            kwargs.set_item("target", helper.bind(py))?;
            let worker = threading.getattr("Thread")?.call((), Some(&kwargs))?;
            worker.call_method0("start")?;
            worker.call_method0("join")?;
            Ok(output(py, json!({"hookSpecificOutput":{}})).unbind())
        });
        let _patch = OptionalPatch::new(adapters(py).as_any(), "spawn", spawn.as_any());
        let result = run_one(py, "spawn", case.root(), 5, &[]);
        assert!(field(&result, "error").is_none());
    });
    let records = seen.lock().unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].0, "RuntimeError");
    assert!(records[0].1.contains("no bound buffer"));
}

#[test]
fn adapter_dict_return_is_used_verbatim() {
    let case = case();
    Python::attach(|py| {
        let _io = IoRestore::new(py);
        let give = callback(py, "ctx", |py, _ctx| {
            Ok(output(
                py,
                json!({"hookSpecificOutput":{"permissionDecision":"deny"}}),
            )
            .unbind())
        });
        let _patch = OptionalPatch::new(adapters(py).as_any(), "give", give.as_any());
        let result = run_one(py, "give", case.root(), 5, &[]);
        equals(
            &field(&result, "output"),
            &output(
                py,
                json!({"hookSpecificOutput":{"permissionDecision":"deny"}}),
            ),
        );
        assert!(field(&result, "error").is_none());
    });
}

#[test]
fn printing_adapter_stdout_is_captured_per_thread() {
    let case = case();
    Python::attach(|py| {
        let _io = IoRestore::new(py);
        let printer = callback(py, "ctx", |py, _ctx| {
            let sys = py.import("sys")?;
            let json = py.import("json")?;
            let payload = json.getattr("load")?.call1((sys.getattr("stdin")?,))?;
            let command = payload.get_item("tool_input")?.get_item("command")?;
            let result = output(py, json!({"seen":command.extract::<String>()?}));
            let text = json.getattr("dumps")?.call1((result,))?;
            py.import("builtins")?.getattr("print")?.call1((text,))?;
            Ok(py.None())
        });
        let _patch = OptionalPatch::new(adapters(py).as_any(), "printer", printer.as_any());
        let one = spec(py, "printer", Some("printer"), &[], 5);
        let specs = PyTuple::new(py, [&one, &one, &one]).unwrap();
        let outcomes = runner(py)
            .getattr("run_all")
            .unwrap()
            .call1((specs, standard_context(py, case.root())))
            .unwrap();
        assert_eq!(outcomes.len().unwrap(), 3);
        let expected = output(py, json!({"seen":"echo hi"}));
        for outcome in outcomes.try_iter().unwrap() {
            let outcome = outcome.unwrap();
            equals(&field(&outcome, "output"), &expected);
            assert!(field(&outcome, "error").is_none());
        }
    });
}

#[test]
fn non_json_stdout_is_an_error() {
    let case = case();
    Python::attach(|py| {
        let _io = IoRestore::new(py);
        let noisy = callback(py, "ctx", |py, _ctx| {
            py.import("builtins")?
                .getattr("print")?
                .call1(("Traceback (most recent call last)",))?;
            Ok(py.None())
        });
        let _patch = OptionalPatch::new(adapters(py).as_any(), "noisy", noisy.as_any());
        let result = run_one(py, "noisy", case.root(), 5, &[]);
        assert!(field(&result, "output").is_none());
        let error = field(&result, "error");
        assert!(!error.is_none());
        assert!(error.extract::<String>().unwrap().contains("not JSON"));
    });
}

#[test]
fn nonzero_system_exit_is_an_error_but_zero_is_not() {
    let case = case();
    Python::attach(|py| {
        let _io = IoRestore::new(py);
        let zero = callback(py, "ctx", |_py, _ctx| Err(PySystemExit::new_err(0)));
        let three = callback(py, "ctx", |_py, _ctx| Err(PySystemExit::new_err(3)));
        let _p0 = OptionalPatch::new(adapters(py).as_any(), "exit0", zero.as_any());
        let _p3 = OptionalPatch::new(adapters(py).as_any(), "exit3", three.as_any());
        assert!(field(&run_one(py, "exit0", case.root(), 5, &[]), "error").is_none());
        assert_eq!(
            field(&run_one(py, "exit3", case.root(), 5, &[]), "error")
                .extract::<String>()
                .unwrap(),
            "SystemExit(3)"
        );
    });
}

#[test]
fn slow_adapter_times_out_without_blocking_the_event() {
    let case = case();
    Python::attach(|py| {
        let _io = IoRestore::new(py);
        let slow = callback(py, "ctx", |py, _ctx| {
            py.import("time")?.getattr("sleep")?.call1((3,))?;
            Ok(py.None())
        });
        let fast = callback(py, "ctx", |py, _ctx| {
            Ok(output(py, json!({"hookSpecificOutput":{}})).unbind())
        });
        let _slow = OptionalPatch::new(adapters(py).as_any(), "slow", slow.as_any());
        let _fast = OptionalPatch::new(adapters(py).as_any(), "fast", fast.as_any());
        let specs = PyTuple::new(
            py,
            [
                spec(py, "slow", Some("slow"), &[], 1),
                spec(py, "fast", Some("fast"), &[], 5),
            ],
        )
        .unwrap();
        let started = Instant::now();
        let outcomes = runner(py)
            .getattr("run_all")
            .unwrap()
            .call1((specs, standard_context(py, case.root())))
            .unwrap();
        assert!(started.elapsed() < Duration::from_millis(2500));
        let rows = outcomes.cast::<PyList>().unwrap();
        assert_eq!(
            field(&rows.get_item(0).unwrap(), "error")
                .extract::<String>()
                .unwrap(),
            "timed out after 1s"
        );
        equals(
            &field(&rows.get_item(1).unwrap(), "output"),
            &output(py, json!({"hookSpecificOutput":{}})),
        );
    });
}

fn script(root: &std::path::Path, name: &str, contents: &str) {
    let file = root.join(name);
    fs::write(&file, contents).unwrap();
    let mode = fs::metadata(&file).unwrap().permissions().mode();
    fs::set_permissions(&file, fs::Permissions::from_mode(mode | 0o100)).unwrap();
}

#[test]
fn subprocess_python_body_gets_payload_and_env() {
    let case = case();
    script(case.root(),"body.py","import json, os, sys\np = json.load(sys.stdin)\nprint(json.dumps({'cmd': p['tool_input']['command'], 'root': os.environ['PROJECT_DIR']}))\n");
    Python::attach(|py| {
        let result = run_one(py, "body", case.root(), 5, &["body.py"]);
        assert!(field(&result, "error").is_none());
        equals(
            &field(&result, "output"),
            &output(
                py,
                json!({"cmd":"echo hi","root":case.root().display().to_string()}),
            ),
        );
    });
}

#[test]
fn subprocess_nonzero_exit_is_an_error() {
    let case = case();
    script(
        case.root(),
        "bad.sh",
        "#!/bin/bash\necho nope >&2\nexit 7\n",
    );
    Python::attach(|py| {
        let result = run_one(py, "bad", case.root(), 5, &["bad.sh"]);
        assert!(field(&result, "output").is_none());
        assert_eq!(
            field(&result, "error").extract::<String>().unwrap(),
            "exit 7: nope"
        );
    });
}

#[test]
fn subprocess_timeout_is_an_error() {
    let case = case();
    script(case.root(), "sleep.sh", "#!/bin/bash\nsleep 5\n");
    Python::attach(|py| {
        let result = run_one(py, "sleepy", case.root(), 1, &["sleep.sh"]);
        assert_eq!(
            field(&result, "error").extract::<String>().unwrap(),
            "timed out after 1s"
        );
    });
}

#[test]
fn workspace_exposure_adapter_emits_exposure_line_verbatim() {
    let case = case();
    Python::attach(|py| {
        let ctx = context(py, "SessionStart", json!({"source":"startup"}), case.root());
        let actual = adapters(py)
            .getattr("workspace_exposure")
            .unwrap()
            .call1((ctx,))
            .unwrap();
        let line = module(py, "conductor.workspace_hygiene")
            .getattr("exposure_line")
            .unwrap()
            .call1((path(py, case.root()),))
            .unwrap();
        let expected = output(
            py,
            json!({"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":line.extract::<String>().unwrap()}}),
        );
        equals(&actual, &expected);
        assert!(actual
            .get_item("hookSpecificOutput")
            .unwrap()
            .get_item("additionalContext")
            .unwrap()
            .extract::<String>()
            .unwrap()
            .starts_with("EXPOSED:"));
    });
}
