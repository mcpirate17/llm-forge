#![cfg(feature = "python-compat-tests")]
//! CLI root selection and output placement in disposable local Git repositories.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyList, PyModule};
use std::ffi::CString;
use std::path::Path;
use std::process::Command;
use support::{module, path, text, AttrPatch, Case};

const CLI_MOCK: &str = r#"
import contextlib
import io
calls = []
def fake_build_receipt(**kwargs):
    calls.append(kwargs['root'])
    return matrix.WorkspaceReceipt(1, 't', matrix.ReceiptStatus.PASS, (), {})
def invoke(args):
    out, err = io.StringIO(), io.StringIO()
    with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
        code = matrix.main(args)
    return code, out.getvalue(), err.getvalue()
"#;

fn git(repo: &Path, args: &[&str]) {
    let result = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&result.stderr)
    );
}

fn init_repo(repo: &Path) {
    std::fs::create_dir_all(repo).unwrap();
    git(repo, &["init", "-b", "main"]);
    git(
        repo,
        &["config", "user.email", "governance-tests@example.invalid"],
    );
    git(repo, &["config", "user.name", "Governance Tests"]);
    git(repo, &["config", "commit.gpgsign", "false"]);
    git(repo, &["commit", "--allow-empty", "-m", "base"]);
}

struct CliMock {
    globals: Py<PyDict>,
    _patch: AttrPatch,
}

impl CliMock {
    fn new(py: Python<'_>, matrix: &Bound<'_, PyModule>) -> Self {
        let globals = PyDict::new(py);
        globals.set_item("matrix", matrix).unwrap();
        py.run(&CString::new(CLI_MOCK).unwrap(), Some(&globals), None)
            .unwrap();
        let fake = globals.get_item("fake_build_receipt").unwrap().unwrap();
        let patch = AttrPatch::replace(matrix.as_any(), "build_receipt", &fake);
        Self {
            globals: globals.unbind(),
            _patch: patch,
        }
    }

    fn invoke(&self, py: Python<'_>, args: &[&str]) -> (i64, String, String) {
        let values = PyList::new(py, args).unwrap();
        self.globals
            .bind(py)
            .get_item("invoke")
            .unwrap()
            .unwrap()
            .call1((values,))
            .unwrap()
            .extract()
            .unwrap()
    }

    fn calls(&self, py: Python<'_>) -> Vec<String> {
        self.globals
            .bind(py)
            .get_item("calls")
            .unwrap()
            .unwrap()
            .extract::<Vec<Py<PyAny>>>()
            .unwrap()
            .iter()
            .map(|root| text(root.bind(py)))
            .collect()
    }
}

#[test]
fn explicit_root_scans_named_repo_instead_of_cwd() {
    let case = Case::new();
    let target = case.root().join("target");
    let decoy = case.root().join("decoy");
    init_repo(&target);
    init_repo(&decoy);
    let _cwd = case.chdir("decoy");
    Python::attach(|py| {
        let matrix = module(py, "conductor.workspace_runtime_matrix");
        let mock = CliMock::new(py, &matrix);
        let (code, _, _) =
            mock.invoke(py, &["--root", target.to_str().unwrap(), "--output", "out"]);
        assert_eq!(code, 0);
        assert_eq!(
            mock.calls(py),
            [target.canonicalize().unwrap().to_str().unwrap()]
        );
    });
}

#[test]
fn default_root_uses_current_worktree_not_module_location() {
    let case = Case::new();
    let repo = case.root().join("repo");
    init_repo(&repo);
    let _cwd = case.chdir("repo");
    Python::attach(|py| {
        let matrix = module(py, "conductor.workspace_runtime_matrix");
        let mock = CliMock::new(py, &matrix);
        assert_eq!(mock.invoke(py, &["--output", "out"]).0, 0);
        assert_eq!(
            mock.calls(py),
            [repo.canonicalize().unwrap().to_str().unwrap()]
        );
        assert_ne!(mock.calls(py)[0], text(&matrix.getattr("ROOT").unwrap()));
    });
}

#[test]
fn cwd_outside_worktree_refuses_without_building_receipt() {
    let case = Case::new();
    let _cwd = case.chdir("not_a_repo");
    Python::attach(|py| {
        let matrix = module(py, "conductor.workspace_runtime_matrix");
        let mock = CliMock::new(py, &matrix);
        assert_eq!(mock.invoke(py, &[]).0, 2);
        assert!(mock.calls(py).is_empty());
    });
}

#[test]
fn resolved_root_is_printed() {
    let case = Case::new();
    let repo = case.root().join("repo");
    init_repo(&repo);
    let _cwd = case.chdir("repo");
    Python::attach(|py| {
        let matrix = module(py, "conductor.workspace_runtime_matrix");
        let mock = CliMock::new(py, &matrix);
        let (code, out, _) = mock.invoke(py, &["--output", "out"]);
        assert_eq!(code, 0);
        assert!(out.contains(&format!("root={}", repo.canonicalize().unwrap().display())));
    });
}

#[test]
fn root_mismatch_warns_with_requested_root() {
    let case = Case::new();
    let target = case.root().join("target");
    let decoy = case.root().join("decoy");
    init_repo(&target);
    init_repo(&decoy);
    let _cwd = case.chdir("decoy");
    Python::attach(|py| {
        let matrix = module(py, "conductor.workspace_runtime_matrix");
        let mock = CliMock::new(py, &matrix);
        let (code, _, err) =
            mock.invoke(py, &["--root", target.to_str().unwrap(), "--output", "out"]);
        assert_eq!(code, 0);
        assert!(err.contains("WARNING"));
        assert!(err.contains(target.canonicalize().unwrap().to_str().unwrap()));
    });
}

#[test]
fn default_relative_output_is_resolved_against_root() {
    let case = Case::new();
    let repo = case.root().join("repo");
    init_repo(&repo);
    let _cwd = case.chdir("repo");
    Python::attach(|py| {
        let matrix = module(py, "conductor.workspace_runtime_matrix");
        let mock = CliMock::new(py, &matrix);
        assert_eq!(mock.invoke(py, &[]).0, 0);
        let default = matrix.getattr("DEFAULT_OUTPUT").unwrap();
        let output = repo.join(text(&default));
        assert!(output.join("receipt.json").is_file());
        assert_eq!(mock.calls(py), [text(&path(py, &repo))]);
    });
}
