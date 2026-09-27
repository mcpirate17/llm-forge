#![cfg(feature = "python-compat-tests")]
//! Rust fixtures and assertions for the Python session-start trust boundary.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyModule};
use serde_json::{json, Value};
use std::fs;
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};
use support::{assert_error, module, path, text, Case};

fn kwargs<'py>(py: Python<'py>, identity: &str, state: &Path) -> Bound<'py, PyDict> {
    let args = PyDict::new(py);
    args.set_item("identity", identity).unwrap();
    args.set_item("state_dir", path(py, state)).unwrap();
    args
}

fn python_json<'py>(py: Python<'py>, value: &Value) -> Bound<'py, PyAny> {
    PyModule::import(py, "json")
        .unwrap()
        .getattr("loads")
        .unwrap()
        .call1((value.to_string(),))
        .unwrap()
}

fn valid_envelope() -> Value {
    json!({
        "schema_version": 1,
        "authority": "bounded-a2a-inbox",
        "agent": "one",
        "unread_only": true,
        "total": 1,
        "shown": 1,
        "omitted": 0,
        "raw_bytes_not_injected": 12,
        "messages": [{
            "id": "00000000-0000-4000-8000-000000000001",
            "from": "peer",
            "at": "2026-08-30T12:00:00+00:00",
            "thread": "thread-1",
            "status": "open",
            "requires_response": true,
            "summary": "short",
            "raw_bytes": 12
        }]
    })
}

fn validate(py: Python<'_>, startup: &Bound<'_, PyModule>, value: &Value) -> PyResult<Py<PyAny>> {
    let args = PyDict::new(py);
    args.set_item("identity", "one").unwrap();
    args.set_item("max_messages", 1).unwrap();
    args.set_item("preview_chars", 140).unwrap();
    startup
        .getattr("validate_compact_envelope")?
        .call((python_json(py, value),), Some(&args))
        .map(Bound::unbind)
}

#[test]
fn identity_commands_and_bounds_keep_startup_scoped() {
    let case = Case::new();
    Python::attach(|py| {
        let startup = module(py, "conductor.a2a_session_start");
        let error_class = startup.getattr("SessionStartError").unwrap();
        let resolve = startup.getattr("resolve_identity").unwrap();
        let environ = PyDict::new(py);
        environ.set_item("A2A_AGENT_NAME", " codex-env ").unwrap();
        assert_eq!(
            text(&resolve.call1(("codex-explicit", &environ)).unwrap()),
            "codex-explicit"
        );
        assert_eq!(
            text(&resolve.call1((py.None(), &environ)).unwrap()),
            "codex-env"
        );
        assert_error(
            py,
            resolve.call1((py.None(), PyDict::new(py))).unwrap_err(),
            &error_class,
            "identity missing",
        );
        assert_error(
            py,
            resolve.call1(("bad identity", &environ)).unwrap_err(),
            &error_class,
            "invalid A2A identity",
        );

        let args = kwargs(py, "codex-efficiency", case.root());
        args.set_item("interpreter", "python-current").unwrap();
        let compact: Vec<String> = startup
            .getattr("compact_inbox_command")
            .unwrap()
            .call((), Some(&args))
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(
            compact,
            vec![
                "python-current",
                "-m",
                "conductor.agent_a2a",
                "--state-dir",
                case.root().to_str().unwrap(),
                "inbox",
                "--as-name",
                "codex-efficiency",
                "--unread",
                "--compact",
                "--max-messages",
                "8",
                "--preview-chars",
                "140",
                "--max-chars",
                "1200",
                "--json"
            ]
        );
        assert!(!compact
            .iter()
            .any(|arg| arg == "--full" || arg == "--limit"));
        for (field, value) in [
            ("max_messages", 0),
            ("max_messages", 9),
            ("preview_chars", 31),
            ("preview_chars", 141),
            ("max_chars", 255),
            ("max_chars", 1201),
        ] {
            args.set_item(field, value).unwrap();
            assert_error(
                py,
                startup
                    .getattr("compact_inbox_command")
                    .unwrap()
                    .call((), Some(&args))
                    .unwrap_err(),
                &error_class,
                field,
            );
            args.del_item(field).unwrap();
        }
        let flush: Vec<String> = startup
            .getattr("sender_flush_command")
            .unwrap()
            .call((), Some(&args))
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(
            &flush[flush.len() - 3..],
            ["flush", "--as-name", "codex-efficiency"]
        );
        assert!(!flush.iter().any(|arg| arg == "--to"));
    });
}

#[test]
fn compact_envelope_rejects_malformed_metadata_and_nested_raw_content() {
    let _case = Case::new();
    Python::attach(|py| {
        let startup = module(py, "conductor.a2a_session_start");
        let error_class = startup.getattr("SessionStartError").unwrap();
        assert_eq!(
            text(
                &validate(py, &startup, &valid_envelope())
                    .unwrap()
                    .bind(py)
                    .get_item("agent")
                    .unwrap()
            ),
            "one"
        );
        let mut cases = vec![(json!([]), "JSON object")];
        for (field, changed, error) in [
            ("schema_version", json!(2), "schema_version"),
            ("schema_version", json!(1.0), "schema_version"),
            ("authority", json!("peer-claimed"), "authority"),
            ("agent", json!("other"), "does not match"),
            ("unread_only", json!(false), "unread_only"),
            ("total", json!(true), "total must be"),
            ("shown", json!("1"), "shown must be"),
            ("omitted", json!(-1), "omitted must be"),
            (
                "raw_bytes_not_injected",
                json!("12"),
                "raw_bytes_not_injected",
            ),
            ("messages", json!({}), "messages must be"),
        ] {
            let mut value = valid_envelope();
            value[field] = changed;
            cases.push((value, error));
        }
        for (field, changed, error) in [
            ("shown", json!(0), "does not match"),
            ("total", json!(2), "count mismatch"),
            ("raw_bytes_not_injected", json!(11), "smaller than shown"),
            ("raw_bytes_not_injected", json!(13), "raw byte mismatch"),
        ] {
            let mut value = valid_envelope();
            value[field] = changed;
            cases.push((value, error));
        }
        let mut too_many = valid_envelope();
        too_many["shown"] = json!(2);
        too_many["total"] = json!(2);
        too_many["raw_bytes_not_injected"] = json!(24);
        too_many["messages"]
            .as_array_mut()
            .unwrap()
            .push(valid_envelope()["messages"][0].clone());
        cases.push((too_many, "exceeds requested maximum"));
        let mut extension = valid_envelope();
        extension["extension"] = json!("schema bump required");
        cases.push((extension, "keys do not match"));
        for (field, changed, error) in [
            ("summary", json!(7), "summary must be a string"),
            (
                "requires_response",
                json!(1),
                "requires_response must be boolean",
            ),
            ("raw_bytes", json!(-1), "raw_bytes must be"),
            ("summary", json!("x".repeat(141)), "summary exceeds 140"),
        ] {
            let mut value = valid_envelope();
            value["messages"][0][field] = changed;
            cases.push((value, error));
        }
        for key in ["body", "data", "data_json"] {
            let mut value = valid_envelope();
            value["messages"][0]["nested"][key] = json!("must not cross boundary");
            cases.push((value, "forbidden raw-content key"));
        }
        for (value, error) in cases {
            assert_error(
                py,
                validate(py, &startup, &value).unwrap_err(),
                &error_class,
                error,
            );
        }
    });
}

#[test]
fn real_compact_preview_withholds_raw_body_and_flushes_only_sender() {
    let binary = std::env::var("FORGE_BIN").expect("prebuilt FORGE_BIN for Python boundary tests");
    let mut case = Case::new();
    case.set_env("FORGE_BIN", &binary);
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../src");
    let mut python_paths = vec![source];
    if let Some(existing) = std::env::var_os("PYTHONPATH") {
        python_paths.extend(std::env::split_paths(&existing));
    }
    let python_path = std::env::join_paths(python_paths).unwrap();
    case.set_env("PYTHONPATH", python_path.to_str().unwrap());
    let state = case.mkdir("a2a");
    let interpreter = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
        .join(".venv/bin/python");
    assert!(
        interpreter.is_file(),
        "Forge Python test environment is required"
    );
    let full_body = format!("{}private-tail-marker", "evidence ".repeat(10_000));
    Python::attach(|py| {
        let startup = module(py, "conductor.a2a_session_start");
        let agent = module(py, "conductor.agent_a2a");
        let store = agent
            .getattr("A2aStore")
            .unwrap()
            .call1((path(py, &state), "one"))
            .unwrap();
        store
            .call_method1(
                "record_inbound",
                (
                    "00000000-0000-4000-8000-000000000001",
                    "peer",
                    "one",
                    full_body.as_str(),
                    py.None(),
                ),
            )
            .unwrap();
        let args = kwargs(py, "one", &state);
        args.set_item("interpreter", interpreter.to_str().unwrap())
            .unwrap();
        let result = startup
            .getattr("request_compact_preview")
            .unwrap()
            .call((), Some(&args))
            .unwrap();
        let payload = result.get_item(0).unwrap();
        let rendered = text(&result.get_item(1).unwrap());
        assert_eq!(
            text(&payload.get_item("authority").unwrap()),
            "bounded-a2a-inbox"
        );
        assert_eq!(
            payload
                .get_item("total")
                .unwrap()
                .extract::<usize>()
                .unwrap(),
            1
        );
        assert!(rendered.len() <= 1200);
        assert!(!rendered.contains(&full_body));
        assert!(!rendered.contains("private-tail-marker"));
        assert!(!rendered.contains("data_json"));
        assert_eq!(
            payload
                .get_item("raw_bytes_not_injected")
                .unwrap()
                .extract::<usize>()
                .unwrap(),
            full_body.len()
        );
        assert_eq!(text(&payload.get_item("agent").unwrap()), "one");

        let registry = module(py, "conductor.a2a_registry");
        registry
            .getattr("init_registry")
            .unwrap()
            .call1((path(py, &state), "one", 7401))
            .unwrap();
        registry
            .getattr("init_registry")
            .unwrap()
            .call1((path(py, &state), "peer", 7402))
            .unwrap();
        let sender = agent
            .getattr("A2aStore")
            .unwrap()
            .call1((path(py, &state), "one"))
            .unwrap();
        sender
            .call_method1(
                "record_outbound",
                ("queued-one", "one", "peer", "body", py.None()),
            )
            .unwrap();
        let status = startup
            .getattr("flush_sender_queue")
            .unwrap()
            .call((), Some(&args))
            .unwrap()
            .extract::<i32>()
            .unwrap();
        assert_eq!(status, 3);
        assert_eq!(
            sender
                .call_method0("queued_outbound")
                .unwrap()
                .len()
                .unwrap(),
            1
        );
    });
}

#[test]
fn failed_compact_command_never_requests_full_inbox() {
    let mut case = Case::new();
    let log = case.root().join("calls.log");
    let stub = case.write(
        "failed-preview",
        &format!(
            "#!/bin/sh\nprintf 'called\\n' >> '{}'\nprintf '%s\\n' \"$@\" >> '{}'\nexit 2\n",
            log.display(),
            log.display()
        ),
    );
    fs::set_permissions(&stub, fs::Permissions::from_mode(0o700)).unwrap();
    case.set_env(
        "PYTHONPATH",
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../src")
            .to_str()
            .unwrap(),
    );
    Python::attach(|py| {
        let startup = module(py, "conductor.a2a_session_start");
        let args = kwargs(py, "one", case.root());
        args.set_item("interpreter", stub.to_str().unwrap())
            .unwrap();
        assert_error(
            py,
            startup
                .getattr("request_compact_preview")
                .unwrap()
                .call((), Some(&args))
                .unwrap_err(),
            &startup.getattr("SessionStartError").unwrap(),
            "bounded A2A preview exited 2",
        );
    });
    let calls = fs::read_to_string(log).unwrap();
    assert_eq!(calls.lines().filter(|line| *line == "called").count(), 1);
    assert!(calls.lines().any(|line| line == "--compact"));
    assert!(!calls.lines().any(|line| line == "--full"));
}

#[test]
fn preview_enforces_serialized_character_budget() {
    let case = Case::new();
    let script = case.write(
        "envelope-preview",
        &format!(
            "#!/bin/sh\ncat <<'ENVELOPE'\n{}\nENVELOPE\n",
            valid_envelope()
        ),
    );
    fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
    Python::attach(|py| {
        let startup = module(py, "conductor.a2a_session_start");
        let args = kwargs(py, "one", case.root());
        args.set_item("interpreter", script.to_str().unwrap())
            .unwrap();
        args.set_item("max_chars", 500).unwrap();
        let preview = startup
            .getattr("request_compact_preview")
            .unwrap()
            .call((), Some(&args))
            .unwrap();
        assert_eq!(
            text(&preview.get_item(0).unwrap().get_item("agent").unwrap()),
            "one"
        );
        assert!(text(&preview.get_item(1).unwrap()).len() <= 500);
        args.set_item("max_chars", 256).unwrap();
        assert_error(
            py,
            startup
                .getattr("request_compact_preview")
                .unwrap()
                .call((), Some(&args))
                .unwrap_err(),
            &startup.getattr("SessionStartError").unwrap(),
            "chars; limit is 256",
        );
    });
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

struct NativeServeGuard {
    pid_file: std::path::PathBuf,
    state_dir: std::path::PathBuf,
}

impl NativeServeGuard {
    fn stop(&self) -> bool {
        let Ok(pid) = fs::read_to_string(&self.pid_file) else {
            return false;
        };
        let pid = pid.trim();
        let Ok(pid_number) = pid.parse::<i32>() else {
            return false;
        };
        let process = format!("/proc/{pid}");
        let Ok(command) = fs::read(format!("{process}/cmdline")) else {
            return false;
        };
        if !String::from_utf8_lossy(&command).contains(self.state_dir.to_str().unwrap()) {
            return false;
        }
        if !Command::new("kill")
            .arg("-TERM")
            .arg(pid)
            .output()
            .is_ok_and(|result| result.status.success())
        {
            return false;
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            let exited = Python::attach(|py| {
                let os = PyModule::import(py, "os").unwrap();
                let no_hang = os.getattr("WNOHANG").unwrap();
                os.getattr("waitpid")
                    .unwrap()
                    .call1((pid_number, no_hang))
                    .ok()
                    .and_then(|result| result.extract::<(i32, i32)>().ok())
                    .is_some_and(|(reaped, _)| reaped == pid_number)
            });
            if exited || !Path::new(&process).exists() {
                return true;
            }
            thread::sleep(Duration::from_millis(20));
        }
        false
    }
}

impl Drop for NativeServeGuard {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

#[test]
fn ensure_serve_starts_native_endpoint_reuses_it_and_rejects_occupied_port() {
    let binary = std::env::var("FORGE_BIN").expect("prebuilt FORGE_BIN for Python boundary tests");
    let mut case = Case::new();
    let state = case.mkdir("a2a");
    let pid_file = case.root().join("serve.pid");
    let wrapper = case.write(
        "logged-forge",
        &format!(
            "#!/bin/sh\nif [ \"$4\" = 'serve' ]; then printf '%s' \"$$\" > '{}'; fi\nexec '{}' \"$@\"\n",
            pid_file.display(),
            binary
        ),
    );
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o700)).unwrap();
    case.set_env("FORGE_BIN", wrapper.to_str().unwrap());
    let serve = NativeServeGuard {
        pid_file,
        state_dir: state.clone(),
    };
    let port = free_port();
    Python::attach(|py| {
        let registry = module(py, "conductor.a2a_registry");
        registry
            .getattr("init_registry")
            .unwrap()
            .call1((path(py, &state), "one", port))
            .unwrap();
        let startup = module(py, "conductor.a2a_session_start");
        let args = kwargs(py, "one", &state);
        assert_eq!(
            text(
                &startup
                    .getattr("ensure_serve")
                    .unwrap()
                    .call((), Some(&args))
                    .unwrap()
            ),
            "started"
        );
        assert_eq!(
            text(
                &startup
                    .getattr("ensure_serve")
                    .unwrap()
                    .call((), Some(&args))
                    .unwrap()
            ),
            "already-running"
        );
        let command: Vec<String> = startup
            .getattr("serve_command")
            .unwrap()
            .call((), Some(&args))
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(&command[..2], [wrapper.to_str().unwrap(), "mailbox"]);
        assert_eq!(&command[command.len() - 3..], ["serve", "--name", "one"]);
    });

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let occupied = listener.local_addr().unwrap().port();
    Python::attach(|py| {
        let registry = module(py, "conductor.a2a_registry");
        registry
            .getattr("init_registry")
            .unwrap()
            .call1((path(py, &state), "occupied", occupied))
            .unwrap();
        let startup = module(py, "conductor.a2a_session_start");
        let args = kwargs(py, "occupied", &state);
        assert_error(
            py,
            startup
                .getattr("ensure_serve")
                .unwrap()
                .call((), Some(&args))
                .unwrap_err(),
            &startup.getattr("SessionStartError").unwrap(),
            "occupied by an invalid endpoint",
        );
    });
    assert!(
        serve.stop(),
        "native endpoint child did not exit after SIGTERM"
    );
}

#[test]
fn grok_hook_json_uses_user_prompt_event_without_raw_preview() {
    let _case = Case::new();
    Python::attach(|py| {
        let startup = module(py, "conductor.a2a_session_start");
        let payload = python_json(py, &json!({"total": 1}));
        let result = startup
            .getattr("StartupResult")
            .unwrap()
            .call1(("grok", "started", 0, payload, "{\"total\":1}"))
            .unwrap();
        let output = PyModule::import(py, "io")
            .unwrap()
            .getattr("StringIO")
            .unwrap()
            .call0()
            .unwrap();
        let redirect = PyModule::import(py, "contextlib")
            .unwrap()
            .getattr("redirect_stdout")
            .unwrap()
            .call1((&output,))
            .unwrap();
        redirect.call_method0("__enter__").unwrap();
        let args = PyDict::new(py);
        args.set_item("output", "hook-json").unwrap();
        args.set_item("event_name", "UserPromptSubmit").unwrap();
        startup
            .getattr("_emit")
            .unwrap()
            .call((&result,), Some(&args))
            .unwrap();
        redirect
            .call_method1("__exit__", (py.None(), py.None(), py.None()))
            .unwrap();
        let rendered = text(&output.call_method0("getvalue").unwrap());
        let emitted: Value = serde_json::from_str(&rendered).unwrap();
        assert_eq!(
            emitted,
            json!({"hookSpecificOutput": {
                "hookEventName": "UserPromptSubmit", "additionalContext": "{\"total\":1}"
            }})
        );
    });
}

fn first_invocation<'py>(
    py: Python<'py>,
    startup: &Bound<'py, PyModule>,
    state: &Path,
    session_id: &str,
) -> Bound<'py, PyAny> {
    let args = kwargs(py, "one", state);
    args.set_item("session_id", session_id).unwrap();
    startup
        .getattr("first_invocation")
        .unwrap()
        .call((), Some(&args))
        .unwrap()
}

#[test]
fn once_per_session_marks_success_and_retries_failed_entry() {
    let case = Case::new();
    Python::attach(|py| {
        let startup = module(py, "conductor.a2a_session_start");
        let enter = |session_id: &str| {
            let context = first_invocation(py, &startup, case.root(), session_id);
            let allowed = context
                .call_method0("__enter__")
                .unwrap()
                .extract::<bool>()
                .unwrap();
            (context, allowed)
        };
        let (first, allowed) = enter("session-a");
        assert!(allowed);
        first
            .call_method1("__exit__", (py.None(), py.None(), py.None()))
            .unwrap();
        let (again, allowed) = enter("session-a");
        assert!(!allowed);
        again
            .call_method1("__exit__", (py.None(), py.None(), py.None()))
            .unwrap();
        let (failed, allowed) = enter("session-b");
        assert!(allowed);
        let exception = PyModule::import(py, "builtins")
            .unwrap()
            .getattr("RuntimeError")
            .unwrap()
            .call1(("boom",))
            .unwrap();
        let suppressed = failed
            .call_method1(
                "__exit__",
                (
                    PyModule::import(py, "builtins")
                        .unwrap()
                        .getattr("RuntimeError")
                        .unwrap(),
                    &exception,
                    py.None(),
                ),
            )
            .unwrap()
            .extract::<bool>()
            .unwrap();
        assert!(!suppressed);
        let (retry, allowed) = enter("session-b");
        assert!(allowed);
        retry
            .call_method1("__exit__", (py.None(), py.None(), py.None()))
            .unwrap();
        let marker_dir = case.root().join("one/.session-start");
        assert_eq!(
            fs::read_dir(marker_dir)
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| entry.file_name().to_string_lossy().ends_with(".done"))
                .count(),
            2
        );
    });
}
