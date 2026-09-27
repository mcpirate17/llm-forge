#![cfg(feature = "python-compat-tests")]
//! Python public facade assertions for Forge's native A2A registry.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyModule};
use std::fs;
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use support::{assert_error, module, path, text, Case};

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn init<'py>(
    py: Python<'py>,
    registry: &Bound<'py, PyModule>,
    state: &Path,
    name: &str,
    port: u16,
    renew: bool,
) -> PyResult<Bound<'py, PyAny>> {
    let kwargs = PyDict::new(py);
    kwargs.set_item("renew_generation", renew)?;
    registry
        .getattr("init_registry")?
        .call((path(py, state), name, port), Some(&kwargs))
}

#[test]
fn native_init_preserves_public_records_permissions_ports_and_rotation() {
    let binary = std::env::var("FORGE_BIN").expect("prebuilt Forge binary required");
    let mut case = Case::new();
    case.set_env("FORGE_BIN", &binary);
    let state = case.mkdir("registry");
    let port = free_port();
    Python::attach(|py| {
        let registry = module(py, "conductor.a2a_registry");
        let first = init(py, &registry, &state, "one", port, false).unwrap();
        let record = first.get_item("one").unwrap();
        assert_eq!(
            record.getattr("port").unwrap().extract::<u16>().unwrap(),
            port
        );
        let token = text(&record.getattr("token").unwrap());
        let generation = text(&record.getattr("generation").unwrap());
        assert!(token.len() >= 16);
        let second = init(py, &registry, &state, "one", port, false).unwrap();
        let same = second.get_item("one").unwrap();
        assert_eq!(text(&same.getattr("token").unwrap()), token);
        assert_eq!(text(&same.getattr("generation").unwrap()), generation);
        let rotated = init(py, &registry, &state, "one", port, true).unwrap();
        let changed = rotated.get_item("one").unwrap();
        assert_eq!(text(&changed.getattr("token").unwrap()), token);
        assert_ne!(text(&changed.getattr("generation").unwrap()), generation);
        let conflict = init(py, &registry, &state, "two", port, false).unwrap_err();
        assert_error(
            py,
            conflict,
            &registry.getattr("A2aError").unwrap(),
            "already assigned",
        );
    });
    assert_eq!(
        fs::metadata(state.join("agents.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o077,
        0
    );
    assert_eq!(
        fs::metadata(state.join(".registry.lock"))
            .unwrap()
            .permissions()
            .mode()
            & 0o077,
        0
    );
}

#[test]
fn native_peers_and_reap_keep_python_result_schema_and_streak() {
    let binary = std::env::var("FORGE_BIN").expect("prebuilt Forge binary required");
    let mut case = Case::new();
    case.set_env("FORGE_BIN", &binary);
    let state = case.mkdir("liveness");
    let port = free_port();
    Python::attach(|py| {
        let registry = module(py, "conductor.a2a_registry");
        let agent = module(py, "conductor.agent_a2a");
        init(py, &registry, &state, "down", port, false).unwrap();
        let peers = registry
            .getattr("list_peers")
            .unwrap()
            .call1((path(py, &state),))
            .unwrap();
        assert_eq!(peers.len().unwrap(), 1);
        let peer = peers.get_item(0).unwrap();
        assert_eq!(text(&peer.get_item("status").unwrap()), "down");
        assert_eq!(
            peer.get_item("consecutive_failures")
                .unwrap()
                .extract::<u64>()
                .unwrap(),
            1
        );
        assert_eq!(text(&peer.get_item("name").unwrap()), "down");
        let result = agent
            .getattr("reap_registry")
            .unwrap()
            .call1((path(py, &state), 3))
            .unwrap();
        assert_eq!(result.get_item("reaped").unwrap().len().unwrap(), 0);
        let result = agent
            .getattr("reap_registry")
            .unwrap()
            .call1((path(py, &state), 3))
            .unwrap();
        assert_eq!(
            result
                .get_item("reaped")
                .unwrap()
                .get_item(0)
                .unwrap()
                .extract::<String>()
                .unwrap(),
            "down"
        );
        let liveness: serde_json::Value =
            serde_json::from_slice(&fs::read(state.join("liveness.json")).unwrap()).unwrap();
        assert!(liveness["agents"].as_object().unwrap().is_empty());
    });
}

#[test]
fn native_registry_launch_failures_translate_to_a2a_error() {
    let mut case = Case::new();
    case.set_env("FORGE_BIN", "/bin/false");
    Python::attach(|py| {
        let registry = module(py, "conductor.a2a_registry");
        let error = registry.getattr("A2aError").unwrap();
        let failed = registry
            .getattr("init_registry")
            .unwrap()
            .call1((path(py, case.root()), "one", free_port()))
            .unwrap_err();
        assert_error(py, failed, &error, "native A2A init failed");
    });
    case.set_env("FORGE_BIN", "/bin/true");
    Python::attach(|py| {
        let registry = module(py, "conductor.a2a_registry");
        let failed = registry
            .getattr("list_peers")
            .unwrap()
            .call1((path(py, case.root()),))
            .unwrap_err();
        assert_error(
            py,
            failed,
            &registry.getattr("A2aError").unwrap(),
            "invalid native A2A peers response",
        );
    });
}

#[test]
fn detached_serve_command_uses_forge_binary_directly() {
    let binary = std::env::var("FORGE_BIN").expect("prebuilt Forge binary required");
    let mut case = Case::new();
    case.set_env("FORGE_BIN", &binary);
    Python::attach(|py| {
        let startup = module(py, "conductor.a2a_session_start");
        let kwargs = PyDict::new(py);
        kwargs.set_item("identity", "one").unwrap();
        kwargs.set_item("state_dir", path(py, case.root())).unwrap();
        let command = startup
            .getattr("serve_command")
            .unwrap()
            .call((), Some(&kwargs))
            .unwrap();
        let items: Vec<String> = command.extract().unwrap();
        assert_eq!(items[0], binary);
        assert_eq!(items[1], "mailbox");
        assert!(items
            .windows(2)
            .any(|pair| pair == ["--state-dir", case.root().to_str().unwrap()]));
        assert_eq!(&items[items.len() - 3..], ["serve", "--name", "one"]);
    });
}
