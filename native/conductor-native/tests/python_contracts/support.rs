//! Shared fixtures for interpreter-dependent Python API contract tests.

use pyo3::prelude::*;
use pyo3::types::{PyAny, PyBytes, PyModule};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};

static NEXT_TREE: AtomicU64 = AtomicU64::new(0);
static PROCESS_STATE: Mutex<()> = Mutex::new(());

const PROJECT_ENV: &[&str] = &[
    "CONDUCTOR_CANDIDATE_POLICY",
    "CONDUCTOR_MUTATION_REGISTRY",
    "CONDUCTOR_PACKAGE_ROOT",
    "CONDUCTOR_MUTATION_RECEIPT_ROOT",
    "CONDUCTOR_NOTES_ROOT",
    "CONDUCTOR_NOTES_DB",
    "CONDUCTOR_GUARDRAIL_ALLOWLIST",
    "CONDUCTOR_MEMORY_SOURCES",
    "CONDUCTOR_INTEGRATION_BRANCH",
    "CONDUCTOR_CRATE_ROSTER",
    "CONDUCTOR_NATIVE_ROOT",
    "CONDUCTOR_RADON_BASELINE",
    "CONDUCTOR_HOST_ROOT",
    "CONDUCTOR_WORKTREE_PATTERNS",
];

pub struct Case {
    env: EnvRestore,
    root: PathBuf,
    sys_path_snapshot: Py<PyAny>,
    // Rust drops fields in declaration order, so release this after env restoration.
    _process_state: MutexGuard<'static, ()>,
}

impl Case {
    pub fn new() -> Self {
        Python::initialize();
        let process_state = PROCESS_STATE.lock().expect("test process-state lock");
        let mut env = EnvRestore::default();
        for name in PROJECT_ENV {
            env.remove(name);
        }
        let root = std::env::temp_dir().join(format!(
            "conductor-python-contracts-{}-{}",
            std::process::id(),
            NEXT_TREE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).expect("create isolated test directory");
        let sys_path_snapshot = Python::attach(|py| {
            PyModule::import(py, "sys")
                .expect("import sys")
                .getattr("path")
                .expect("sys.path")
                .call_method0("copy")
                .expect("copy sys.path")
                .unbind()
        });
        Self {
            env,
            root,
            sys_path_snapshot,
            _process_state: process_state,
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn set_env(&mut self, name: &'static str, value: &str) {
        self.env.set(name, value);
    }

    pub fn remove_env(&mut self, name: &'static str) {
        self.env.remove(name);
    }

    pub fn write(&self, relative: &str, contents: &str) -> PathBuf {
        let path = self.root.join(relative);
        fs::create_dir_all(path.parent().expect("file parent")).expect("create file parent");
        fs::write(&path, contents).expect("write test fixture");
        path
    }

    pub fn mkdir(&self, relative: &str) -> PathBuf {
        let path = self.root.join(relative);
        fs::create_dir_all(&path).expect("create test directory");
        path
    }

    pub fn chdir(&self, relative: &str) -> CwdRestore {
        let prior = std::env::current_dir().expect("current test directory");
        let target = self.mkdir(relative);
        std::env::set_current_dir(target).expect("change to isolated test directory");
        CwdRestore(prior)
    }
}

pub struct CwdRestore(PathBuf);

impl Drop for CwdRestore {
    fn drop(&mut self) {
        std::env::set_current_dir(&self.0).expect("restore test working directory");
    }
}

impl Drop for Case {
    fn drop(&mut self) {
        Python::attach(|py| {
            let sys_path = PyModule::import(py, "sys")
                .expect("import sys")
                .getattr("path")
                .expect("sys.path");
            sys_path
                .call_method0("clear")
                .expect("clear test sys.path entries");
            sys_path
                .call_method1("extend", (self.sys_path_snapshot.bind(py),))
                .expect("restore sys.path");
        });
        fs::remove_dir_all(&self.root).expect("remove isolated test directory");
    }
}

#[derive(Default)]
struct EnvRestore(Vec<(&'static str, Option<OsString>)>);

impl EnvRestore {
    fn remember(&mut self, name: &'static str) {
        if !self.0.iter().any(|(saved, _)| *saved == name) {
            self.0.push((name, std::env::var_os(name)));
        }
    }

    fn set(&mut self, name: &'static str, value: &str) {
        self.remember(name);
        std::env::set_var(name, value);
        python_env(name, Some(OsStr::new(value)));
    }

    fn remove(&mut self, name: &'static str) {
        self.remember(name);
        std::env::remove_var(name);
        python_env(name, None);
    }
}

impl Drop for EnvRestore {
    fn drop(&mut self) {
        for (name, value) in self.0.drain(..).rev() {
            match value {
                Some(saved) => {
                    std::env::set_var(name, &saved);
                    python_env(name, Some(&saved));
                }
                None => {
                    std::env::remove_var(name);
                    python_env(name, None);
                }
            }
        }
    }
}

fn python_env(name: &str, value: Option<&OsStr>) {
    Python::attach(|py| {
        let environ = PyModule::import(py, "os")
            .expect("import os")
            .getattr("environb")
            .expect("os.environb");
        let key = PyBytes::new(py, name.as_bytes());
        match value {
            Some(value) => environ
                .set_item(&key, PyBytes::new(py, value.as_bytes()))
                .expect("set Python environment"),
            None => {
                environ
                    .call_method1("pop", (&key, py.None()))
                    .expect("remove Python environment");
            }
        }
    });
}

pub fn module<'py>(py: Python<'py>, name: &str) -> Bound<'py, PyModule> {
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("src")
        .canonicalize()
        .expect("resolve Forge Python source tree");
    let sys = PyModule::import(py, "sys").expect("import sys");
    sys.getattr("path")
        .expect("sys.path")
        .call_method1("insert", (0, source.to_str().expect("UTF-8 source path")))
        .expect("prepend Forge Python source path");
    PyModule::import(py, name).unwrap_or_else(|error| panic!("import {name}: {error}"))
}

pub fn path<'py>(py: Python<'py>, value: &Path) -> Bound<'py, PyAny> {
    PyModule::import(py, "pathlib")
        .expect("import pathlib")
        .getattr("Path")
        .expect("pathlib.Path")
        .call1((value.to_str().expect("UTF-8 test path"),))
        .expect("create Python Path")
}

pub fn text(value: &Bound<'_, PyAny>) -> String {
    value
        .str()
        .expect("stringify Python object")
        .to_str()
        .expect("UTF-8 Python string")
        .to_owned()
}

pub fn attr_text(value: &Bound<'_, PyAny>, field: &str) -> String {
    text(&value.getattr(field).expect("Python attribute"))
}

pub fn attr_bool(value: &Bound<'_, PyAny>, field: &str) -> bool {
    value
        .getattr(field)
        .expect("Python boolean attribute")
        .extract()
        .expect("extract Python boolean")
}

pub fn assert_error(py: Python<'_>, error: PyErr, class: &Bound<'_, PyAny>, message: &str) {
    assert!(
        error.matches(py, class).unwrap(),
        "expected {}, got {error}",
        text(class)
    );
    assert!(
        error.to_string().contains(message),
        "expected {message:?} in {error}"
    );
}

/// Restore a patched Python attribute even if a contract assertion panics.
pub struct AttrPatch {
    target: Py<PyAny>,
    name: String,
    original: Py<PyAny>,
}

impl AttrPatch {
    pub fn replace(target: &Bound<'_, PyAny>, name: &str, value: &Bound<'_, PyAny>) -> Self {
        let original = target
            .getattr(name)
            .expect("original Python attribute")
            .unbind();
        target.setattr(name, value).expect("patch Python attribute");
        Self {
            target: target.clone().unbind(),
            name: name.to_owned(),
            original,
        }
    }
}

impl Drop for AttrPatch {
    fn drop(&mut self) {
        Python::attach(|py| {
            self.target
                .bind(py)
                .setattr(self.name.as_str(), self.original.bind(py))
                .expect("restore Python attribute");
        });
    }
}
