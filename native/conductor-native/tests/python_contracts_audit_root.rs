#![cfg(feature = "python-compat-tests")]
//! Rust-owned assertions for Git audit-root resolution and provenance output.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::prelude::*;
use pyo3::types::PyDict;
use std::fs;
use std::path::Path;
use std::process::Command;
use support::{assert_error, module, path, text, AttrPatch, Case};

fn git(repo: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .expect("run fixture git command");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn init_repo(case: &Case, name: &str) -> std::path::PathBuf {
    let repo = case.mkdir(name);
    git(&repo, &["init", "-b", "main"]);
    git(
        &repo,
        &["config", "user.email", "governance-tests@example.invalid"],
    );
    git(&repo, &["config", "user.name", "Governance Tests"]);
    git(&repo, &["config", "commit.gpgsign", "false"]);
    fs::write(repo.join("marker.txt"), "x\n").unwrap();
    git(&repo, &["add", "marker.txt"]);
    git(&repo, &["commit", "-m", "init"]);
    repo
}

fn resolve_root<'py>(
    py: Python<'py>,
    explicit: Option<&Path>,
    cwd: &Path,
) -> PyResult<Bound<'py, PyAny>> {
    let kwargs = PyDict::new(py);
    kwargs.set_item("cwd", path(py, cwd))?;
    let resolver = module(py, "conductor.audit_root").getattr("resolve_audit_root")?;
    match explicit {
        Some(root) => resolver.call((root.to_str().unwrap(),), Some(&kwargs)),
        None => resolver.call((py.None(),), Some(&kwargs)),
    }
}

fn provenance(py: Python<'_>, root: &Path, cwd: &Path) -> (String, String) {
    let io = module(py, "io");
    let stdout = io.call_method0("StringIO").unwrap();
    let stderr = io.call_method0("StringIO").unwrap();
    let sys = module(py, "sys");
    let _stdout_patch = AttrPatch::replace(sys.as_any(), "stdout", &stdout);
    let _stderr_patch = AttrPatch::replace(sys.as_any(), "stderr", &stderr);
    let kwargs = PyDict::new(py);
    kwargs.set_item("cwd", path(py, cwd)).unwrap();
    module(py, "conductor.audit_root")
        .getattr("print_audit_provenance")
        .unwrap()
        .call(("demo-tool", path(py, root)), Some(&kwargs))
        .unwrap();
    (
        text(&stdout.call_method0("getvalue").unwrap()),
        text(&stderr.call_method0("getvalue").unwrap()),
    )
}

#[test]
fn explicit_root_honoured_over_cwd() {
    let case = Case::new();
    let real = init_repo(&case, "real");
    let other = init_repo(&case, "other");
    Python::attach(|py| {
        assert!(resolve_root(py, Some(&other), &real)
            .unwrap()
            .eq(path(py, &other))
            .unwrap());
    });
}

#[test]
fn default_resolution_uses_cwd_toplevel_not_a_nested_dir() {
    let case = Case::new();
    let repo = init_repo(&case, "repo");
    let nested = case.mkdir("repo/a/b");
    Python::attach(|py| {
        assert!(resolve_root(py, None, &nested)
            .unwrap()
            .eq(path(py, &repo))
            .unwrap());
    });
}

#[test]
fn cwd_outside_worktree_refuses_without_explicit_root() {
    let case = Case::new();
    let outside = case.mkdir("not_a_repo");
    Python::attach(|py| {
        let audit = module(py, "conductor.audit_root");
        let error = resolve_root(py, None, &outside).unwrap_err();
        assert_error(
            py,
            error,
            &audit.getattr("AuditRootError").unwrap(),
            "not inside a Git worktree",
        );
    });
}

#[test]
fn explicit_root_must_exist() {
    let case = Case::new();
    let repo = init_repo(&case, "repo");
    Python::attach(|py| {
        let audit = module(py, "conductor.audit_root");
        let error = resolve_root(py, Some(&case.root().join("missing")), &repo).unwrap_err();
        assert_error(
            py,
            error,
            &audit.getattr("AuditRootError").unwrap(),
            "does not exist",
        );
    });
}

#[test]
fn resolved_root_is_printed() {
    let case = Case::new();
    let repo = init_repo(&case, "repo");
    Python::attach(|py| {
        let (out, _) = provenance(py, &repo, &repo);
        assert!(out.contains(&format!("root={}", repo.display())), "{out}");
        assert!(out.contains("git-head="), "{out}");
    });
}

#[test]
fn mismatch_between_root_and_cwd_toplevel_warns() {
    let case = Case::new();
    let standing_in = init_repo(&case, "standing_in");
    let elsewhere = init_repo(&case, "elsewhere");
    Python::attach(|py| {
        let (_, err) = provenance(py, &elsewhere, &standing_in);
        assert!(err.contains("WARNING"), "{err}");
        assert!(err.contains(&elsewhere.display().to_string()), "{err}");
        assert!(err.contains(&standing_in.display().to_string()), "{err}");
    });
}

#[test]
fn no_warning_when_root_matches_cwd_toplevel() {
    let case = Case::new();
    let repo = init_repo(&case, "repo");
    Python::attach(|py| {
        let (_, err) = provenance(py, &repo, &repo);
        assert_eq!(err, "");
    });
}
