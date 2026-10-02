#![cfg(feature = "python-compat-tests")]
//! Rust assertions at the Python-to-native A2A delivery boundary.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)] // Shared fixture has helpers for the other contract suites.
mod support;

use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyModule};
use std::fs;
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};
use support::{assert_error, module, path, text, Case};

fn history<'py>(
    py: Python<'py>,
    delivery: &Bound<'py, PyModule>,
    state: &Path,
) -> PyResult<Bound<'py, PyAny>> {
    delivery
        .getattr("history")?
        .call1((path(py, state), "sender"))
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("bind fixture port")
        .local_addr()
        .expect("fixture address")
        .port()
}

#[test]
fn launch_errors_and_invalid_native_output_raise_a2a_error() {
    let mut case = Case::new();
    let broken = case.write("broken-forge", "not an executable image\n");
    fs::set_permissions(&broken, fs::Permissions::from_mode(0o700)).unwrap();
    case.set_env("FORGE_BIN", broken.to_str().unwrap());
    Python::attach(|py| {
        let delivery = module(py, "conductor.a2a_delivery");
        let error_class = delivery.getattr("A2aError").unwrap();
        assert_error(
            py,
            history(py, &delivery, case.root()).unwrap_err(),
            &error_class,
            "cannot run native A2A history",
        );
    });

    case.set_env("FORGE_BIN", "/bin/true");
    Python::attach(|py| {
        let delivery = module(py, "conductor.a2a_delivery");
        let error_class = delivery.getattr("A2aError").unwrap();
        assert_error(
            py,
            history(py, &delivery, case.root()).unwrap_err(),
            &error_class,
            "invalid native A2A history response",
        );
    });

    case.set_env("FORGE_BIN", "/bin/false");
    Python::attach(|py| {
        let delivery = module(py, "conductor.a2a_delivery");
        let error_class = delivery.getattr("A2aError").unwrap();
        assert_error(
            py,
            history(py, &delivery, case.root()).unwrap_err(),
            &error_class,
            "native A2A history failed",
        );
    });
}

fn registered_store<'py>(
    py: Python<'py>,
    state: &Path,
) -> (Bound<'py, PyModule>, Bound<'py, PyAny>) {
    let registry = module(py, "conductor.a2a_registry");
    let delivery = module(py, "conductor.a2a_delivery");
    let agent = module(py, "conductor.agent_a2a");
    let init = registry.getattr("init_registry").unwrap();
    let port = free_port();
    let mut peer_port = free_port();
    while peer_port == port {
        peer_port = free_port();
    }
    init.call1((path(py, state), "sender", port)).unwrap();
    init.call1((path(py, state), "peer", peer_port)).unwrap();
    let absent = history(py, &delivery, state).unwrap();
    assert!(!absent
        .get_item("available")
        .unwrap()
        .extract::<bool>()
        .unwrap());
    assert!(!state.join("sender/store.sqlite").exists());
    let store = agent
        .getattr("A2aStore")
        .unwrap()
        .call1((path(py, state), "sender"))
        .unwrap();
    (delivery, store)
}

fn send_structured_and_assert<'py>(
    py: Python<'py>,
    delivery: &Bound<'py, PyModule>,
    store: &Bound<'py, PyAny>,
    state: &Path,
) -> String {
    let started_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    let data = PyDict::new(py);
    data.set_item("kind", "coordination-v2").unwrap();
    data.set_item("thread_id", "python-boundary").unwrap();
    data.set_item("summary", "retry status").unwrap();
    data.set_item("status", "open").unwrap();
    data.set_item("requires_response", true).unwrap();
    data.set_item("supersedes", Vec::<String>::new()).unwrap();
    let receipt = delivery
        .getattr("send_message")
        .unwrap()
        .call1(("sender", "peer", "private 🦀 body", &data, path(py, state)))
        .unwrap();
    assert_eq!(
        text(&receipt.get_item("authority").unwrap()),
        "a2a-delivery-receipt"
    );
    assert_eq!(
        text(&receipt.get_item("delivery_status").unwrap()),
        "queued"
    );
    assert_eq!(
        receipt
            .get_item("body_bytes")
            .unwrap()
            .extract::<usize>()
            .unwrap(),
        "private 🦀 body".len()
    );
    assert!(!receipt.contains("body").unwrap());
    assert!(!receipt.contains("data_json").unwrap());
    let id = text(&receipt.get_item("message_id").unwrap());
    let connection = rusqlite::Connection::open(state.join("sender/store.sqlite")).unwrap();
    let (attempts, due): (u32, i64) = connection
        .query_row(
            "SELECT attempts,next_attempt_ms FROM outbound_retries WHERE message_id=?1",
            [&id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(attempts, 1);
    assert!(
        due >= started_ms + 250,
        "retry must retain its durable delay"
    );
    let outbound = store
        .call_method1("message", (id.as_str(), "outbound"))
        .unwrap();
    let stored_data = outbound.get_item("data_json").unwrap();
    let decoded = PyModule::import(py, "json")
        .unwrap()
        .getattr("loads")
        .unwrap()
        .call1((stored_data,))
        .unwrap();
    assert_eq!(
        text(&decoded.get_item("thread_id").unwrap()),
        "python-boundary"
    );
    id
}

fn retry_and_refuse<'py>(
    py: Python<'py>,
    delivery: &Bound<'py, PyModule>,
    store: &Bound<'py, PyAny>,
    state: &Path,
    id: &str,
) {
    let connection = rusqlite::Connection::open(state.join("sender/store.sqlite")).unwrap();
    // Hold the fixture clock deterministically so a slow test cannot cross the due time.
    connection
        .execute(
            "UPDATE outbound_retries SET next_attempt_ms=?1 WHERE message_id=?2",
            rusqlite::params![i64::MAX, id],
        )
        .unwrap();
    let early = delivery
        .getattr("flush_queued")
        .unwrap()
        .call1((path(py, state), "sender"))
        .unwrap();
    assert_eq!(early.len().unwrap(), 0, "deferred retry must remain queued");
    connection
        .execute(
            "UPDATE outbound_retries SET next_attempt_ms=0 WHERE message_id=?1",
            [id],
        )
        .unwrap();
    let rows = delivery
        .getattr("flush_queued")
        .unwrap()
        .call1((path(py, state), "sender"))
        .unwrap();
    assert_eq!(rows.len().unwrap(), 1);
    assert_eq!(
        text(&rows.get_item(0).unwrap().get_item("message_id").unwrap()),
        id
    );
    assert_eq!(
        text(&rows.get_item(0).unwrap().get_item("status").unwrap()),
        "queued"
    );
    let events = history(py, delivery, state).unwrap();
    assert!(events
        .get_item("available")
        .unwrap()
        .extract::<bool>()
        .unwrap());
    assert!(events.get_item("events").unwrap().len().unwrap() >= 2);
    assert!(!text(&events).contains("private 🦀 body"));
    let refused = delivery
        .getattr("send_message")
        .unwrap()
        .call1((
            "sender",
            "peer",
            "no queue",
            py.None(),
            path(py, state),
            false,
        ))
        .unwrap_err();
    assert_error(
        py,
        refused,
        &delivery.getattr("A2aError").unwrap(),
        "queue disabled",
    );
    assert_eq!(
        store
            .call_method0("queued_outbound")
            .unwrap()
            .len()
            .unwrap(),
        1
    );
}

#[test]
fn native_send_flush_and_history_preserve_python_receipt_contract() {
    let binary = std::env::var("FORGE_BIN")
        .expect("FORGE_BIN must name the prebuilt Forge executable for this contract suite");
    let mut case = Case::new();
    case.set_env("FORGE_BIN", &binary);
    let state = case.mkdir("a2a");
    Python::attach(|py| {
        let (delivery, store) = registered_store(py, &state);
        let id = send_structured_and_assert(py, &delivery, &store, &state);
        retry_and_refuse(py, &delivery, &store, &state, &id);
    });
}

#[test]
fn wrapper_rejects_non_string_body_and_unserializable_data() {
    let mut case = Case::new();
    case.set_env("FORGE_BIN", "/bin/true");
    Python::attach(|py| {
        let delivery = module(py, "conductor.a2a_delivery");
        let error_class = delivery.getattr("A2aError").unwrap();
        let send = delivery.getattr("send_message").unwrap();
        let bad_body = send
            .call1(("sender", "peer", 7, py.None(), path(py, case.root())))
            .unwrap_err();
        assert_error(py, bad_body, &error_class, "body must be a string");
        let data = PyDict::new(py);
        data.set_item(
            "unserializable",
            PyModule::import(py, "builtins")
                .unwrap()
                .getattr("object")
                .unwrap()
                .call0()
                .unwrap(),
        )
        .unwrap();
        let bad_data = send
            .call1(("sender", "peer", "body", &data, path(py, case.root())))
            .unwrap_err();
        assert_error(py, bad_data, &error_class, "invalid A2A data payload");
    });
}
