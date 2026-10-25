#![cfg(feature = "python-compat-tests")]
//! Candidate review isolation contracts.

#[path = "python_contracts/candidate_review_support.rs"]
#[allow(dead_code)]
mod candidate_review_support;
#[path = "python_contracts/git_fixture_support.rs"]
#[allow(dead_code)]
mod git_fixture_support;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use candidate_review_support as fixture;
use pyo3::prelude::*;
use serde_json::{json, Value};
use std::fs;
use std::path::PathBuf;
use support::{module, Case};

fn side_effect_probe_repo(case: &Case) -> PathBuf {
    let repo = case.root().join("repo");
    fixture::init_repo(&repo);
    let artifacts = case.root().join("gate-findings");
    fixture::write_fixture(
        &repo,
        "pyproject.toml",
        &format!(
            "[tool.forge.equivalence_probe]\nrun_artifacts = {:?}\n",
            artifacts.to_str().unwrap()
        ),
    );
    fixture::write_fixture(
        &repo,
        "subject.py",
        r#"
from pathlib import Path
import sqlite3

DB_PATH = Path(__file__).with_name("evaluation.db")
with sqlite3.connect(DB_PATH) as connection:
    connection.execute("CREATE TABLE IF NOT EXISTS probe_runs (value INTEGER)")

def evaluate(value):
    return value + 1
"#,
    );
    fixture::write_fixture(
        &repo,
        "test_subject.py",
        r#"
from subject import DB_PATH, evaluate

def test_evaluate():
    assert evaluate(4) == 5
    assert DB_PATH.is_file()
"#,
    );
    fixture::commit_all(&repo, "baseline");
    fixture::write_fixture(
        &repo,
        "subject.py",
        r#"
from pathlib import Path
import sqlite3

DB_PATH = Path(__file__).with_name("evaluation.db")
with sqlite3.connect(DB_PATH) as connection:
    connection.execute("CREATE TABLE IF NOT EXISTS probe_runs (value INTEGER)")

def evaluate(value):
    return value + 1

def candidate_only(value):
    return value * 2
"#,
    );
    fixture::git(&repo, &["add", "subject.py"]);
    repo
}

fn candidate<'py>(
    py: Python<'py>,
    repo: &std::path::Path,
    policy: &Bound<'py, PyAny>,
) -> Bound<'py, PyAny> {
    let resolved = fixture::resolve_candidate(py, repo, "index", None, None);
    fixture::classify_candidate(py, &resolved, policy)
}

#[test]
fn equivalence_probe_replays_side_effects_outside_candidate_snapshot() {
    let case = fixture::isolated_case();
    let repo = side_effect_probe_repo(&case);
    Python::attach(|py| {
        let policy = fixture::default_policy(py);
        let candidate = candidate(py, &repo, &policy);
        let source_before = fs::read(repo.join("subject.py")).unwrap();
        let test_before = fs::read(repo.join("test_subject.py")).unwrap();
        let artifact_dir = case.root().join("gate-findings");

        fixture::with_materialized(
            py,
            &repo,
            &fixture::string_attr(&candidate, "tree_oid"),
            None,
            |snapshot, entries| {
                let snapshot_source = snapshot.join("subject.py");
                let snapshot_test = snapshot.join("test_subject.py");
                let source_before = fs::read(&snapshot_source).unwrap();
                let test_before = fs::read(&snapshot_test).unwrap();
                let ctx = fixture::review_context(
                    py,
                    &repo,
                    snapshot,
                    &candidate,
                    entries,
                    &policy,
                    "pre-commit",
                    "full",
                    None,
                    &case.root().join("runtime-equivalence-probe"),
                );
                let result = module(py, "conductor.candidate_review.equivalence_probe_check")
                    .getattr("check_equivalence_probe")
                    .unwrap()
                    .call1((ctx,))
                    .unwrap();

                assert_eq!(
                    result
                        .getattr("metrics")
                        .unwrap()
                        .get_item("modules_probed")
                        .unwrap()
                        .extract::<usize>()
                        .unwrap(),
                    1,
                    "check result: {}",
                    result.repr().unwrap()
                );
                assert!(!snapshot.join("evaluation.db").exists());
                assert_eq!(fs::read(snapshot_source).unwrap(), source_before);
                assert_eq!(fs::read(snapshot_test).unwrap(), test_before);
            },
        );

        assert_eq!(fs::read(repo.join("subject.py")).unwrap(), source_before);
        assert_eq!(fs::read(repo.join("test_subject.py")).unwrap(), test_before);
        let artifacts: Vec<_> = fs::read_dir(&artifact_dir).unwrap().collect();
        assert_eq!(artifacts.len(), 1);
        let summary: Value =
            serde_json::from_slice(&fs::read(artifacts[0].as_ref().unwrap().path()).unwrap())
                .unwrap();
        assert_eq!(summary["modules_probed"], 1);
        assert_eq!(summary["modules_without_drivers"], json!([]));
        assert_eq!(summary["incomplete"], json!([]));
    });
}
