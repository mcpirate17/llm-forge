#![cfg(feature = "python-compat-tests")]
//! Rust-owned lease-record and release contracts for the Python governance lock.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::prelude::*;
use pyo3::types::PyDict;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use support::{module, path, text, Case};

fn init_repo(case: &Case) -> PathBuf {
    let repo = case.mkdir("repo");
    let output = Command::new("git")
        .args(["init", "-q"])
        .current_dir(&repo)
        .output()
        .expect("initialize fixture Git repository");
    assert!(
        output.status.success(),
        "git init: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    repo
}

fn lock_path(py: Python<'_>, repo: &Path) -> PathBuf {
    let engine = module(py, "conductor.candidate_review.engine");
    let value = engine
        .getattr("_governance_lock_path")
        .unwrap()
        .call1((path(py, repo),))
        .unwrap();
    PathBuf::from(text(&value))
}

struct HeldLock(Py<PyAny>);

impl HeldLock {
    fn acquire(py: Python<'_>, repo: &Path, token: &str) -> Self {
        let kwargs = PyDict::new(py);
        kwargs.set_item("exclusive", true).unwrap();
        kwargs.set_item("timeout_seconds", 1.0).unwrap();
        kwargs.set_item("lease_token", token).unwrap();
        let context = module(py, "conductor.candidate_review.engine")
            .getattr("_held_governance_lock")
            .unwrap()
            .call((path(py, repo),), Some(&kwargs))
            .unwrap();
        context.call_method0("__enter__").unwrap();
        Self(context.unbind())
    }
}

impl Drop for HeldLock {
    fn drop(&mut self) {
        Python::attach(|py| {
            self.0
                .bind(py)
                .call_method1("__exit__", (py.None(), py.None(), py.None()))
                .expect("release fixture governance lock");
        });
    }
}

#[test]
fn released_governance_lock_leaves_no_lease_claim() {
    let case = Case::new();
    let repo = init_repo(&case);
    Python::attach(|py| {
        let lock = lock_path(py, &repo);
        let token = "c".repeat(64);
        {
            let _held = HeldLock::acquire(py, &repo, &token);
            let held: Value = serde_json::from_str(&fs::read_to_string(&lock).unwrap()).unwrap();
            assert_eq!(held["pid"].as_u64(), Some(u64::from(std::process::id())));
            assert_eq!(held["token"].as_str(), Some(token.as_str()));
            assert!(held["acquired"].as_str().is_some());
        }
        assert_eq!(fs::read_to_string(lock).unwrap(), "");
    });
}

#[test]
fn release_does_not_erase_another_holders_record() {
    let case = Case::new();
    let repo = init_repo(&case);
    Python::attach(|py| {
        let lock = lock_path(py, &repo);
        fs::create_dir_all(lock.parent().unwrap()).unwrap();
        let peer = format!(
            "{{\"pid\": {}, \"token\": \"{}\"}}",
            std::process::id() + 1,
            "d".repeat(64)
        );
        {
            let _held = HeldLock::acquire(py, &repo, &"e".repeat(64));
            fs::write(&lock, &peer).unwrap();
        }
        assert_eq!(fs::read_to_string(lock).unwrap(), peer);
    });
}

#[test]
fn an_unreadable_record_does_not_break_release() {
    let case = Case::new();
    let repo = init_repo(&case);
    Python::attach(|py| {
        let lock = lock_path(py, &repo);
        fs::create_dir_all(lock.parent().unwrap()).unwrap();
        let token = "f".repeat(64);
        {
            let _held = HeldLock::acquire(py, &repo, &token);
            fs::write(&lock, "not json at all\n").unwrap();
        }
        assert_eq!(fs::read_to_string(&lock).unwrap(), "not json at all\n");
        let _held_again = HeldLock::acquire(py, &repo, &token);
    });
}
