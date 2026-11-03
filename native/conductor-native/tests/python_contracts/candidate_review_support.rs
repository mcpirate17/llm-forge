//! Minimal retained Git source fixtures for flow and runtime contracts.

pub use super::git_fixture_support::git;
use super::support::{module, path, Case};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict};
use std::fs;
use std::path::{Path, PathBuf};

pub fn isolated_case() -> Case {
    let mut case = Case::new();
    for name in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_NAMESPACE",
        "GIT_CEILING_DIRECTORIES",
        "GIT_CONFIG_COUNT",
        "GIT_CONFIG_PARAMETERS",
        "GIT_CONFIG_KEY_0",
        "GIT_CONFIG_VALUE_0",
    ] {
        case.remove_env(name);
    }
    case.set_env("GIT_CONFIG_NOSYSTEM", "1");
    case.set_env("GIT_CONFIG_GLOBAL", "/dev/null");
    case.set_env("GIT_CONFIG_SYSTEM", "/dev/null");
    case
}

pub fn init_repo(repo: &Path) {
    fs::create_dir_all(repo).unwrap();
    git(repo, &["init", "--quiet", "--initial-branch=main"]);
    git(repo, &["config", "user.name", "Candidate Review Test"]);
    git(
        repo,
        &["config", "user.email", "candidate-review@example.invalid"],
    );
    git(repo, &["config", "commit.gpgsign", "false"]);
    git(repo, &["config", "core.hooksPath", "/dev/null"]);
}

pub fn commit_all(repo: &Path, message: &str) -> String {
    git(repo, &["add", "--all"]);
    git(
        repo,
        &[
            "commit",
            "--quiet",
            "--message",
            &format!("{message}\n\nAgent: llm-fixture\n"),
        ],
    );
    git(repo, &["rev-parse", "HEAD"])
}

pub fn write_fixture(repo: &Path, relative: &str, contents: &str) -> PathBuf {
    let target = repo.join(relative);
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    fs::write(&target, contents).unwrap();
    target
}

pub fn resolve_candidate<'py>(
    py: Python<'py>,
    repo: &Path,
    kind: &str,
    base_ref: Option<&str>,
    target_ref: Option<&str>,
) -> Bound<'py, PyAny> {
    let kwargs = PyDict::new(py);
    kwargs.set_item("kind", kind).unwrap();
    if let Some(base) = base_ref {
        kwargs.set_item("base_ref", base).unwrap();
    }
    if let Some(target) = target_ref {
        kwargs.set_item("target_ref", target).unwrap();
    }
    module(py, "conductor.candidate_review.git_source")
        .getattr("resolve_candidate")
        .unwrap()
        .call((path(py, repo),), Some(&kwargs))
        .unwrap()
}

pub fn with_materialized<'py, R>(
    py: Python<'py>,
    repo: &Path,
    tree_oid: &str,
    max_blob_bytes: Option<u64>,
    body: impl FnOnce(&Path, &Bound<'py, PyAny>) -> R,
) -> R {
    let kwargs = PyDict::new(py);
    if let Some(limit) = max_blob_bytes {
        kwargs.set_item("max_blob_bytes", limit).unwrap();
    }
    let manager = module(py, "conductor.candidate_review.git_source")
        .getattr("materialize_tree")
        .unwrap()
        .call((path(py, repo), tree_oid), Some(&kwargs))
        .unwrap();
    let entered = manager.call_method0("__enter__").unwrap();
    let snapshot: PathBuf = entered.get_item(0).unwrap().extract().unwrap();
    let entries = entered.get_item(1).unwrap();
    let guard = ContextExit(manager.unbind());
    let result = body(&snapshot, &entries);
    drop(guard);
    result
}

struct ContextExit(Py<PyAny>);

impl Drop for ContextExit {
    fn drop(&mut self) {
        Python::attach(|py| {
            self.0
                .bind(py)
                .call_method1("__exit__", (py.None(), py.None(), py.None()))
                .expect("close materialized snapshot");
        });
    }
}

pub fn string_attr(object: &Bound<'_, PyAny>, name: &str) -> String {
    object.getattr(name).unwrap().extract().unwrap()
}
