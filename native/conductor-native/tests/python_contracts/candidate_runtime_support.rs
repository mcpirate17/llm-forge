//! Fixtures for candidate-review runtime, diff, research, and attestation tests.
use super::candidate_review_support as fixture;
use super::support::{module, path, AttrPatch, Case};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyCFunction, PyDict, PyTuple};
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

pub fn repo(case: &Case) -> PathBuf {
    let root = case.root().join("repo");
    fixture::init_repo(&root);
    root
}

pub fn write(root: &Path, relative: &str, body: &str) {
    fixture::write_fixture(root, relative, body);
}

pub fn staged_probe(case: &Case) -> PathBuf {
    let root = repo(case);
    write(&root, "probe.py", "def value():\n    return 2\n");
    fixture::commit_all(&root, "baseline");
    write(&root, "probe.py", "def value():\n    return 3\n");
    fixture::git(&root, &["add", "probe.py"]);
    root
}

pub fn policy<'py>(py: Python<'py>) -> Bound<'py, PyAny> {
    let file = module(py, "conductor.candidate_review.policy_path")
        .getattr("resolve_policy_path")
        .unwrap()
        .call0()
        .unwrap();
    module(py, "conductor.candidate_review.policy")
        .getattr("load_policy")
        .unwrap()
        .call1((file,))
        .unwrap()
}

pub fn staged_candidate<'py>(py: Python<'py>, root: &Path) -> Bound<'py, PyAny> {
    let candidate = fixture::resolve_candidate(py, root, "index", None, None);
    fixture::classify_candidate(py, &candidate, &policy(py))
}

pub fn with_context<'py, R>(
    py: Python<'py>,
    root: &Path,
    candidate: &Bound<'py, PyAny>,
    surface: &str,
    profile: &str,
    runtime: &Path,
    body: impl FnOnce(Bound<'py, PyAny>) -> R,
) -> R {
    let tree: String = candidate.getattr("tree_oid").unwrap().extract().unwrap();
    fixture::with_materialized(py, root, &tree, None, |snapshot, entries| {
        let context = fixture::review_context(
            py,
            root,
            snapshot,
            candidate,
            entries,
            &policy(py),
            surface,
            profile,
            None,
            runtime,
        );
        body(context)
    })
}

pub fn json_value(value: &Bound<'_, PyAny>) -> Value {
    let py = value.py();
    let encoded: String = module(py, "json")
        .getattr("dumps")
        .unwrap()
        .call1((value,))
        .unwrap()
        .extract()
        .unwrap();
    serde_json::from_str(&encoded).unwrap()
}

pub fn constant<'py>(py: Python<'py>, value: &Bound<'py, PyAny>) -> Bound<'py, PyAny> {
    let held = value.clone().unbind();
    PyCFunction::new_closure(py, None, None, move |args, _| -> PyResult<Py<PyAny>> {
        Ok(held.clone_ref(args.py()))
    })
    .unwrap()
    .into_any()
}

pub fn raise<'py>(py: Python<'py>, error: &Bound<'py, PyAny>) -> Bound<'py, PyAny> {
    let held = error.clone().unbind();
    PyCFunction::new_closure(py, None, None, move |args, _| -> PyResult<Py<PyAny>> {
        Err(PyErr::from_value(held.bind(args.py()).clone()))
    })
    .unwrap()
    .into_any()
}

pub fn replace<'py>(
    py: Python<'py>,
    object: &Bound<'py, PyAny>,
    fields: &[(&str, &Bound<'py, PyAny>)],
) -> Bound<'py, PyAny> {
    let kwargs = PyDict::new(py);
    for (key, value) in fields {
        kwargs.set_item(key, value).unwrap();
    }
    module(py, "dataclasses")
        .getattr("replace")
        .unwrap()
        .call((object,), Some(&kwargs))
        .unwrap()
}

pub fn numbered(prefix: &str, count: usize) -> String {
    (1..=count)
        .map(|n| format!("{prefix}_{n} = {n}\n"))
        .collect()
}

pub fn dated_commit(root: &Path, message: &str, when: &str) -> String {
    fixture::git(root, &["add", "--all"]);
    let output = Command::new("git")
        .args(["commit", "--quiet", "--message", message])
        .current_dir(root)
        .env("GIT_AUTHOR_DATE", when)
        .env("GIT_COMMITTER_DATE", when)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "dated commit: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    fixture::git(root, &["rev-parse", "HEAD"])
}

pub fn attestation(py: Python<'_>, root: &Path, base: &str, tip: &str) -> (Value, Vec<Value>) {
    let candidate = fixture::resolve_candidate(py, root, "range", Some(base), Some(tip));
    let candidate = fixture::classify_candidate(py, &candidate, &policy(py));
    with_context(
        py,
        root,
        &candidate,
        "ci",
        "fast",
        &root.join("runtime"),
        |ctx| {
            let result = module(py, "conductor.candidate_review.engine")
                .getattr("_bypass_evidence")
                .unwrap()
                .call1((ctx,))
                .unwrap();
            let evidence = json_value(&result.get_item(0).unwrap());
            let findings = result
                .get_item(1)
                .unwrap()
                .getattr("findings")
                .unwrap()
                .try_iter()
                .unwrap()
                .map(|row| {
                    let row = row.unwrap();
                    let severity: String = row
                        .getattr("severity")
                        .unwrap()
                        .getattr("name")
                        .unwrap()
                        .extract()
                        .unwrap();
                    serde_json::json!({
                        "rule_id": row.getattr("rule_id").unwrap().extract::<String>().unwrap(),
                        "severity": severity,
                        "evidence": json_value(&row.getattr("evidence").unwrap()),
                    })
                })
                .collect();
            (evidence, findings)
        },
    )
}

pub fn research_rules(py: Python<'_>, root: &Path) -> (Vec<String>, Value) {
    let candidate = staged_candidate(py, root);
    with_context(
        py,
        root,
        &candidate,
        "ci",
        "full",
        &root.join("runtime"),
        |ctx| {
            let selection = fixture::test_selection(py, &[]);
            let result = module(py, "conductor.candidate_review.checks")
                .getattr("check_research_evidence")
                .unwrap()
                .call1((ctx, selection))
                .unwrap();
            let rules = result
                .getattr("findings")
                .unwrap()
                .try_iter()
                .unwrap()
                .map(|row| row.unwrap().getattr("rule_id").unwrap().extract().unwrap())
                .collect();
            (rules, json_value(&result.getattr("metrics").unwrap()))
        },
    )
}

pub fn set_module_attr<'py>(
    py: Python<'py>,
    module_name: &str,
    name: &str,
    value: &Bound<'py, PyAny>,
) -> AttrPatch {
    AttrPatch::replace(module(py, module_name).as_any(), name, value)
}

pub fn tuple<'py>(py: Python<'py>, items: &[&str]) -> Bound<'py, PyTuple> {
    PyTuple::new(py, items).unwrap()
}

pub fn write_raw(root: &Path, relative: &str, body: &str) {
    let target = root.join(relative);
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    fs::write(target, body).unwrap();
}

pub fn path_object<'py>(py: Python<'py>, root: &Path) -> Bound<'py, PyAny> {
    path(py, root)
}
