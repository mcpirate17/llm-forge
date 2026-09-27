#![cfg(feature = "python-compat-tests")]
//! Dispatch selection, native splice, and host wire-protocol contracts.

#[path = "python_contracts/agent_comm_support.rs"]
#[allow(dead_code)]
mod comm_support;
#[path = "python_contracts/dispatch_runner_support.rs"]
#[allow(dead_code)]
mod runner_support;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use comm_support::{buffer_text, capture, py_json};
use pyo3::exceptions::PyZeroDivisionError;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyBytes, PyCFunction, PyDict, PyList, PyTuple};
use runner_support::{
    adapters, callback, case, deny, entry, outcome_names, output, runner, spec, IoRestore,
    OptionalPatch, PAYLOAD,
};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use support::{module, path, AttrPatch};

fn eq(value: &Bound<'_, PyAny>, expected: &Bound<'_, PyAny>) {
    assert!(value.eq(expected).unwrap());
}
fn names(value: &Bound<'_, PyAny>) -> Vec<String> {
    outcome_names(value)
}
fn select(py: Python<'_>, event: &str, body: Value) -> Vec<String> {
    let result = runner(py)
        .getattr("select")
        .unwrap()
        .call1((event, output(py, body)))
        .unwrap();
    names(&result)
}
fn dispatch<'py>(
    py: Python<'py>,
    event: &str,
    raw: &[u8],
    root: &std::path::Path,
) -> Bound<'py, PyAny> {
    runner(py)
        .getattr("dispatch")
        .unwrap()
        .call1((event, PyBytes::new(py, raw), path(py, root)))
        .unwrap()
}
fn outcome<'py>(
    py: Python<'py>,
    name: &str,
    body: Option<Value>,
    error: Option<&str>,
) -> Bound<'py, PyAny> {
    let kwargs = PyDict::new(py);
    kwargs.set_item("name", name).unwrap();
    kwargs
        .set_item("output", body.map(|v| py_json(py, v)))
        .unwrap();
    if let Some(error) = error {
        kwargs.set_item("error", error).unwrap();
    }
    kwargs.set_item("elapsed_ms", 1.0).unwrap();
    runner(py)
        .getattr("HookOutcome")
        .unwrap()
        .call((), Some(&kwargs))
        .unwrap()
}

#[test]
fn select_uses_registry_matchers() {
    let _case = case();
    Python::attach(|py| {
        assert_eq!(
            select(
                py,
                "PreToolUse",
                json!({"session_id":"t","tool_name":"Bash","tool_input":{"command":"echo hi"}})
            ),
            [
                "crg_refresh_report_pre",
                "crg_gate_verify_bash",
                "pre_bash",
                "current_work_guard_bash"
            ]
        );
        assert_eq!(
            select(py, "SessionStart", json!({"source":"resume"})),
            [
                "crg_refresh_report_session",
                "session_start",
                "workspace_exposure_session",
                "session_handoff",
                "native_freshness"
            ]
        );
        assert_eq!(
            select(py, "PreToolUse", json!({"tool_name":"Glob"})),
            ["crg_refresh_report_pre"]
        );
        assert_eq!(
            select(
                py,
                "PreToolUse",
                json!({"tool_name":"mcp__code-review-graph__locate_tool"})
            ),
            [
                "crg_gate_mark",
                "crg_refresh_wait",
                "crg_refresh_report_pre"
            ]
        );
    });
}

#[test]
fn select_drops_hooks_named_in_forge_native_hooks() {
    let mut case = case();
    case.set_env("FORGE_NATIVE_HOOKS", " pre_bash ,,current_work_guard_bash");
    Python::attach(|py| {
        assert_eq!(
            select(
                py,
                "PreToolUse",
                json!({"session_id":"t","tool_name":"Bash","tool_input":{"command":"echo hi"}})
            ),
            ["crg_refresh_report_pre", "crg_gate_verify_bash"]
        )
    });
}

#[test]
fn select_runs_everything_when_forge_native_hooks_is_unset() {
    let mut case = case();
    case.remove_env("FORGE_NATIVE_HOOKS");
    Python::attach(|py| {
        assert_eq!(
            select(
                py,
                "PreToolUse",
                json!({"session_id":"t","tool_name":"Bash","tool_input":{"command":"echo hi"}})
            ),
            [
                "crg_refresh_report_pre",
                "crg_gate_verify_bash",
                "pre_bash",
                "current_work_guard_bash"
            ]
        )
    });
}

#[test]
fn dispatch_merges_and_reports_errors() {
    let case = case();
    Python::attach(|py| {
        let _io = IoRestore::new(py);
        let deny_cb = callback(py, "ctx", |py, _ctx| Ok(output(py, deny()).unbind()));
        let boom = callback(py, "ctx", |_py, _ctx| {
            Err(PyZeroDivisionError::new_err("division by zero"))
        });
        let _deny = OptionalPatch::new(adapters(py).as_any(), "deny", deny_cb.as_any());
        let _boom = OptionalPatch::new(adapters(py).as_any(), "boom", boom.as_any());
        let specs = PyTuple::new(
            py,
            [
                spec(py, "deny", Some("deny"), &[], 5),
                spec(py, "boom", Some("boom"), &[], 5),
            ],
        )
        .unwrap()
        .unbind();
        let hooks = callback(py, "event", move |py, _event| {
            Ok(specs.clone_ref(py).into_any())
        });
        let _patch = AttrPatch::replace(runner(py).as_any(), "hooks_for", hooks.as_any());
        let pair = dispatch(py, "PreToolUse", PAYLOAD.as_bytes(), case.root());
        let result = pair.get_item(0).unwrap();
        let outcomes = pair.get_item(1).unwrap();
        assert_eq!(
            result
                .get_item("hookSpecificOutput")
                .unwrap()
                .get_item("permissionDecision")
                .unwrap()
                .extract::<String>()
                .unwrap(),
            "deny"
        );
        assert!(result
            .get_item("systemMessage")
            .unwrap()
            .extract::<String>()
            .unwrap()
            .contains("HOOK ERROR [boom]: ZeroDivisionError"));
        assert_eq!(
            std::env::var("PROJECT_DIR").unwrap(),
            case.root().display().to_string()
        );
        assert_eq!(outcomes.len().unwrap(), 2);
    });
}

#[test]
fn malformed_payload_dispatches_with_empty_payload() {
    let case = case();
    Python::attach(|py| {
        let _io = IoRestore::new(py);
        let hooks = callback(py, "event", |py, _event| {
            Ok(PyTuple::empty(py).unbind().into_any())
        });
        let _patch = AttrPatch::replace(runner(py).as_any(), "hooks_for", hooks.as_any());
        let pair = dispatch(py, "PostToolUse", b"not json", case.root());
        eq(
            &pair.get_item(0).unwrap(),
            &output(
                py,
                json!({"hookSpecificOutput":{"hookEventName":"PostToolUse"}}),
            ),
        );
        eq(&pair.get_item(1).unwrap(), PyList::empty(py).as_any());
    });
}

fn normalize<'py>(py: Python<'py>, host: &str, event: &str, body: Value) -> Bound<'py, PyAny> {
    entry(py)
        .getattr("_normalize_output")
        .unwrap()
        .call1((host, event, output(py, body)))
        .unwrap()
}

#[test]
fn codex_pretooluse_normalization_is_host_specific() {
    let _case = case();
    Python::attach(|py| {
        let allow = json!({"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"allow","permissionDecisionReason":"benign"}});
        eq(
            &normalize(py, "claude", "PreToolUse", allow.clone()),
            &output(py, allow.clone()),
        );
        eq(
            &normalize(py, "codex", "PreToolUse", allow.clone()),
            &output(py, json!({})),
        );
        let with_message = json!({"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"allow","permissionDecisionReason":"benign"},"systemMessage":"kept","ignored":true});
        eq(
            &normalize(py, "codex", "PreToolUse", with_message),
            &output(py, json!({"systemMessage":"kept"})),
        );
        let rewrite = json!({"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"allow","updatedInput":{"command":"echo rewritten"}}});
        eq(
            &normalize(py, "codex", "PreToolUse", rewrite.clone()),
            &output(py, rewrite),
        );
        let deny = json!({"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"blocked"}});
        eq(
            &normalize(py, "codex", "PreToolUse", deny.clone()),
            &output(py, deny),
        );
    });
}

#[test]
fn codex_pretooluse_turns_unsupported_ask_into_deny() {
    let _case = case();
    Python::attach(|py| {
        let result = normalize(
            py,
            "codex",
            "PreToolUse",
            json!({"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"ask","permissionDecisionReason":"review this"}}),
        );
        let specific = result.get_item("hookSpecificOutput").unwrap();
        assert_eq!(
            specific
                .get_item("permissionDecision")
                .unwrap()
                .extract::<String>()
                .unwrap(),
            "deny"
        );
        assert_eq!(
            specific
                .get_item("permissionDecisionReason")
                .unwrap()
                .extract::<String>()
                .unwrap(),
            "review this"
        );
        let result = normalize(
            py,
            "codex",
            "PreToolUse",
            json!({"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"ask"}}),
        );
        assert_eq!(
            result
                .get_item("hookSpecificOutput")
                .unwrap()
                .get_item("permissionDecisionReason")
                .unwrap()
                .extract::<String>()
                .unwrap(),
            "Hook requested approval, but Codex PreToolUse hooks do not support ask."
        );
    });
}

#[test]
fn codex_posttooluse_drops_unsupported_rewrites_and_suppression() {
    let _case = case();
    Python::attach(|py| {
        eq(
            &normalize(
                py,
                "codex",
                "PostToolUse",
                json!({"hookSpecificOutput":{"hookEventName":"PostToolUse","updatedToolOutput":{"output":"bounded"},"updatedMCPToolOutput":{"content":[]}},"suppressOutput":true,"systemMessage":"kept"}),
            ),
            &output(py, json!({"systemMessage":"kept"})),
        );
        let supported = json!({"hookSpecificOutput":{"hookEventName":"PostToolUse","additionalContext":"reviewed"},"decision":"block","reason":"needs review","continue":false,"stopReason":"replace output"});
        eq(
            &normalize(py, "codex", "PostToolUse", supported.clone()),
            &output(py, supported),
        );
    });
}

#[test]
fn main_applies_explicit_codex_protocol_before_stdout() {
    let mut case = case();
    case.set_env("HOOK_DISPATCH_TRACE", "1");
    Python::attach(|py| {
        let _io = IoRestore::new(py);
        let sys = module(py, "sys");
        let io = module(py, "io");
        let raw = PyBytes::new(py, b"{\"turn_id\":\"turn-test\"}");
        let input = io
            .getattr("TextIOWrapper")
            .unwrap()
            .call1((io.getattr("BytesIO").unwrap().call1((raw,)).unwrap(),))
            .unwrap();
        let _stdin = AttrPatch::replace(sys.as_any(), "stdin", &input);
        let (stdout, _stdout) = capture(py, "stdout");
        let (stderr, _stderr) = capture(py, "stderr");
        let roots = Arc::new(Mutex::new(Vec::<String>::new()));
        let saved = Arc::clone(&roots);
        let result = json!({"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"allow"}});
        let expected_root = case.root().canonicalize().unwrap();
        let dispatch_cb =
            PyCFunction::new_closure(py, None, None, move |args, kw| -> PyResult<Py<PyAny>> {
                let sig = comm_support::signature(args.py(), &["event", "payload", "root"], &[]);
                let bound = comm_support::bind_signature(&sig, args, kw)?;
                let values = bound.getattr("arguments")?;
                let root = values.get_item("root")?;
                let pathlib = module(args.py(), "pathlib");
                assert!(root.is_instance(&pathlib.getattr("Path")?)?);
                assert!(root.eq(path(args.py(), &expected_root))?);
                saved.lock().unwrap().push(root.str()?.to_str()?.to_owned());
                let rows = PyList::new(
                    args.py(),
                    [
                        outcome(args.py(), "json-hook", Some(json!({"ok":true})), None),
                        outcome(args.py(), "quiet-hook", None, None),
                        outcome(args.py(), "error-hook", None, Some("boom")),
                    ],
                )?;
                Ok(PyTuple::new(
                    args.py(),
                    [output(args.py(), result.clone()), rows.into_any()],
                )?
                .into_any()
                .unbind())
            })
            .unwrap();
        let _dispatch = AttrPatch::replace(runner(py).as_any(), "dispatch", dispatch_cb.as_any());
        let timings = PyCFunction::new_closure(py, None, None, |args, kw| -> PyResult<Py<PyAny>> {
            if kw.is_some_and(|kw| !kw.is_empty()) {
                return Err(pyo3::exceptions::PyTypeError::new_err("unexpected keyword"));
            }
            Ok(args.py().None())
        })
        .unwrap();
        let _timings =
            AttrPatch::replace(entry(py).as_any(), "_record_hook_timings", timings.as_any());
        let argv = [
            "PreToolUse",
            "--protocol",
            "codex",
            "--project-dir",
            case.root().to_str().unwrap(),
        ];
        assert_eq!(
            entry(py)
                .getattr("main")
                .unwrap()
                .call1((argv,))
                .unwrap()
                .extract::<i32>()
                .unwrap(),
            0
        );
        assert_eq!(
            serde_json::from_str::<Value>(&buffer_text(&stdout)).unwrap(),
            json!({})
        );
        assert!(buffer_text(&stdout).ends_with('\n'));
        assert_eq!(
            *roots.lock().unwrap(),
            vec![case.root().display().to_string()]
        );
        let trace = buffer_text(&stderr);
        for (name, status) in [
            ("json-hook", "json"),
            ("quiet-hook", "quiet"),
            ("error-hook", "boom"),
        ] {
            assert!(trace
                .lines()
                .any(|line| line.contains(&format!("[dispatch] {name}"))
                    && line.ends_with(&format!("  {status}"))));
        }
    });
}

#[test]
fn main_settings_prints_the_registry_block() {
    let _case = case();
    Python::attach(|py| {
        let _io = IoRestore::new(py);
        let (stdout, _stdout) = capture(py, "stdout");
        let settings =
            runner_support::no_args(py, |py| Ok(output(py, json!({"hooks":"ok"})).unbind()));
        let _patch = AttrPatch::replace(entry(py).as_any(), "settings_block", settings.as_any());
        assert_eq!(
            entry(py)
                .getattr("main")
                .unwrap()
                .call1((["settings"],))
                .unwrap()
                .extract::<i32>()
                .unwrap(),
            0
        );
        assert_eq!(
            serde_json::from_str::<Value>(&buffer_text(&stdout)).unwrap(),
            json!({"hooks":"ok"})
        );
    });
}

#[test]
fn record_hook_timings_labels_quiet_outcomes() {
    let _case = case();
    let events = Arc::new(Mutex::new(Vec::<Py<PyAny>>::new()));
    Python::attach(|py| {
        let captured = Arc::clone(&events);
        let recorder =
            PyCFunction::new_closure(py, None, None, move |args, kw| -> PyResult<Py<PyAny>> {
                let sig = comm_support::signature(args.py(), &["event", "path"], &[]);
                let bound = comm_support::bind_signature(&sig, args, kw)?;
                captured
                    .lock()
                    .unwrap()
                    .push(bound.getattr("arguments")?.get_item("event")?.unbind());
                Ok(args.py().None())
            })
            .unwrap();
        let telemetry = module(py, "conductor.context_telemetry");
        let _patch = AttrPatch::replace(telemetry.as_any(), "record", recorder.as_any());
        let rows = PyList::new(py, [outcome(py, "quiet-hook", None, None)]).unwrap();
        entry(py)
            .getattr("_record_hook_timings")
            .unwrap()
            .call1(("PostToolUse", rows, "session"))
            .unwrap();
        assert_eq!(
            events.lock().unwrap()[0]
                .bind(py)
                .get_item("status")
                .unwrap()
                .extract::<String>()
                .unwrap(),
            "quiet"
        );
    });
}

struct Splice {
    _patches: Vec<OptionalPatch>,
    _registry: AttrPatch,
    calls: Arc<Mutex<Vec<String>>>,
}

fn splice(py: Python<'_>) -> Splice {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let mut patches = Vec::new();
    for name in ["a", "pre_bash", "b"] {
        let saved = Arc::clone(&calls);
        let cb = PyCFunction::new_closure(py, None, None, move |args, kw| -> PyResult<Py<PyAny>> {
            // Original `lambda ctx, name=name`: ctx is required, name may be overridden.
            let inspect = module(args.py(), "inspect");
            let parameter = inspect.getattr("Parameter")?;
            let kind = parameter.getattr("POSITIONAL_OR_KEYWORD")?;
            let required = parameter.call1(("ctx", &kind))?;
            let options = PyDict::new(args.py());
            options.set_item("default", name)?;
            let defaulted = parameter.call(("name", &kind), Some(&options))?;
            let signature = inspect
                .getattr("Signature")?
                .call1((PyList::new(args.py(), [required, defaulted])?,))?;
            let bound = signature.call_method("bind", args, kw)?;
            bound.call_method0("apply_defaults")?;
            let called: String = bound.getattr("arguments")?.get_item("name")?.extract()?;
            saved.lock().unwrap().push(called.clone());
            Ok(output(args.py(), json!({"ok":called})).unbind())
        })
        .unwrap();
        patches.push(OptionalPatch::new(adapters(py).as_any(), name, cb.as_any()));
    }
    let specs = PyTuple::new(
        py,
        [
            spec(py, "a", Some("a"), &[], 5),
            spec(py, "pre_bash", Some("pre_bash"), &[], 5),
            spec(py, "b", Some("b"), &[], 5),
        ],
    )
    .unwrap()
    .unbind();
    let hook = callback(py, "event", move |py, _event| {
        Ok(specs.clone_ref(py).into_any())
    });
    let registry = AttrPatch::replace(runner(py).as_any(), "hooks_for", hook.as_any());
    Splice {
        _patches: patches,
        _registry: registry,
        calls,
    }
}

#[test]
fn dispatch_splices_a_native_answer_back_in_at_its_registry_position() {
    let mut case = case();
    let native = json!({"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"native deny"}});
    case.set_env("FORGE_NATIVE_HOOKS", "pre_bash");
    case.set_env(
        "FORGE_NATIVE_ANSWERS",
        &json!({"pre_bash":native}).to_string(),
    );
    Python::attach(|py| {
        let _io = IoRestore::new(py);
        let fixture = splice(py);
        let pair = dispatch(py, "PreToolUse", PAYLOAD.as_bytes(), case.root());
        let result = pair.get_item(0).unwrap();
        let outcomes = pair.get_item(1).unwrap();
        assert_eq!(names(&outcomes), ["a", "pre_bash", "b"]);
        let pre = outcomes.get_item(1).unwrap();
        eq(&pre.getattr("output").unwrap(), &output(py, native));
        assert!(pre.getattr("error").unwrap().is_none());
        assert_eq!(*fixture.calls.lock().unwrap(), ["a", "b"]);
        assert_eq!(
            result
                .get_item("hookSpecificOutput")
                .unwrap()
                .get_item("permissionDecision")
                .unwrap()
                .extract::<String>()
                .unwrap(),
            "deny"
        );
    });
}

#[test]
fn dispatch_raises_loud_when_a_served_hook_has_no_native_answer() {
    let mut case = case();
    case.set_env("FORGE_NATIVE_HOOKS", "pre_bash");
    case.remove_env("FORGE_NATIVE_ANSWERS");
    Python::attach(|py| {
        let _io = IoRestore::new(py);
        let _fixture = splice(py);
        let error = runner(py)
            .getattr("dispatch")
            .unwrap()
            .call1((
                "PreToolUse",
                PyBytes::new(py, PAYLOAD.as_bytes()),
                path(py, case.root()),
            ))
            .unwrap_err();
        assert!(error.is_instance_of::<pyo3::exceptions::PyValueError>(py));
        assert!(error.to_string().contains("pre_bash"));
    });
}

#[test]
fn dispatch_runs_everything_in_python_when_forge_native_hooks_is_empty() {
    let mut case = case();
    case.set_env("FORGE_NATIVE_HOOKS", "");
    case.remove_env("FORGE_NATIVE_ANSWERS");
    Python::attach(|py| {
        let _io = IoRestore::new(py);
        let fixture = splice(py);
        let pair = dispatch(py, "PreToolUse", PAYLOAD.as_bytes(), case.root());
        assert_eq!(*fixture.calls.lock().unwrap(), ["a", "pre_bash", "b"]);
        assert_eq!(names(&pair.get_item(1).unwrap()), ["a", "pre_bash", "b"]);
    });
}
