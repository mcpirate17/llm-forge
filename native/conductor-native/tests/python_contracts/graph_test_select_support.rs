//! Disposable Git and SQLite fixtures for the advisory graph selector.

use crate::support::{module, path, text, AttrPatch, Case};
use pyo3::prelude::*;
use pyo3::types::PyModule;
use rusqlite::{params, Connection};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct GraphRepo<'a> {
    case: &'a Case,
    root: PathBuf,
    database: PathBuf,
}

impl<'a> GraphRepo<'a> {
    pub fn new(case: &'a Case) -> Self {
        let root = case.mkdir("graph_ws");
        git(&root, &["init", "--quiet"]);
        git(&root, &["config", "user.name", "gselect"]);
        git(&root, &["config", "user.email", "gselect@test.com"]);
        case.write("graph_ws/base.py", "def base(): pass\n");
        git(&root, &["add", "base.py"]);
        git(&root, &["commit", "-m", "init-base", "--quiet"]);
        let database = Python::attach(|py| {
            let graph = module(py, "conductor.graph_test_select");
            let value = graph
                .getattr("graph_database_path")
                .unwrap()
                .call1((path(py, &root),))
                .unwrap();
            PathBuf::from(text(&value))
        });
        fs::create_dir_all(database.parent().unwrap()).unwrap();
        let connection = Connection::open(&database).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE nodes (qualified_name TEXT, file_path TEXT, is_test INTEGER);\n\
                 CREATE TABLE edges (source_qualified TEXT, target_qualified TEXT);",
            )
            .unwrap();
        Self {
            case,
            root,
            database,
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn database(&self) -> &Path {
        &self.database
    }

    pub fn write(&self, relative: &str, body: &str) -> PathBuf {
        self.case.write(&format!("graph_ws/{relative}"), body)
    }

    pub fn edge(&self, source: &str, test: &str) {
        let (source_abs, test_abs) = Python::attach(|py| {
            (
                resolved(py, &self.root, source),
                resolved(py, &self.root, test),
            )
        });
        let connection = Connection::open(&self.database).unwrap();
        connection
            .execute(
                "INSERT INTO nodes VALUES (?, ?, ?)",
                params!["pkg.engine", source_abs, 0],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO nodes VALUES (?, ?, ?)",
                params!["tests.test_engine", test_abs, 1],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO edges VALUES (?, ?)",
                params!["tests.test_engine", "pkg.engine"],
            )
            .unwrap();
    }
}

fn resolved(py: Python<'_>, root: &Path, relative: &str) -> String {
    let value = path(py, &root.join(relative))
        .call_method0("resolve")
        .unwrap();
    text(&value)
}

pub fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("LC_ALL", "C")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_CONFIG_COUNT")
        .env_remove("GIT_CONFIG_PARAMETERS")
        .output()
        .expect("run Git inside graph fixture");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

pub fn capture<T>(py: Python<'_>, action: impl FnOnce() -> T) -> (T, String, String) {
    let io = PyModule::import(py, "io").unwrap();
    let stdout = io.call_method0("StringIO").unwrap();
    let stderr = io.call_method0("StringIO").unwrap();
    let sys = PyModule::import(py, "sys").unwrap();
    let _out = AttrPatch::replace(sys.as_any(), "stdout", &stdout);
    let _err = AttrPatch::replace(sys.as_any(), "stderr", &stderr);
    let value = action();
    let out = stdout.call_method0("getvalue").unwrap().extract().unwrap();
    let err = stderr.call_method0("getvalue").unwrap().extract().unwrap();
    (value, out, err)
}

pub fn graph_module<'py>(py: Python<'py>) -> Bound<'py, PyModule> {
    module(py, "conductor.graph_test_select")
}
