#![cfg(feature = "python-compat-tests")]
//! The installed `forge_test_record` pytest plugin, run in a child pytest over a
//! scratch project, against the rows it stores.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::prelude::*;
use pyo3::types::{PyDict, PySet};
use rusqlite::Connection;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use support::{module, path, text, AttrPatch, Case};

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
    // Exit 1: test_fails fails by design.
    pytest_in(&python(), root, extra_env, 1)
}

fn pytest_in(python: &OsStr, root: &Path, extra_env: &[(&str, &str)], code: i32) -> String {
    let mut child = Command::new(python)
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
    assert_eq!(output.status.code(), Some(code), "{stdout}\n{stderr}");
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
    // The key holds installed names only: a version bump is judged per test.
    assert!(env["installed"]
        .as_array()
        .is_some_and(|names| names.iter().any(|name| name == "pytest")));
    assert!(env.get("dists").is_none());
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

fn recorded(store: &Path) -> Vec<(i64, String)> {
    let connection = Connection::open(store).expect("open store");
    let mut query = connection
        .prepare("SELECT session, nodeid FROM runs ORDER BY id")
        .expect("runs query");
    query
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .expect("runs")
        .collect::<Result<_, _>>()
        .expect("run rows")
}

#[test]
fn reuse_skips_recorded_passes_until_an_input_changes() {
    let case = Case::new();
    project(&case);
    pytest(case.root(), &[]);
    let report = case.root().join("reuse-report");
    let reuse = [
        ("FORGE_TEST_RECORD_REUSE", "1"),
        (
            "FORGE_TEST_RECORD_REUSE_REPORT",
            report.to_str().expect("UTF-8 path"),
        ),
        ("PYTEST_ADDOPTS", "-rs"),
    ];
    // pytest's progress characters, one per test in definition order.
    let progress = |stdout: &str| stdout.lines().next().unwrap_or_default().to_owned();

    // A pass with no opaque step is skipped; a failure, an xfail, a subprocess and a
    // test that was never recorded run.
    let stdout = pytest(case.root(), &reuse);
    assert!(
        progress(&stdout).starts_with("sFxs..")
            && stdout.contains("test record reuse: 2 of 6 tests passed before against identical inputs and were skipped")
            && stdout.contains("SKIPPED [2] tests/test_pkg.py: forge test record: passed in session 1 against identical inputs")
            && stdout.contains("1 failed, 2 passed, 2 skipped, 1 xfailed"),
        "{stdout}"
    );
    let files: Vec<_> = std::fs::read_dir(&report)
        .expect("reuse report dir")
        .map(|entry| entry.expect("report entry").path())
        .collect();
    assert_eq!(files.len(), 1, "{files:?}");
    let counts: Value =
        serde_json::from_slice(&std::fs::read(&files[0]).expect("report")).expect("JSON report");
    assert_eq!(
        (counts["collected"].as_u64(), counts["reused"].as_u64()),
        (Some(6), Some(2))
    );
    assert!(
        counts["recorded_seconds"].as_f64().is_some_and(|s| s > 0.0),
        "{counts}"
    );
    let store = case.root().join("record/test_record.sqlite");
    let second: Vec<String> = recorded(&store)
        .into_iter()
        .filter(|(session, _)| *session == 2)
        .map(|(_, nodeid)| nodeid)
        .collect();
    assert!(
        !second
            .iter()
            .any(|nodeid| nodeid.ends_with("::test_fixture") || nodeid.ends_with("::test_reads")),
        "a skipped test is not recorded again: {second:?}"
    );

    // The fixture's data file changes: only its test runs again.
    case.write("tests/table.txt", "1  2\n");
    let stdout = pytest(case.root(), &reuse);
    assert!(
        progress(&stdout).starts_with(".Fxs..")
            && stdout.contains("test record reuse: 1 of 6 tests")
            && stdout
                .contains("SKIPPED [1] tests/test_pkg.py: forge test record: passed in session 1")
            && stdout.contains("1 failed, 3 passed, 1 skipped, 1 xfailed"),
        "{stdout}"
    );
}

/// A venv inside the project whose interpreter finds pytest and the plugin through
/// the host venv, and whose own site directory holds `fakedist`. Returns its python
/// and site directory.
fn venv_with_fakedist(case: &Case) -> (PathBuf, PathBuf) {
    let venv = case.root().join("venv");
    let status = Command::new(python())
        .args(["-m", "venv", "--without-pip"])
        .arg(&venv)
        .status()
        .expect("create venv");
    assert!(status.success());
    let host = Command::new(python())
        .args(["-c", "import site; print(site.getsitepackages()[0])"])
        .output()
        .expect("host site directory");
    let host = String::from_utf8(host.stdout).expect("UTF-8 path");
    let site = venv.join("lib/python3.12/site-packages");
    assert!(site.is_dir(), "{}", site.display());
    std::fs::write(
        site.join("host.pth"),
        format!("import site; site.addsitedir({:?})\n", host.trim()),
    )
    .expect("host .pth");
    fakedist(&site, "1");
    (venv.join("bin/python"), site)
}

fn fakedist(site: &Path, value: &str) {
    std::fs::create_dir_all(site.join("fakedist")).expect("fakedist dir");
    std::fs::create_dir_all(site.join("fakedist-1.0.dist-info")).expect("fakedist metadata");
    std::fs::write(
        site.join("fakedist/__init__.py"),
        format!("def value():\n    return {value}\n"),
    )
    .expect("fakedist code");
    std::fs::write(
        site.join("fakedist-1.0.dist-info/RECORD"),
        format!("fakedist/__init__.py,sha256={value},1\nfakedist-1.0.dist-info/RECORD,,\n"),
    )
    .expect("fakedist RECORD");
    std::fs::write(
        site.join("fakedist-1.0.dist-info/METADATA"),
        "Metadata-Version: 2.1\nName: fakedist\nVersion: 1.0\n",
    )
    .expect("fakedist METADATA");
}

#[test]
fn a_rebuilt_distribution_reruns_only_the_tests_that_loaded_it() {
    let case = Case::new();
    case.write("pyproject.toml", PYPROJECT);
    case.write("conductor/__init__.py", "");
    case.write(
        "tests/test_dist.py",
        "import fakedist\n\n\ndef test_uses_dist():\n    assert fakedist.value() > 0\n",
    );
    case.write(
        "tests/test_plain.py",
        "def test_plain():\n    assert 1 + 1 == 2\n",
    );
    let (python, site) = venv_with_fakedist(&case);
    let reuse = [("FORGE_TEST_RECORD_REUSE", "1"), ("PYTEST_ADDOPTS", "-rs")];
    let progress = |stdout: &str| stdout.lines().next().unwrap_or_default().to_owned();
    let stdout = pytest_in(python.as_os_str(), case.root(), &[], 0);
    assert!(progress(&stdout).starts_with(".."), "{stdout}");
    let store = case.root().join("record/test_record.sqlite");
    let recorded = runs(&store);
    let installed = |nodeid: &str| -> Vec<String> {
        recorded[nodeid]
            .deps
            .keys()
            .filter(|(kind, _)| kind == "inst")
            .map(|(_, label)| label.clone())
            .collect()
    };
    assert_eq!(
        installed("tests/test_dist.py::test_uses_dist"),
        ["fakedist-1.0.dist-info"]
    );
    assert!(installed("tests/test_plain.py::test_plain").is_empty());

    // The same version rebuilt with other code: only the test that loaded it reruns.
    fakedist(&site, "2");
    let stdout = pytest_in(python.as_os_str(), case.root(), &reuse, 0);
    assert!(
        progress(&stdout).starts_with(".s") && stdout.contains("1 passed, 1 skipped"),
        "{stdout}"
    );

    // Installing a distribution changes the environment key: nothing is reused.
    std::fs::create_dir_all(site.join("other-1.0.dist-info")).expect("other metadata");
    let stdout = pytest_in(python.as_os_str(), case.root(), &reuse, 0);
    assert!(
        progress(&stdout).starts_with("..") && stdout.contains("2 passed"),
        "{stdout}"
    );
}

#[test]
fn a_changed_function_no_recorded_test_ran_is_reported() {
    let case = Case::new();
    project(&case);
    case.write(
        "pkg/more.py",
        "def used():\n    return 1\n\n\ndef unused():\n    return 2\n",
    );
    case.write(
        "tests/test_more.py",
        "from pkg.more import used\n\n\ndef test_used():\n    assert used() == 1\n",
    );
    pytest(case.root(), &[]);
    Python::attach(|py| {
        let glue = module(py, "conductor.candidate_review.untested_changes");
        // `unused` was imported at collection but never called; `double`'s line 3
        // ran in test_fails.
        let changed = PyDict::new(py);
        changed
            .set_item("pkg/more.py", PySet::new(py, [2, 6]).unwrap())
            .unwrap();
        changed
            .set_item("pkg/helpers.py", PySet::new(py, [3]).unwrap())
            .unwrap();
        let mock_args = PyDict::new(py);
        mock_args.set_item("return_value", changed).unwrap();
        let lines = module(py, "unittest.mock")
            .getattr("Mock")
            .unwrap()
            .call((), Some(&mock_args))
            .unwrap();
        let _guard = AttrPatch::replace(&glue, "changed_source_lines", &lines);
        let context_args = PyDict::new(py);
        context_args
            .set_item("snapshot", path(py, case.root()))
            .unwrap();
        let context = module(py, "types")
            .getattr("SimpleNamespace")
            .unwrap()
            .call((), Some(&context_args))
            .unwrap();
        let (findings, metrics): (Vec<Bound<'_, PyAny>>, BTreeMap<String, usize>) = glue
            .getattr("untested_changes")
            .unwrap()
            .call1((context, "targeted-tests-full"))
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(
            metrics,
            BTreeMap::from([
                ("changed_functions".to_owned(), 3),
                ("untested_changed_functions".to_owned(), 1),
            ])
        );
        assert_eq!(findings.len(), 1);
        let finding = &findings[0];
        assert_eq!(
            text(&finding.getattr("rule_id").unwrap()),
            "untested-changed-function"
        );
        assert_eq!(text(&finding.getattr("severity").unwrap()), "medium");
        assert_eq!(
            text(&finding.getattr("message").unwrap()),
            "1 of 3 changed functions ran in no recorded test: pkg/more.py:5 unused"
        );
    });
}
