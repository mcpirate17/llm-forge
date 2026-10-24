#![cfg(feature = "python-compat-tests")]
//! The installed `forge_test_record` pytest plugin, run in a child pytest over a
//! scratch project, against the rows it stores.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use rusqlite::Connection;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use support::Case;

const PYPROJECT: &str = "[project]\nname = \"scratch\"\nversion = \"0\"\n\n\
[tool.forge.test_record]\nstore = \"record/test_record.sqlite\"\nenvironment = [\"RECORD_KEY\"]\n";

const HELPERS: &str =
    "def double(value):\n    if value < 0:\n        return 0\n    return value * 2\n";

const CONFTEST: &str = "import pathlib\n\nimport pytest\n\n\n@pytest.fixture(scope=\"module\")\n\
def table():\n    return pathlib.Path(__file__).with_name(\"table.txt\").read_text().split()\n";

const TESTS: &str = r#"import os
import pathlib
import sqlite3
import subprocess
import sys

import pytest

from pkg.helpers import double

HERE = pathlib.Path(__file__).parent


def test_fixture(table):
    assert [double(int(v)) for v in table] == [2, 4]


def test_fails():
    assert double(-1) == 1


@pytest.mark.xfail(reason="known")
def test_xfail():
    assert False


def test_reads():
    assert (HERE / "data.bin").read_bytes() == b"x"
    assert "data.bin" in os.listdir(HERE)
    assert any(entry.name == "conftest.py" for entry in os.scandir(HERE / "deep"))
    os.close(os.open(HERE / "fd.txt", os.O_RDONLY))
    sqlite3.connect(HERE / "db.sqlite").close()


def test_subprocess():
    subprocess.run([sys.executable, "-c", "pass"], check=True)


def test_unstable():
    path = HERE / "mutable.txt"
    path.write_text(path.read_text() + "more")
"#;

fn python() -> OsString {
    std::env::var_os("PYO3_PYTHON").expect("make test exports PYO3_PYTHON")
}

fn project(case: &Case) {
    case.write("pyproject.toml", PYPROJECT);
    // The host's own regular `conductor` package shadows forge's on the path.
    case.write("conductor/__init__.py", "");
    case.write("pkg/__init__.py", "");
    case.write("pkg/helpers.py", HELPERS);
    case.write("tests/conftest.py", CONFTEST);
    case.write("tests/test_pkg.py", TESTS);
    case.write("tests/table.txt", "1 2\n");
    case.write("tests/data.bin", "x");
    case.write("tests/fd.txt", "fd");
    case.write("tests/db.sqlite", "");
    case.write("tests/mutable.txt", "once");
    case.write("tests/deep/conftest.py", "");
}

/// Runs pytest in `root` and returns its stdout.
fn pytest(root: &Path, extra_env: &[(&str, &str)]) -> String {
    let mut child = Command::new(python())
        .args(["-m", "pytest", "-q", "-p", "no:cacheprovider", "tests"])
        .current_dir(root)
        .env("PYTHONPATH", root)
        .env("CUDA_VISIBLE_DEVICES", "")
        .env("RECORD_KEY", "k")
        .env_remove("FORGE_TEST_RECORD")
        .envs(extra_env.iter().copied())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("launch child pytest");
    let deadline = Instant::now() + Duration::from_secs(120);
    while child.try_wait().expect("poll child pytest").is_none() {
        assert!(Instant::now() < deadline, "child pytest exceeded 120 s");
        std::thread::sleep(Duration::from_millis(50));
    }
    let output = child.wait_with_output().expect("child pytest output");
    let stdout = String::from_utf8(output.stdout).expect("UTF-8 stdout");
    let stderr = String::from_utf8_lossy(&output.stderr);
    // Exit 1: test_fails fails by design.
    assert_eq!(output.status.code(), Some(1), "{stdout}\n{stderr}");
    stdout
}

fn unpacked(blob: Vec<u8>) -> Value {
    serde_json::from_slice(&zstd::decode_all(blob.as_slice()).expect("zstd window"))
        .expect("JSON window")
}

/// One run: outcome, opaque reason, and its windows' deps and executed lines merged.
struct Run {
    outcome: String,
    opaque: Option<String>,
    deps: BTreeMap<(String, String), String>,
    lines: BTreeMap<String, BTreeSet<u64>>,
}

fn runs(store: &Path) -> BTreeMap<String, Run> {
    let connection = Connection::open(store).expect("open store");
    let mut query = connection
        .prepare("SELECT id, nodeid, outcome, opaque FROM runs")
        .expect("runs query");
    let rows: Vec<(i64, String, String, Option<String>)> = query
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .expect("runs")
        .collect::<Result<_, _>>()
        .expect("run rows");
    let mut windows = connection
        .prepare("SELECT w.deps, w.lines FROM run_windows r JOIN windows w ON w.digest = r.window WHERE r.run = ?1")
        .expect("windows query");
    let mut out = BTreeMap::new();
    for (id, nodeid, outcome, opaque) in rows {
        let mut run = Run {
            outcome,
            opaque,
            deps: BTreeMap::new(),
            lines: BTreeMap::new(),
        };
        let blobs: Vec<(Vec<u8>, Vec<u8>)> = windows
            .query_map([id], |row| Ok((row.get(0)?, row.get(1)?)))
            .expect("windows")
            .collect::<Result<_, _>>()
            .expect("window rows");
        for (deps, lines) in blobs {
            for dep in unpacked(deps).as_array().expect("deps array") {
                let text = |index: usize| dep[index].as_str().expect("dep field").to_owned();
                run.deps.insert((text(0), text(1)), text(2));
            }
            for file in unpacked(lines).as_array().expect("lines array") {
                let hit = file[2]
                    .as_array()
                    .expect("hit lines")
                    .iter()
                    .map(|line| line.as_u64().expect("line"));
                run.lines
                    .entry(file[0].as_str().expect("label").to_owned())
                    .or_default()
                    .extend(hit);
            }
        }
        out.insert(nodeid, run);
    }
    out
}

fn blob_oid(root: &Path, relative: &str) -> String {
    let output = Command::new("git")
        .args(["hash-object", relative])
        .current_dir(root)
        .output()
        .expect("git hash-object");
    assert!(output.status.success());
    String::from_utf8(output.stdout)
        .expect("oid")
        .trim()
        .to_owned()
}

#[test]
fn a_session_records_outcomes_dependencies_and_opacity() {
    let case = Case::new();
    project(&case);
    let stdout = pytest(case.root(), &[]);
    assert!(
        stdout.contains("test record: 5 test runs recorded")
            && stdout.contains(
                "1 not recorded because a file they depend on changed during the session (tests/mutable.txt)"
            ),
        "{stdout}"
    );
    let runs = runs(&case.root().join("record/test_record.sqlite"));
    let outcome = |name: &str| runs[&format!("tests/test_pkg.py::{name}")].outcome.as_str();
    assert_eq!(
        [
            outcome("test_fixture"),
            outcome("test_fails"),
            outcome("test_xfail"),
            outcome("test_reads")
        ],
        ["passed", "failed", "xfailed", "passed"]
    );
    assert!(!runs.contains_key("tests/test_pkg.py::test_unstable"));

    let fixture = &runs["tests/test_pkg.py::test_fixture"];
    let file = |path: &str| ("file".to_owned(), path.to_owned());
    assert_eq!(
        fixture.deps[&file("tests/table.txt")],
        blob_oid(case.root(), "tests/table.txt")
    );
    assert_eq!(
        fixture.deps[&file("pkg/helpers.py")],
        blob_oid(case.root(), "pkg/helpers.py")
    );
    assert_eq!(
        fixture.deps[&file("conftest.py")],
        "absent",
        "a conftest added at the root changes the test"
    );
    assert_eq!(
        fixture.lines["pkg/helpers.py"],
        BTreeSet::from([1, 2, 4]),
        "import-time def plus the positive branch"
    );
    assert_eq!(
        runs["tests/test_pkg.py::test_fails"].lines["pkg/helpers.py"],
        BTreeSet::from([1, 2, 3])
    );

    let reads = &runs["tests/test_pkg.py::test_reads"];
    for path in ["tests/data.bin", "tests/fd.txt", "tests/db.sqlite"] {
        assert_eq!(
            reads.deps[&file(path)],
            blob_oid(case.root(), path),
            "{path}"
        );
    }
    let listing = |path: &str| reads.deps.get(&("list".to_owned(), path.to_owned()));
    assert!(listing("tests").is_some_and(|value| value.starts_with("list:")));
    assert!(listing("tests/deep").is_some_and(|value| value.starts_with("list:")));
    assert_eq!(reads.opaque, None);
    assert_eq!(
        runs["tests/test_pkg.py::test_subprocess"].opaque.as_deref(),
        Some("subprocess.Popen")
    );

    let connection =
        Connection::open(case.root().join("record/test_record.sqlite")).expect("store");
    let env: Vec<u8> = connection
        .query_row("SELECT data FROM envs", [], |row| row.get(0))
        .expect("env row");
    let env = unpacked(env);
    assert_eq!(env["vars"]["RECORD_KEY"], "k");
    assert_eq!(env["vars"]["CUDA_VISIBLE_DEVICES"], "");
    assert!(env["dists"]
        .as_object()
        .is_some_and(|dists| dists.keys().any(|name| name.starts_with("pytest-"))));
}

#[test]
fn recording_is_off_without_the_table_or_with_the_switch() {
    let case = Case::new();
    project(&case);
    let stdout = pytest(case.root(), &[("FORGE_TEST_RECORD", "0")]);
    assert!(!stdout.contains("test record"), "{stdout}");
    case.write(
        "pyproject.toml",
        "[project]\nname = \"scratch\"\nversion = \"0\"\n",
    );
    let stdout = pytest(case.root(), &[]);
    assert!(!stdout.contains("test record"), "{stdout}");
    assert!(!case.root().join("record").exists());
}
