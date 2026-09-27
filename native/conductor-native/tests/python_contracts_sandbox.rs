#![cfg(feature = "python-compat-tests")]
//! Rust-owned contracts for copy-only sandbox exports and their CLI boundary.

#[path = "python_contracts/agent_comm_support.rs"]
#[allow(dead_code)]
mod comm_support;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use comm_support::{buffer_text, capture};
use pyo3::prelude::*;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use support::{assert_error, module, path, text, Case};

fn git(root: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .env("GIT_AUTHOR_NAME", "Sandbox Test")
        .env("GIT_AUTHOR_EMAIL", "sandbox@example.invalid")
        .env("GIT_COMMITTER_NAME", "Sandbox Test")
        .env("GIT_COMMITTER_EMAIL", "sandbox@example.invalid")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .expect("run Git in temporary sandbox fixture");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("UTF-8 Git output")
}

fn repo(case: &Case) -> PathBuf {
    let root = case.mkdir("repo");
    fs::create_dir(root.join("wanted")).unwrap();
    fs::create_dir(root.join("unwanted")).unwrap();
    fs::write(root.join("wanted/tool.py"), "print('v1')\n").unwrap();
    fs::write(root.join("unwanted/heavy.pt"), b"weights").unwrap();
    git(&root, &["init", "-b", "master", "."]);
    git(&root, &["add", "."]);
    git(&root, &["commit", "-m", "one"]);
    root
}

fn export(
    py: Python<'_>,
    root: &Path,
    commit: &str,
    paths: &[&str],
    dest: &Path,
) -> PyResult<Py<PyAny>> {
    module(py, "conductor.sandbox")
        .getattr("export")?
        .call1((path(py, root), commit, paths.to_vec(), path(py, dest)))
        .map(Bound::unbind)
}

#[test]
fn export_takes_only_the_named_paths() {
    let case = Case::new();
    let root = repo(&case);
    let dest = case.root().join("box");
    Python::attach(|py| {
        let result = export(py, &root, "HEAD", &["wanted"], &dest).unwrap();
        assert_eq!(text(result.bind(py)), dest.display().to_string());
    });
    assert_eq!(
        fs::read_to_string(dest.join("wanted/tool.py")).unwrap(),
        "print('v1')\n"
    );
    assert!(!dest.join("unwanted").exists());
    assert!(!dest.join(".git").exists());
}

#[test]
fn export_is_frozen_at_the_commit() {
    let case = Case::new();
    let root = repo(&case);
    let first = git(&root, &["rev-parse", "HEAD"]).trim().to_owned();
    fs::write(root.join("wanted/tool.py"), "print('v2')\n").unwrap();
    git(&root, &["commit", "-am", "two"]);
    let dest = case.root().join("box");
    Python::attach(|py| {
        export(py, &root, &first, &["wanted"], &dest).unwrap();
    });
    assert_eq!(
        fs::read_to_string(dest.join("wanted/tool.py")).unwrap(),
        "print('v1')\n"
    );
}

#[test]
fn export_replaces_a_previous_sandbox() {
    let case = Case::new();
    let root = repo(&case);
    let dest = case.mkdir("box");
    fs::write(dest.join("stale.txt"), "old\n").unwrap();
    Python::attach(|py| {
        export(py, &root, "HEAD", &["wanted"], &dest).unwrap();
    });
    assert!(!dest.join("stale.txt").exists());
}

#[test]
fn export_refuses_an_empty_path_list() {
    let case = Case::new();
    let root = repo(&case);
    Python::attach(|py| {
        let error = export(py, &root, "HEAD", &[], &case.root().join("box")).unwrap_err();
        assert_error(
            py,
            error,
            &py.import("builtins")
                .unwrap()
                .getattr("ValueError")
                .unwrap(),
            "directories the run imports",
        );
    });
}

#[test]
fn export_fails_loudly_on_an_unknown_commit() {
    let case = Case::new();
    let root = repo(&case);
    let dest = case.root().join("box");
    Python::attach(|py| {
        let error = export(py, &root, "nope", &["wanted"], &dest).unwrap_err();
        assert_error(
            py,
            error,
            &py.import("builtins")
                .unwrap()
                .getattr("RuntimeError")
                .unwrap(),
            "git archive",
        );
    });
    assert!(!dest.exists());
}

#[test]
fn main_prints_the_sandbox_path() {
    let case = Case::new();
    let root = repo(&case);
    let outer = case.root().join("root");
    Python::attach(|py| {
        let sandbox = module(py, "conductor.sandbox");
        let (stdout, _capture) = capture(py, "stdout");
        let argv = vec![
            "--repo",
            root.to_str().unwrap(),
            "--root",
            outer.to_str().unwrap(),
            "--name",
            "run",
            "wanted",
        ];
        let code: i32 = sandbox
            .getattr("main")
            .unwrap()
            .call1((argv,))
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(code, 0);
        assert_eq!(
            buffer_text(&stdout).trim(),
            outer.join("run").to_str().unwrap()
        );
    });
    assert!(outer.join("run/wanted/tool.py").exists());
}

#[test]
fn main_reports_failure_without_a_traceback() {
    let case = Case::new();
    let root = repo(&case);
    let outer = case.root().join("root");
    Python::attach(|py| {
        let sandbox = module(py, "conductor.sandbox");
        let (stderr, _capture) = capture(py, "stderr");
        let argv = vec![
            "--repo",
            root.to_str().unwrap(),
            "--root",
            outer.to_str().unwrap(),
            "--name",
            "run",
            "absent",
        ];
        let code: i32 = sandbox
            .getattr("main")
            .unwrap()
            .call1((argv,))
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(code, 2);
        assert!(buffer_text(&stderr).contains("sandbox:"));
        assert!(!buffer_text(&stderr).contains("Traceback"));
    });
}

#[test]
fn default_root_prefers_the_session_scratchpad() {
    let mut case = Case::new();
    case.remove_env("SANDBOX_ROOT");
    case.set_env(
        "CLAUDE_SCRATCHPAD_DIR",
        case.root().join("scratch").to_str().unwrap(),
    );
    Python::attach(|py| {
        let sandbox = module(py, "conductor.sandbox");
        let root = sandbox.getattr("default_root").unwrap();
        assert_eq!(
            text(&root.call0().unwrap()),
            case.root().join("scratch").display().to_string()
        );
        case.set_env(
            "SANDBOX_ROOT",
            case.root().join("explicit").to_str().unwrap(),
        );
        assert_eq!(
            text(&root.call0().unwrap()),
            case.root().join("explicit").display().to_string()
        );
    });
}
