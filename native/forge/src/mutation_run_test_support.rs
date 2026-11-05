//! Manifests, receipts and Git repositories for the mutation runner's unit tests.

use crate::mutation_campaign::Campaign;
use crate::mutation_score::{Receipt, RUNNING};
use serde_json::{json, Value};
use std::fs;
use std::path::Path;
use std::process::Command;

/// A loadable manifest in `dir`, top-level keys replaced by `overrides`.
pub(crate) fn manifest(dir: &Path, overrides: Value) -> std::path::PathBuf {
    let pin = "0".repeat(64);
    let mut payload = json!({
        "campaign_id": "fixture",
        "title": "fixture",
        "language": "python",
        "mutation_engine": "fest",
        "generator": {"source": ["src/a.py"], "run_timeout_seconds": 60},
        "test_argv": ["python", "-m", "pytest", "tests/test_a.py"],
        "source_sha256": {"src/a.py": pin},
        "test_sha256": {"tests/test_a.py": pin},
    });
    let fields = payload.as_object_mut().expect("object");
    for (key, value) in overrides.as_object().expect("overrides are an object") {
        fields.insert(key.clone(), value.clone());
    }
    let path = dir.join("campaign.json");
    fs::write(&path, payload.to_string()).expect("write manifest");
    path
}

pub(crate) fn campaign(dir: &Path, overrides: Value) -> Campaign {
    Campaign::load(&manifest(dir, overrides)).expect("fixture manifest loads")
}

/// A running receipt holding one row per `(id, outcome)`.
pub(crate) fn receipt(outcomes: &[(&str, &str)]) -> Receipt {
    let rows: Vec<Value> = outcomes
        .iter()
        .map(|(id, outcome)| json!({"id": id, "outcome": outcome}))
        .collect();
    let mut receipt = Receipt::new();
    receipt.insert("status".into(), json!(RUNNING));
    receipt.insert("mutants".into(), Value::Array(rows));
    receipt
}

pub(crate) fn git(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
        .args(args)
        .output()
        .expect("git runs");
    assert!(output.status.success(), "{args:?}: {output:?}");
}

pub(crate) fn write(root: &Path, relative: &str, body: &str) {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    fs::write(path, body).expect("write");
}

/// A repository with `files` committed on `main`, checked out on branch `feat`.
pub(crate) fn repository(files: &[&str]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    git(root, &["init", "-q", "-b", "main"]);
    for file in files {
        write(root, file, "x = 1\n");
    }
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "base", "--allow-empty"]);
    git(root, &["checkout", "-q", "-b", "feat"]);
    dir
}

/// Commit a change to `files`, so they are this branch's diff against `main`.
pub(crate) fn change(root: &Path, files: &[&str]) {
    for file in files {
        write(root, file, "x = 2\n");
    }
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "change"]);
}
