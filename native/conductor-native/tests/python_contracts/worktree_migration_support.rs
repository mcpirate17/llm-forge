//! Rust-owned temporary Git and environment fixtures for the four worktree suites.
//! Install at tests/python_contracts/worktree_migration_support.rs.

use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyModule};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::support::Case;

pub struct WorkspaceCase {
    // Drop the selector guard while Case still owns its process-state mutex.
    _git_env: GitEnvGuard,
    pub case: Case,
}

impl WorkspaceCase {
    pub fn new() -> Self {
        let case = Case::new();
        let git_env = GitEnvGuard::new();
        Self {
            _git_env: git_env,
            case,
        }
    }

    pub fn root(&self) -> &Path {
        self.case.root()
    }
    pub fn mkdir(&self, name: &str) -> PathBuf {
        self.case.mkdir(name)
    }
    pub fn write(&self, name: &str, body: &str) -> PathBuf {
        self.case.write(name, body)
    }
}

struct GitEnvGuard(Vec<(OsString, Option<OsString>)>);

impl GitEnvGuard {
    fn new() -> Self {
        let mut saved = Vec::new();
        for (key, value) in std::env::vars_os() {
            if key.as_os_str().as_bytes().starts_with(b"GIT_") {
                saved.push((key.clone(), Some(value)));
                set_env(&key, None);
            }
        }
        for (key, value) in [
            ("GIT_CONFIG_NOSYSTEM", "1"),
            ("GIT_CONFIG_GLOBAL", "/dev/null"),
        ] {
            let key = OsString::from(key);
            if !saved.iter().any(|(old, _)| old == &key) {
                saved.push((key.clone(), None));
            }
            set_env(&key, Some(OsStr::new(value)));
        }
        Self(saved)
    }
}

impl Drop for GitEnvGuard {
    fn drop(&mut self) {
        for (name, value) in self.0.drain(..).rev() {
            set_env(&name, value.as_deref());
        }
    }
}

fn set_env(key: &OsStr, value: Option<&OsStr>) {
    match value {
        Some(value) => std::env::set_var(key, value),
        None => std::env::remove_var(key),
    }
    Python::attach(|py| {
        let environ = PyModule::import(py, "os")
            .unwrap()
            .getattr("environb")
            .unwrap();
        let name = PyBytes::new(py, key.as_bytes());
        match value {
            Some(value) => environ
                .set_item(name, PyBytes::new(py, value.as_bytes()))
                .unwrap(),
            None => {
                environ.call_method1("pop", (name, py.None())).unwrap();
            }
        }
    });
}

pub fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?} in {}: {}",
        repo.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

pub fn repository(case: &WorkspaceCase) -> PathBuf {
    let repo = case.mkdir("repo");
    git(&repo, &["init", "--quiet", "-b", "main"]);
    git(&repo, &["config", "user.name", "Snapshot Test"]);
    git(&repo, &["config", "user.email", "snapshot@test.invalid"]);
    git(&repo, &["config", "commit.gpgsign", "false"]);
    git(&repo, &["config", "core.hooksPath", "/dev/null"]);
    fs::write(repo.join("changed.py"), "before\n").unwrap();
    fs::write(repo.join("deleted.py"), "remove me\n").unwrap();
    git(&repo, &["add", "changed.py", "deleted.py"]);
    git(&repo, &["commit", "--quiet", "-m", "baseline"]);
    repo
}

pub fn object_inventory(repo: &Path) -> Vec<String> {
    let raw = PathBuf::from(git(repo, &["rev-parse", "--git-path", "objects"]));
    let object_dir = if raw.is_absolute() {
        raw
    } else {
        repo.join(raw)
    };
    let mut files = Vec::new();
    let mut pending = vec![object_dir.clone()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if entry.file_type().unwrap().is_dir() {
                pending.push(path);
            } else if entry.file_type().unwrap().is_file() {
                files.push(
                    path.strip_prefix(&object_dir)
                        .unwrap()
                        .to_string_lossy()
                        .replace('\\', "/"),
                );
            }
        }
    }
    files.sort();
    files
}

pub struct SnapshotGuard {
    context: Py<PyAny>,
}

impl SnapshotGuard {
    pub fn enter<'py>(py: Python<'py>, repo: &Path) -> (Self, Bound<'py, PyAny>) {
        let source = crate::support::module(py, "conductor.snapshot_worktree");
        let context = source
            .getattr("isolated_snapshot")
            .unwrap()
            .call1((crate::support::path(py, repo),))
            .unwrap();
        let value = context.call_method0("__enter__").unwrap();
        (
            Self {
                context: context.unbind(),
            },
            value,
        )
    }
}

impl Drop for SnapshotGuard {
    fn drop(&mut self) {
        Python::attach(|py| {
            self.context
                .bind(py)
                .call_method1("__exit__", (py.None(), py.None(), py.None()))
                .expect("exit fixture snapshot");
        });
    }
}
