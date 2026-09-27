#![cfg(feature = "python-compat-tests")]
//! Rust-owned Python/Forge notes-index parity contracts.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::prelude::*;
use pyo3::types::{PyCFunction, PyDict, PyModule, PyString, PyTuple};
use rusqlite::Connection;
use serde_json::Value;
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};
use support::{module, AttrPatch, Case};

const TABLE_TITLED: &str = "\n| a | b |\n|---|---|\n| 1 | 2 |\n";
const TABLE_UNTITLED: &str = "\n| x | y | z |\n|---|---|---|\n| 1 | 2 | 3 |\n| 4 | 5 | 6 |\n";

fn fixture(case: &Case) -> (PathBuf, PathBuf) {
    let host = case.root().join("host");
    let vault = case.root().join("vault");
    for i in 1..=8 {
        case.write(
            &format!("host/research/notes/note{i}.md"),
            &format!("# Note {i}\n\nbody text {i} apple\n"),
        );
    }
    case.write(
        "host/research/notes/note_titled.md",
        &format!("# Titled Heading\n\nprose about apple banana\n{TABLE_TITLED}"),
    );
    case.write(
        "host/research/notes/note_untitled.md",
        &format!("prose with no heading, mentions apple\n{TABLE_UNTITLED}"),
    );
    case.write("host/tasks/task1.md", "# Task One\n\ntask body apple\n");
    case.write("host/tasks/task2.md", "# Task Two\n\ntask body banana\n");
    case.write(
        "host/tasks/audit/excluded.md",
        "# Excluded\n\nshould never be indexed apple\n",
    );
    for i in 1..=4 {
        case.write(
            &format!("vault/research/vnote{i}.md"),
            &format!("# Vault Note {i}\n\nvault body apple {i}\n"),
        );
    }
    case.write(
        "vault/dashboards/dash1.md",
        "# Dashboard One\n\ndashboard body apple\n",
    );
    case.write(
        "vault/runbooks/runbook1.md",
        "# Runbook One\n\nrunbook body apple\n",
    );
    (host, vault)
}

fn index_python(py: Python<'_>, host: &Path, vault: &Path, db: &Path) -> (usize, usize) {
    fs::write(db, "").unwrap();
    let notes = module(py, "conductor.index_notes");
    let host_text = host.to_str().unwrap();
    let vault_text = vault.to_str().unwrap();
    let _repo = AttrPatch::replace(&notes, "REPO", PyString::new(py, host_text).as_any());
    let _vault = AttrPatch::replace(&notes, "VAULT_ROOT", PyString::new(py, vault_text).as_any());
    let task_source = ("tasks", host.join("tasks").to_str().unwrap().to_owned())
        .into_pyobject(py)
        .unwrap();
    let _tasks = AttrPatch::replace(&notes, "TASKS_SOURCE", task_source.as_any());
    let vault_sources = PyTuple::new(
        py,
        [
            (
                "vault_research",
                vault.join("research").to_str().unwrap().to_owned(),
            ),
            (
                "vault_dashboards",
                vault.join("dashboards").to_str().unwrap().to_owned(),
            ),
            (
                "vault_runbooks",
                vault.join("runbooks").to_str().unwrap().to_owned(),
            ),
        ],
    )
    .unwrap();
    let _sources = AttrPatch::replace(&notes, "VAULT_SOURCES", vault_sources.as_any());
    let notes_root = host.join("research/notes").to_str().unwrap().to_owned();
    let fallback = PyCFunction::new_closure(
        py,
        None,
        None,
        move |args: &Bound<'_, PyTuple>,
              kw: Option<&Bound<'_, PyDict>>|
              -> PyResult<(String, String)> {
            if !args.is_empty() || kw.is_some_and(|kw| !kw.is_empty()) {
                return Err(pyo3::exceptions::PyTypeError::new_err(
                    "_fallback_notes_source takes no arguments",
                ));
            }
            Ok(("notes".to_owned(), notes_root.clone()))
        },
    )
    .unwrap();
    let _fallback = AttrPatch::replace(&notes, "_fallback_notes_source", fallback.as_any());
    let sqlite = py.import("sqlite3").unwrap();
    let conn = sqlite
        .getattr("connect")
        .unwrap()
        .call1((db.to_str().unwrap(),))
        .unwrap();
    let result = notes
        .getattr("rebuild")
        .unwrap()
        .call1((&conn,))
        .unwrap()
        .extract()
        .unwrap();
    conn.call_method0("close").unwrap();
    result
}

fn forge_bin() -> PathBuf {
    PathBuf::from(std::env::var("FORGE_BIN").expect("prebuilt FORGE_BIN for notes parity"))
}

fn stop_failed_child(child: &mut Child, reason: &str) -> ! {
    child.kill().expect("kill failed notes command");
    child.wait().expect("reap failed notes command");
    panic!("{reason}");
}

fn notes_output(command: &mut Command, capture_dir: &Path) -> Output {
    // File-backed capture cannot fill a pipe while we enforce the original
    // subprocess.run(timeout=60) deadline.
    let capture = |name| {
        fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(capture_dir.join(name))
            .unwrap()
    };
    let mut stdout = capture("notes.stdout");
    let mut stderr = capture("notes.stderr");
    let mut child = command
        .stdin(Stdio::null())
        .stdout(stdout.try_clone().unwrap())
        .stderr(stderr.try_clone().unwrap())
        .spawn()
        .expect("spawn notes command");
    let deadline = Instant::now() + Duration::from_secs(60);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(error) => stop_failed_child(&mut child, &format!("poll notes command: {error}")),
        }
        if Instant::now() >= deadline {
            stop_failed_child(&mut child, "notes command exceeded 60 seconds");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let mut out = Output {
        status,
        stdout: Vec::new(),
        stderr: Vec::new(),
    };
    stdout.seek(SeekFrom::Start(0)).unwrap();
    stderr.seek(SeekFrom::Start(0)).unwrap();
    stdout.read_to_end(&mut out.stdout).unwrap();
    stderr.read_to_end(&mut out.stderr).unwrap();
    out
}

fn index_forge(host: &Path, vault: &Path, db: &Path) {
    fs::write(db, "").unwrap();
    let output = notes_output(
        Command::new(forge_bin()).args([
            "notes",
            "index",
            "--host",
            host.to_str().unwrap(),
            "--vault",
            vault.to_str().unwrap(),
            "--db",
            db.to_str().unwrap(),
        ]),
        db.parent().unwrap(),
    );
    assert!(
        output.status.success(),
        "forge notes index failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

type FtsRow = (String, String, String, String, f64);
type TableRow = (
    String,
    String,
    String,
    i64,
    String,
    i64,
    i64,
    String,
    String,
);

fn fts_rows(db: &Path) -> Vec<FtsRow> {
    let conn = Connection::open(db).unwrap();
    let mut stmt = conn
        .prepare("SELECT path, source, title, body, mtime FROM notes_fts ORDER BY path, source")
        .unwrap();
    stmt.query_map([], |row| {
        Ok((
            row.get(0)?,
            row.get(1)?,
            row.get(2)?,
            row.get(3)?,
            row.get(4)?,
        ))
    })
    .unwrap()
    .map(Result::unwrap)
    .collect()
}

fn table_rows(db: &Path) -> Vec<TableRow> {
    let conn = Connection::open(db).unwrap();
    let mut stmt = conn.prepare("SELECT source, path, note, table_idx, section_heading, n_cols, n_rows, headers_json, rows_json FROM note_tables ORDER BY note, table_idx").unwrap();
    stmt.query_map([], |row| {
        Ok((
            row.get(0)?,
            row.get(1)?,
            row.get(2)?,
            row.get(3)?,
            row.get(4)?,
            row.get(5)?,
            row.get(6)?,
            row.get(7)?,
            row.get(8)?,
        ))
    })
    .unwrap()
    .map(Result::unwrap)
    .collect()
}

fn python_search(py: Python<'_>, db: &Path, query: &str) -> Vec<String> {
    let notes: Bound<'_, PyModule> = module(py, "conductor.index_notes");
    let conn = py
        .import("sqlite3")
        .unwrap()
        .getattr("connect")
        .unwrap()
        .call1((db.to_str().unwrap(),))
        .unwrap();
    let hits = notes
        .getattr("search_notes")
        .unwrap()
        .call1((&conn, query))
        .unwrap();
    let paths = hits
        .try_iter()
        .unwrap()
        .map(|hit| hit.unwrap().get_item("path").unwrap().extract().unwrap())
        .collect();
    conn.call_method0("close").unwrap();
    paths
}

fn forge_search(host: &Path, db: &Path, query: &str) -> Vec<String> {
    let output = notes_output(
        Command::new(forge_bin()).args([
            "notes",
            "search",
            "--host",
            host.to_str().unwrap(),
            "--db",
            db.to_str().unwrap(),
            "--json",
            query,
        ]),
        db.parent().unwrap(),
    );
    assert!(
        output.status.success(),
        "forge notes search failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let hits: Vec<Value> = serde_json::from_slice(&output.stdout).unwrap();
    hits.iter()
        .map(|hit| hit["path"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn python_and_forge_index_identically() {
    let case = Case::new();
    let (host, vault) = fixture(&case);
    let db_py = case.root().join("py.db");
    let db_rs = case.root().join("rs.db");
    let (n_files_py, _n_tables_py) = Python::attach(|py| index_python(py, &host, &vault, &db_py));
    index_forge(&host, &vault, &db_rs);
    assert_eq!(n_files_py, 18);
    let fts_py = fts_rows(&db_py);
    let fts_rs = fts_rows(&db_rs);
    assert_eq!(fts_py.len(), n_files_py);
    assert_eq!(fts_rs.len(), fts_py.len());
    for (left, right) in fts_py.iter().zip(fts_rs.iter()) {
        assert_eq!(
            (&left.0, &left.1, &left.2, &left.3),
            (&right.0, &right.1, &right.2, &right.3)
        );
        assert!(
            (left.4 - right.4).abs() < 1e-6,
            "mtime mismatch: {left:?} vs {right:?}"
        );
    }
    assert!(!fts_py.iter().any(|row| row.0.starts_with("tasks/audit/")));
    assert!(!fts_rs.iter().any(|row| row.0.starts_with("tasks/audit/")));
    let tables_py = table_rows(&db_py);
    let tables_rs = table_rows(&db_rs);
    assert_eq!(tables_py, tables_rs);
    assert_eq!(tables_py.len(), 2);
}

#[test]
fn python_and_forge_search_agree_for_all_parameter_rows() {
    for query in ["apple", "banana", "Titled Heading"] {
        let case = Case::new();
        let (host, vault) = fixture(&case);
        let db_py = case.root().join("py.db");
        let db_rs = case.root().join("rs.db");
        Python::attach(|py| index_python(py, &host, &vault, &db_py));
        index_forge(&host, &vault, &db_rs);
        let py_paths = Python::attach(|py| python_search(py, &db_py, query));
        let rs_paths = forge_search(&host, &db_rs, query);
        assert_eq!(py_paths, rs_paths, "query {query}");
        assert!(
            !py_paths.is_empty(),
            "query {query} matched no fixture rows"
        );
    }
}
