//! Isolated Python object and Git fixtures for the candidate structure contracts.

use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyTuple};
use std::path::Path;
use std::process::Command;

use super::support::{module, path};

#[derive(Clone, Copy)]
pub enum ChangeKind {
    NativeModified,
    PythonAdded,
}

pub fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("LC_ALL", "C")
        .output()
        .expect("run Git in isolated candidate fixture");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

pub fn policy<'py>(py: Python<'py>) -> Bound<'py, PyAny> {
    let policy_module = module(py, "conductor.candidate_review.policy");
    let policy_path = module(py, "conductor.candidate_review.policy_path")
        .getattr("resolve_policy_path")
        .unwrap()
        .call0()
        .unwrap();
    policy_module
        .getattr("load_policy")
        .unwrap()
        .call1((policy_path,))
        .unwrap()
}

fn change<'py>(py: Python<'py>, rel: &str, kind: ChangeKind) -> Bound<'py, PyAny> {
    let model = module(py, "conductor.candidate_review.model");
    let kwargs = PyDict::new(py);
    let (status, old_mode, classes) = match kind {
        ChangeKind::NativeModified => ("M", "100644", ("native", "source")),
        ChangeKind::PythonAdded => ("A", "000000", ("python", "source")),
    };
    kwargs.set_item("status", status).unwrap();
    kwargs.set_item("path", rel).unwrap();
    kwargs.set_item("old_path", py.None()).unwrap();
    kwargs.set_item("old_mode", old_mode).unwrap();
    kwargs.set_item("new_mode", "100644").unwrap();
    kwargs.set_item("old_oid", "0".repeat(40)).unwrap();
    kwargs.set_item("new_oid", "1".repeat(40)).unwrap();
    kwargs.set_item("classes", classes).unwrap();
    model
        .getattr("Change")
        .unwrap()
        .call((), Some(&kwargs))
        .unwrap()
}

fn entry<'py>(py: Python<'py>, rel: &str) -> Bound<'py, PyAny> {
    let model = module(py, "conductor.candidate_review.model");
    let kwargs = PyDict::new(py);
    kwargs.set_item("path", rel).unwrap();
    kwargs.set_item("mode", "100644").unwrap();
    kwargs.set_item("object_type", "blob").unwrap();
    kwargs.set_item("oid", "0".repeat(40)).unwrap();
    model
        .getattr("TreeEntry")
        .unwrap()
        .call((), Some(&kwargs))
        .unwrap()
}

pub fn context<'py>(
    py: Python<'py>,
    repo: &Path,
    snapshot: &Path,
    base_tree: &str,
    changed: &[&str],
    entries: &[&str],
    kind: ChangeKind,
) -> Bound<'py, PyAny> {
    let model = module(py, "conductor.candidate_review.model");
    let checks = module(py, "conductor.candidate_review.checks");
    let candidate_kwargs = PyDict::new(py);
    candidate_kwargs.set_item("kind", "index").unwrap();
    candidate_kwargs
        .set_item("tree_oid", "a".repeat(40))
        .unwrap();
    candidate_kwargs
        .set_item("base_tree_oid", base_tree)
        .unwrap();
    match kind {
        ChangeKind::NativeModified => candidate_kwargs
            .set_item("base_commit_oid", py.None())
            .unwrap(),
        ChangeKind::PythonAdded => candidate_kwargs
            .set_item("base_commit_oid", "c".repeat(40))
            .unwrap(),
    }
    candidate_kwargs.set_item("commit_oid", py.None()).unwrap();
    candidate_kwargs.set_item("target_ref", "HEAD").unwrap();
    let changes = changed
        .iter()
        .map(|rel| change(py, rel, kind))
        .collect::<Vec<_>>();
    candidate_kwargs
        .set_item("changes", PyTuple::new(py, changes).unwrap())
        .unwrap();
    let candidate = model
        .getattr("Candidate")
        .unwrap()
        .call((), Some(&candidate_kwargs))
        .unwrap();
    let context_kwargs = PyDict::new(py);
    context_kwargs.set_item("repo", path(py, repo)).unwrap();
    context_kwargs
        .set_item("snapshot", path(py, snapshot))
        .unwrap();
    context_kwargs.set_item("candidate", candidate).unwrap();
    let tree_entries = entries.iter().map(|rel| entry(py, rel)).collect::<Vec<_>>();
    context_kwargs
        .set_item("entries", PyTuple::new(py, tree_entries).unwrap())
        .unwrap();
    context_kwargs.set_item("policy", policy(py)).unwrap();
    context_kwargs.set_item("surface", "manual").unwrap();
    context_kwargs.set_item("profile", "full").unwrap();
    context_kwargs.set_item("owner", py.None()).unwrap();
    context_kwargs
        .set_item(
            "runtime_dir",
            path(py, &snapshot.parent().unwrap().join("runtime")),
        )
        .unwrap();
    checks
        .getattr("ReviewContext")
        .unwrap()
        .call((), Some(&context_kwargs))
        .unwrap()
}
