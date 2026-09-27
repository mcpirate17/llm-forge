#![cfg(feature = "python-compat-tests")]
//! Rust-owned contracts for snapshotting and fast-forwarding fixture checkouts.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::prelude::*;
use pyo3::types::PyDict;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use support::{assert_error, module, path, text, Case};

fn checkout_case() -> Case {
    let mut case = Case::new();
    case.set_env("CONDUCTOR_INTEGRATION_BRANCH", "master");
    case.set_env("GIT_CONFIG_NOSYSTEM", "1");
    case.set_env("GIT_CONFIG_GLOBAL", "/dev/null");
    for key in ["GIT_INDEX_FILE", "GIT_DIR", "GIT_WORK_TREE"] {
        case.remove_env(key);
    }
    case
}

fn git(repo: &Path, args: &[&str]) -> String {
    let done = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .expect("run fixture Git command");
    assert!(
        done.status.success(),
        "git {args:?} in {} failed: {}",
        repo.display(),
        String::from_utf8_lossy(&done.stderr)
    );
    String::from_utf8(done.stdout).expect("UTF-8 fixture Git output")
}

fn pair(case: &Case) -> (PathBuf, PathBuf) {
    let upstream = case.mkdir("upstream");
    git(&upstream, &["init", "-q", "-b", "master"]);
    git(&upstream, &["config", "user.email", "t@example.invalid"]);
    git(&upstream, &["config", "user.name", "test"]);
    git(&upstream, &["config", "commit.gpgsign", "false"]);
    fs::write(upstream.join("tracked.txt"), "one\n").unwrap();
    git(&upstream, &["add", "-A"]);
    git(&upstream, &["commit", "-qm", "first"]);
    let clone = case.root().join("clone");
    git(
        case.root(),
        &[
            "clone",
            "-q",
            upstream.to_str().unwrap(),
            clone.to_str().unwrap(),
        ],
    );
    git(&clone, &["config", "user.email", "t@example.invalid"]);
    git(&clone, &["config", "user.name", "test"]);
    git(&clone, &["config", "commit.gpgsign", "false"]);
    (upstream, clone)
}

fn advance(upstream: &Path, clone: &Path, name: &str, body: &str) {
    fs::write(upstream.join(name), body).unwrap();
    git(upstream, &["add", "-A"]);
    git(upstream, &["commit", "-qm", &format!("add {name}")]);
    git(clone, &["fetch", "-q", "origin"]);
}

fn sync<'py>(py: Python<'py>, clone: &Path, dry_run: bool) -> Bound<'py, PyDict> {
    let kwargs = PyDict::new(py);
    kwargs.set_item("dry_run", dry_run).unwrap();
    module(py, "conductor.checkout_sync")
        .getattr("sync")
        .unwrap()
        .call((path(py, clone),), Some(&kwargs))
        .unwrap()
        .cast_into::<PyDict>()
        .unwrap()
}

fn field<'py>(result: &Bound<'py, PyDict>, name: &str) -> Bound<'py, pyo3::types::PyAny> {
    result.get_item(name).unwrap().unwrap()
}

#[test]
fn checkout_already_even_with_line_is_left_alone() {
    let case = checkout_case();
    let (_, clone) = pair(&case);
    Python::attach(|py| {
        let result = sync(py, &clone, false);
        assert_eq!(text(&field(&result, "outcome")), "already even");
        assert!(field(&result, "snapshot").is_none());
    });
}

#[test]
fn dirty_checkout_fast_forwards_and_whole_tree_is_recoverable() {
    let case = checkout_case();
    let (upstream, clone) = pair(&case);
    advance(&upstream, &clone, "incoming.txt", "landed\n");
    fs::write(clone.join("tracked.txt"), "edited locally\n").unwrap();
    fs::write(clone.join("untracked.txt"), "never committed\n").unwrap();
    Python::attach(|py| {
        let result = sync(py, &clone, false);
        assert_eq!(text(&field(&result, "outcome")), "fast-forwarded");
        assert_eq!(field(&result, "behind").extract::<i64>().unwrap(), 1);
        assert_eq!(
            fs::read_to_string(clone.join("incoming.txt")).unwrap(),
            "landed\n"
        );
        assert_eq!(
            fs::read_to_string(clone.join("tracked.txt")).unwrap(),
            "edited locally\n"
        );
        let saved = text(&field(&result, "snapshot"));
        let namespace = text(
            &module(py, "conductor.checkout_sync")
                .getattr("SNAPSHOT_NAMESPACE")
                .unwrap(),
        );
        assert!(saved.starts_with(&namespace));
        assert_eq!(
            git(&clone, &["show", &format!("{saved}:untracked.txt")]),
            "never committed\n"
        );
        assert_eq!(
            git(&clone, &["show", &format!("{saved}:tracked.txt")]),
            "edited locally\n"
        );
    });
}

#[test]
fn snapshot_never_stages_anything_in_callers_index() {
    let case = checkout_case();
    let (_, clone) = pair(&case);
    fs::write(clone.join("untracked.txt"), "never committed\n").unwrap();
    Python::attach(|py| {
        module(py, "conductor.checkout_sync")
            .getattr("snapshot")
            .unwrap()
            .call1((path(py, &clone),))
            .unwrap();
    });
    assert_eq!(git(&clone, &["diff", "--cached", "--name-only"]), "");
    assert!(git(&clone, &["status", "--porcelain"]).contains("?? untracked.txt"));
}

#[test]
fn clean_tree_has_nothing_to_snapshot() {
    let case = checkout_case();
    let (_, clone) = pair(&case);
    Python::attach(|py| {
        assert!(module(py, "conductor.checkout_sync")
            .getattr("snapshot")
            .unwrap()
            .call1((path(py, &clone),))
            .unwrap()
            .is_none());
    });
}

#[test]
fn tracked_file_changed_on_both_sides_blocks_merge_and_names_itself() {
    let case = checkout_case();
    let (upstream, clone) = pair(&case);
    advance(&upstream, &clone, "tracked.txt", "changed upstream\n");
    fs::write(clone.join("tracked.txt"), "changed locally\n").unwrap();
    let before = git(&clone, &["rev-parse", "HEAD"]);
    Python::attach(|py| {
        let result = sync(py, &clone, false);
        assert_eq!(text(&field(&result, "outcome")), "blocked");
        assert_eq!(
            field(&result, "blocked_by")
                .extract::<Vec<String>>()
                .unwrap(),
            ["tracked.txt"]
        );
        assert!(field(&result, "snapshot").is_none());
    });
    assert_eq!(git(&clone, &["rev-parse", "HEAD"]), before);
}

#[test]
fn checkout_holding_unlanded_commits_is_refused() {
    let case = checkout_case();
    let (upstream, clone) = pair(&case);
    advance(&upstream, &clone, "incoming.txt", "landed\n");
    fs::write(clone.join("local.txt"), "mine\n").unwrap();
    git(&clone, &["add", "-A"]);
    git(&clone, &["commit", "-qm", "local work"]);
    Python::attach(|py| {
        let source = module(py, "conductor.checkout_sync");
        let error = source
            .getattr("sync")
            .unwrap()
            .call1((path(py, &clone),))
            .unwrap_err();
        assert_error(
            py,
            error,
            &source.getattr("SyncError").unwrap(),
            "rather than fast-forwarding over them",
        );
    });
}

#[test]
fn dry_run_reports_move_without_making_it() {
    let case = checkout_case();
    let (upstream, clone) = pair(&case);
    advance(&upstream, &clone, "incoming.txt", "landed\n");
    let before = git(&clone, &["rev-parse", "HEAD"]);
    Python::attach(|py| {
        let result = sync(py, &clone, true);
        assert_eq!(text(&field(&result, "outcome")), "would fast-forward");
        assert!(field(&result, "snapshot").is_none());
    });
    assert_eq!(git(&clone, &["rev-parse", "HEAD"]), before);
    assert!(!clone.join("incoming.txt").exists());
}
