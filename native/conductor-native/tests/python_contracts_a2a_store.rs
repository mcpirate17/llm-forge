#![cfg(feature = "python-compat-tests")]
//! Shared-library identity and SQLite visibility across the public store facade.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::prelude::*;
use support::{module, path, Case};

#[test]
fn native_hold_updates_are_visible_to_fresh_python_connections() {
    let case = Case::new();
    Python::attach(|py| {
        let store = module(py, "conductor.agent_a2a")
            .getattr("A2aStore")
            .unwrap()
            .call1((path(py, case.root()), "reader"))
            .unwrap();
        store
            .call_method1(
                "record_inbound",
                ("held-message", "writer", "reader", "retain this", py.None()),
            )
            .unwrap();
        for reason in [Some("independent review"), None, Some("second hold"), None] {
            store
                .call_method1("set_hold", ("held-message", reason))
                .unwrap();
            let context = store.call_method0("connect").unwrap();
            let connection = context.call_method0("__enter__").unwrap();
            let row = connection
                .call_method1(
                    "execute",
                    (
                        "SELECT hold_reason FROM message_state WHERE message_id=?",
                        ("held-message",),
                    ),
                )
                .unwrap()
                .call_method0("fetchone")
                .unwrap();
            let observed: Option<String> = row.get_item(0).unwrap().extract().unwrap();
            context
                .call_method1("__exit__", (py.None(), py.None(), py.None()))
                .unwrap();
            assert_eq!(observed.as_deref(), reason);
        }
    });
}

#[test]
fn unverifiable_sqlite_library_is_refused_before_store_creation() {
    let case = Case::new();
    let invalid_library = case.write("invalid-library.so", "not a shared library");
    let state = case.root().join("uncreated");
    Python::attach(|py| {
        let sqlite = module(py, "_sqlite3");
        let original = sqlite.getattr("__file__").unwrap();
        sqlite
            .setattr("__file__", invalid_library.to_str().unwrap())
            .unwrap();
        let result = module(py, "conductor_native")
            .getattr("A2aSqliteStore")
            .unwrap()
            .call1((state.to_str().unwrap(), "reader"));
        sqlite.setattr("__file__", original).unwrap();
        let error = result.unwrap_err();
        assert!(error.is_instance_of::<pyo3::exceptions::PyRuntimeError>(py));
        assert!(error
            .to_string()
            .contains("cannot verify Python SQLite library identity"));
        assert!(!state.exists());
    });
}
