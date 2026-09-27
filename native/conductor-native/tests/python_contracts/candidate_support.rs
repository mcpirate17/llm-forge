//! Shared Rust-owned fixtures for candidate-review Python compatibility seams.

use pyo3::prelude::*;
use pyo3::types::{PyDict, PyTuple};
use std::path::Path;
use std::process::Command;

use super::support::{module, path};

pub fn review_context<'py>(py: Python<'py>, root: &Path) -> Bound<'py, PyAny> {
    let model = module(py, "conductor.candidate_review.model");
    let checks = module(py, "conductor.candidate_review.checks");
    let candidate_kwargs = PyDict::new(py);
    for (key, value) in [
        ("kind", "pr"),
        ("tree_oid", "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
        ("base_tree_oid", "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
        (
            "base_commit_oid",
            "cccccccccccccccccccccccccccccccccccccccc",
        ),
        ("commit_oid", "dddddddddddddddddddddddddddddddddddddddd"),
        ("target_ref", "refs/heads/main"),
    ] {
        candidate_kwargs.set_item(key, value).unwrap();
    }
    candidate_kwargs
        .set_item("changes", PyTuple::empty(py))
        .unwrap();
    let candidate = model
        .getattr("Candidate")
        .unwrap()
        .call((), Some(&candidate_kwargs))
        .unwrap();
    let kwargs = PyDict::new(py);
    kwargs.set_item("repo", path(py, root)).unwrap();
    kwargs.set_item("snapshot", path(py, root)).unwrap();
    kwargs.set_item("candidate", candidate).unwrap();
    kwargs.set_item("entries", PyTuple::empty(py)).unwrap();
    kwargs.set_item("policy", py.None()).unwrap();
    kwargs.set_item("surface", "test").unwrap();
    kwargs.set_item("profile", "fast").unwrap();
    kwargs.set_item("owner", py.None()).unwrap();
    kwargs
        .set_item("runtime_dir", path(py, &root.join("runtime")))
        .unwrap();
    checks
        .getattr("ReviewContext")
        .unwrap()
        .call((), Some(&kwargs))
        .unwrap()
}

pub fn git(root: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {}: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}
