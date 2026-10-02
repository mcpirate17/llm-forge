#![cfg(feature = "python-compat-tests")]
//! Real Linux notification and explicit polling fallback contracts.
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;
use pyo3::prelude::*;
use std::fs;
use support::{module, path, AttrPatch, Case};

#[test]
fn mailbox_write_wakes_and_non_mailbox_writes_do_not() {
    let case = Case::new();
    fs::write(case.root().join("store.sqlite"), b"fixture").unwrap();
    Python::attach(|py| {
        let wait = module(py, "conductor.a2a_wait");
        let wakeup = wait
            .getattr("MailboxWakeup")
            .unwrap()
            .call1((path(py, case.root()),))
            .unwrap();
        assert!(wakeup.getattr("fd").unwrap().extract::<i32>().unwrap() >= 0);
        assert!(!wakeup
            .call_method1("wait", (0.0,))
            .unwrap()
            .extract::<bool>()
            .unwrap());
        fs::write(case.root().join("supervisor.json"), b"irrelevant").unwrap();
        assert!(!wakeup
            .call_method1("wait", (0.0,))
            .unwrap()
            .extract::<bool>()
            .unwrap());
        fs::write(case.root().join("store.sqlite-wal"), b"new messages").unwrap();
        assert!(wakeup
            .call_method1("wait", (0.1,))
            .unwrap()
            .extract::<bool>()
            .unwrap());
        wakeup.call_method0("close").unwrap();
        assert_eq!(wakeup.getattr("fd").unwrap().extract::<i32>().unwrap(), -1);
    });
}

#[test]
fn missing_mailbox_uses_bounded_polling_without_creating_a_database() {
    let case = Case::new();
    Python::attach(|py| {
        let wait = module(py, "conductor.a2a_wait");
        let time = wait.getattr("time").unwrap();
        let sleep = py
            .import("unittest.mock")
            .unwrap()
            .getattr("MagicMock")
            .unwrap()
            .call0()
            .unwrap();
        let _patch = AttrPatch::replace(&time, "sleep", &sleep);
        let wakeup = wait
            .getattr("MailboxWakeup")
            .unwrap()
            .call1((path(py, case.root()),))
            .unwrap();
        assert_eq!(wakeup.getattr("fd").unwrap().extract::<i32>().unwrap(), -1);
        assert!(!wakeup
            .call_method1("wait", (0.25,))
            .unwrap()
            .extract::<bool>()
            .unwrap());
        sleep
            .call_method1("assert_called_once_with", (0.25,))
            .unwrap();
        wakeup.call_method0("close").unwrap();
        assert!(!case.root().join("store.sqlite").exists());
        // Public CLI parsing exposes logical keys and explicit unread recovery.
        let cli = module(py, "conductor.a2a_cli");
        let args = cli
            .getattr("build_parser")
            .unwrap()
            .call0()
            .unwrap()
            .call_method1(
                "parse_args",
                (vec![
                    "watch",
                    "--as-name",
                    "tester",
                    "--once",
                    "--replay-unread",
                ],),
            )
            .unwrap();
        assert!(args
            .getattr("replay_unread")
            .unwrap()
            .extract::<bool>()
            .unwrap());
    });
}
