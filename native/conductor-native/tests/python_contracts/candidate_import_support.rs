//! Rust-owned fixtures for the import-declaration compatibility contracts.

use crate::support::{module, path, AttrPatch, Case};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyCFunction, PyDict, PyModule, PyTuple};
use serde_json::{json, Value};
use std::fs;

pub const MANIFEST: &str = "[project]\nname = \"probe\"\ndependencies = [\"polars>=1.0\"]\n\n[project.optional-dependencies]\nresearch = [\"scipy>=1.12\"]\n\n[dependency-groups]\ndev = [\"pytest>=8.0\"]\n\n[build-system]\nrequires = [\"hatchling\"]\n";

pub fn import_decl(py: Python<'_>) -> Bound<'_, PyModule> {
    module(py, "conductor.candidate_review.import_declaration")
}

pub fn checks(py: Python<'_>) -> Bound<'_, PyModule> {
    module(py, "conductor.candidate_review.checks")
}

pub fn model(py: Python<'_>) -> Bound<'_, PyModule> {
    module(py, "conductor.candidate_review.model")
}

pub fn py_json<'py>(py: Python<'py>, value: Value) -> Bound<'py, PyAny> {
    module(py, "json")
        .getattr("loads")
        .unwrap()
        .call1((value.to_string(),))
        .unwrap()
}

pub fn equal(actual: &Bound<'_, PyAny>, expected: &Bound<'_, PyAny>) {
    assert!(
        actual.eq(expected).unwrap(),
        "actual {:?}, expected {:?}",
        actual,
        expected
    );
}

pub fn py_tuple<'py>(py: Python<'py>, values: &[&str]) -> Bound<'py, PyAny> {
    PyTuple::new(py, values).unwrap().into_any()
}

pub fn py_frozenset<'py>(py: Python<'py>, values: &[&str]) -> Bound<'py, PyAny> {
    let values = PyTuple::new(py, values).unwrap();
    py.import("builtins")
        .unwrap()
        .getattr("frozenset")
        .unwrap()
        .call1((values,))
        .unwrap()
}

pub struct FileChange<'a> {
    pub path: &'a str,
    pub classes: &'a [&'a str],
    pub deleted: bool,
}

impl<'a> FileChange<'a> {
    pub fn source(path: &'a str) -> Self {
        Self {
            path,
            classes: &["python", "source"],
            deleted: false,
        }
    }

    pub fn test(path: &'a str) -> Self {
        Self {
            path,
            classes: &["python", "test"],
            deleted: false,
        }
    }

    pub fn deleted(path: &'a str) -> Self {
        Self {
            path,
            classes: &["python", "source"],
            deleted: true,
        }
    }
}

pub fn change<'py>(py: Python<'py>, input: &FileChange<'_>) -> Bound<'py, PyAny> {
    let kwargs = PyDict::new(py);
    kwargs
        .set_item("status", if input.deleted { "D" } else { "M" })
        .unwrap();
    kwargs.set_item("path", input.path).unwrap();
    kwargs.set_item("old_path", py.None()).unwrap();
    kwargs.set_item("old_mode", "100644").unwrap();
    kwargs
        .set_item("new_mode", if input.deleted { "000000" } else { "100644" })
        .unwrap();
    kwargs.set_item("old_oid", "1".repeat(40)).unwrap();
    kwargs.set_item("new_oid", "2".repeat(40)).unwrap();
    kwargs
        .set_item("classes", PyTuple::new(py, input.classes).unwrap())
        .unwrap();
    model(py)
        .getattr("Change")
        .unwrap()
        .call((), Some(&kwargs))
        .unwrap()
}

/// Build the fields that `check_import_declaration` reads from `_gate_context`.
/// The original fixture's Git anchor and grandfather inventory are unrelated to
/// this check; its ReviewContext is otherwise reproduced with the same values.
pub fn review_context<'py>(
    py: Python<'py>,
    case: &Case,
    files: &[(&str, &str)],
    changes: &[FileChange<'_>],
    manifest: &str,
) -> Bound<'py, PyAny> {
    let snapshot = case.root().join("snapshot");
    let registry = snapshot.join("conductor/mutation_campaigns/registry.json");
    fs::create_dir_all(registry.parent().unwrap()).unwrap();
    fs::write(registry, "{}").unwrap();
    let probe = snapshot.join("research/tests/test_probe.py");
    fs::create_dir_all(probe.parent().unwrap()).unwrap();
    fs::write(probe, "def test_probe_new():\n    assert True\n").unwrap();
    fs::write(snapshot.join("pyproject.toml"), manifest).unwrap();
    for (relative, content) in files {
        let file = snapshot.join(relative);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, content).unwrap();
    }

    let candidate_kwargs = PyDict::new(py);
    candidate_kwargs.set_item("kind", "index").unwrap();
    candidate_kwargs
        .set_item("tree_oid", "a".repeat(40))
        .unwrap();
    candidate_kwargs
        .set_item("base_tree_oid", "b".repeat(40))
        .unwrap();
    candidate_kwargs
        .set_item("base_commit_oid", "c".repeat(40))
        .unwrap();
    candidate_kwargs.set_item("commit_oid", py.None()).unwrap();
    candidate_kwargs.set_item("target_ref", "HEAD").unwrap();
    let rows: Vec<_> = changes.iter().map(|item| change(py, item)).collect();
    candidate_kwargs
        .set_item("changes", PyTuple::new(py, rows).unwrap())
        .unwrap();
    let candidate = model(py)
        .getattr("Candidate")
        .unwrap()
        .call((), Some(&candidate_kwargs))
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
    let context_kwargs = PyDict::new(py);
    context_kwargs
        .set_item("repo", path(py, case.root()))
        .unwrap();
    context_kwargs
        .set_item("snapshot", path(py, &snapshot))
        .unwrap();
    context_kwargs.set_item("candidate", candidate).unwrap();
    context_kwargs
        .set_item("entries", PyTuple::empty(py))
        .unwrap();
    context_kwargs.set_item("policy", policy).unwrap();
    context_kwargs.set_item("surface", "manual").unwrap();
    context_kwargs.set_item("profile", "fast").unwrap();
    context_kwargs.set_item("owner", py.None()).unwrap();
    context_kwargs
        .set_item("runtime_dir", path(py, &case.root().join("runtime")))
        .unwrap();
    checks(py)
        .getattr("ReviewContext")
        .unwrap()
        .call((), Some(&context_kwargs))
        .unwrap()
}

/// Match the original no-argument metadata provider monkeypatch exactly.
pub fn distributions_patch(py: Python<'_>, distributions: Option<Value>) -> AttrPatch {
    let value = distributions.unwrap_or_else(|| json!({"yaml":["PyYAML"]}));
    let callback =
        PyCFunction::new_closure(py, None, None, move |args, kwargs| -> PyResult<Py<PyAny>> {
            if !args.is_empty() || kwargs.is_some_and(|items| !items.is_empty()) {
                return Err(pyo3::exceptions::PyTypeError::new_err(
                    "packages_distributions takes no arguments",
                ));
            }
            Ok(py_json(args.py(), value.clone()).unbind())
        })
        .unwrap();
    AttrPatch::replace(
        import_decl(py).as_any(),
        "packages_distributions",
        callback.as_any(),
    )
}

/// Restore the test-tool exemption mapping after a scoped review contract.
pub struct ExemptionPatch {
    table: Py<PyAny>,
    key: String,
    previous: Option<Py<PyAny>>,
}

impl ExemptionPatch {
    pub fn add(py: Python<'_>, key: &str, module_name: &str) -> Self {
        let table = import_decl(py)
            .getattr("RUNTIME_TEST_TOOL_EXEMPTIONS")
            .unwrap();
        let previous = table.get_item(key).ok().map(Bound::unbind);
        let singleton = PyTuple::new(py, [module_name]).unwrap();
        let grant = py
            .import("builtins")
            .unwrap()
            .getattr("frozenset")
            .unwrap()
            .call1((singleton,))
            .unwrap();
        table.set_item(key, grant).unwrap();
        Self {
            table: table.unbind(),
            key: key.to_owned(),
            previous,
        }
    }
}

impl Drop for ExemptionPatch {
    fn drop(&mut self) {
        Python::attach(|py| {
            if let Some(value) = &self.previous {
                self.table
                    .bind(py)
                    .set_item(self.key.as_str(), value.bind(py))
                    .unwrap();
            } else {
                self.table.bind(py).del_item(self.key.as_str()).unwrap();
            }
        });
    }
}
