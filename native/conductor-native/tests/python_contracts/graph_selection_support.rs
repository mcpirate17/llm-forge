//! Isolated review-context and snapshot fixtures for re-export selection tests.

use crate::support::{module, path, Case};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyTuple};

pub const SOURCE: &str = "component_fab/equations/adaptation.py";
pub const MODULE: &str = "def adapt_equation_distribution():\n    return 1\n";
pub const INIT: &str = "from .adaptation import adapt_equation_distribution\n";
pub const SELECTED: &str = "component_fab/tests/test_equation_adaptation.py";

pub fn context<'py>(py: Python<'py>, case: &Case) -> Bound<'py, PyAny> {
    let model = module(py, "conductor.candidate_review.model");
    let checks = module(py, "conductor.candidate_review.checks");
    let candidate = PyDict::new(py);
    candidate.set_item("kind", "index").unwrap();
    candidate.set_item("tree_oid", "a".repeat(40)).unwrap();
    candidate.set_item("base_tree_oid", "b".repeat(40)).unwrap();
    candidate
        .set_item("base_commit_oid", "c".repeat(40))
        .unwrap();
    candidate.set_item("commit_oid", py.None()).unwrap();
    candidate.set_item("target_ref", "HEAD").unwrap();
    candidate.set_item("changes", PyTuple::empty(py)).unwrap();
    let candidate = model
        .getattr("Candidate")
        .unwrap()
        .call((), Some(&candidate))
        .unwrap();

    let policy_path = module(py, "conductor.candidate_review.policy_path")
        .getattr("resolve_policy_path")
        .unwrap()
        .call0()
        .unwrap();
    let policy = module(py, "conductor.candidate_review.policy")
        .getattr("load_policy")
        .unwrap()
        .call1((policy_path,))
        .unwrap();
    let kwargs = PyDict::new(py);
    kwargs.set_item("repo", path(py, case.root())).unwrap();
    kwargs
        .set_item("snapshot", path(py, &case.root().join("snapshot")))
        .unwrap();
    kwargs.set_item("candidate", candidate).unwrap();
    kwargs.set_item("entries", PyTuple::empty(py)).unwrap();
    kwargs.set_item("policy", policy).unwrap();
    kwargs.set_item("surface", "manual").unwrap();
    kwargs.set_item("profile", "fast").unwrap();
    kwargs.set_item("owner", py.None()).unwrap();
    kwargs
        .set_item("runtime_dir", path(py, &case.root().join("runtime")))
        .unwrap();
    checks
        .getattr("ReviewContext")
        .unwrap()
        .call((), Some(&kwargs))
        .unwrap()
}

pub fn snapshot(case: &Case, init: &str, source: &str, test: &str) {
    case.write(&format!("snapshot/{SOURCE}"), source);
    case.write("snapshot/component_fab/equations/__init__.py", init);
    case.write(&format!("snapshot/{SELECTED}"), test);
}
