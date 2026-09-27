//! Rust-owned fixtures for mutation patch audit contracts.
use super::support::{module, path, AttrPatch, Case};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyCFunction, PyDict, PyList, PyTuple};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

pub fn audit<'py>(py: Python<'py>) -> Bound<'py, PyAny> {
    module(py, "conductor.mutation_patch_audit").into_any()
}

pub fn testing<'py>(py: Python<'py>) -> Bound<'py, PyAny> {
    module(py, "conductor.mutation_testing").into_any()
}

pub fn json_to_py<'py>(py: Python<'py>, value: &Value) -> Bound<'py, PyAny> {
    module(py, "json")
        .getattr("loads")
        .unwrap()
        .call1((value.to_string(),))
        .unwrap()
}

pub fn py_to_json(value: &Bound<'_, PyAny>) -> Value {
    let encoded: String = module(value.py(), "json")
        .getattr("dumps")
        .unwrap()
        .call1((value,))
        .unwrap()
        .extract()
        .unwrap();
    serde_json::from_str(&encoded).unwrap()
}

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
    ] {
        case.remove_env(name);
    }
    case.set_env("GIT_CONFIG_NOSYSTEM", "1");
    case.set_env("GIT_CONFIG_GLOBAL", "/dev/null");
    case.set_env("GIT_CONFIG_SYSTEM", "/dev/null");
    case
}

pub fn git(root: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

pub fn git_repo(root: &Path) {
    fs::create_dir_all(root).unwrap();
    git(root, &["init", "-q"]);
    fs::write(root.join("source.py"), "VALUE = 1\n").unwrap();
    git(root, &["add", "-A"]);
}

pub fn patch(root: &Path, name: &str, old: &str, new: &str) -> PathBuf {
    let file = root.join(format!("{name}.patch"));
    fs::write(&file, format!("diff --git a/source.py b/source.py\n--- a/source.py\n+++ b/source.py\n@@ -1 +1 @@\n-{old}\n+{new}\n")).unwrap();
    file
}

pub fn sha256(py: Python<'_>, file: &Path) -> String {
    testing(py)
        .getattr("_sha256")
        .unwrap()
        .call1((path(py, file),))
        .unwrap()
        .extract()
        .unwrap()
}

pub fn mutation<'py>(
    py: Python<'py>,
    id: &str,
    patch_file: &Path,
    digest: Option<&str>,
) -> Bound<'py, PyAny> {
    testing(py)
        .getattr("Mutation")
        .unwrap()
        .call1((
            id,
            path(py, patch_file),
            digest
                .map(str::to_owned)
                .unwrap_or_else(|| sha256(py, patch_file)),
            ("source.py",),
            PyTuple::empty(py),
        ))
        .unwrap()
}

pub fn campaign<'py>(
    py: Python<'py>,
    root: &Path,
    id: &str,
    mutations: &[Bound<'py, PyAny>],
) -> Bound<'py, PyAny> {
    let manifest = root.join(format!("{id}.json"));
    fs::write(&manifest, "{}\n").unwrap();
    let kwargs = PyDict::new(py);
    kwargs
        .set_item("manifest_path", path(py, &manifest))
        .unwrap();
    kwargs
        .set_item("manifest_sha256", sha256(py, &manifest))
        .unwrap();
    kwargs.set_item("campaign_id", id).unwrap();
    kwargs.set_item("title", id).unwrap();
    kwargs.set_item("language", "python").unwrap();
    kwargs
        .set_item("mutation_engine", "reviewed_unified_diff")
        .unwrap();
    kwargs
        .set_item("expected_mutations", mutations.len())
        .unwrap();
    kwargs.set_item("source_sha256", PyDict::new(py)).unwrap();
    kwargs.set_item("ranked_tests", PyTuple::empty(py)).unwrap();
    kwargs
        .set_item("planned_mutations", PyTuple::empty(py))
        .unwrap();
    kwargs
        .set_item("mutations", PyTuple::new(py, mutations).unwrap())
        .unwrap();
    kwargs
        .set_item("test_argv", ("python", "-m", "pytest"))
        .unwrap();
    kwargs.set_item("timeout_seconds", 10).unwrap();
    kwargs
        .set_item("blocked_process_substrings", PyTuple::empty(py))
        .unwrap();
    kwargs.set_item("poll_seconds", 1).unwrap();
    kwargs.set_item("environment", PyDict::new(py)).unwrap();
    kwargs
        .set_item("host_read_dependencies", PyTuple::empty(py))
        .unwrap();
    kwargs.set_item("test_scopes", PyDict::new(py)).unwrap();
    testing(py)
        .getattr("Campaign")
        .unwrap()
        .call((), Some(&kwargs))
        .unwrap()
}

pub fn replace<'py>(
    py: Python<'py>,
    object: &Bound<'py, PyAny>,
    fields: &Value,
) -> Bound<'py, PyAny> {
    let kwargs = json_to_py(py, fields);
    module(py, "dataclasses")
        .getattr("replace")
        .unwrap()
        .call((object,), Some(kwargs.cast::<PyDict>().unwrap()))
        .unwrap()
}

pub fn tree<'py>(py: Python<'py>, root: &Path) -> Bound<'py, PyAny> {
    audit(py)
        .getattr("_TreeHasher")
        .unwrap()
        .call1((path(py, root),))
        .unwrap()
}

pub fn constant<'py>(py: Python<'py>, value: &Bound<'py, PyAny>) -> Bound<'py, PyAny> {
    let held = value.clone().unbind();
    PyCFunction::new_closure(py, None, None, move |args, _| -> PyResult<Py<PyAny>> {
        Ok(held.clone_ref(args.py()))
    })
    .unwrap()
    .into_any()
}

pub fn patch_constant(py: Python<'_>, name: &str, value: &Bound<'_, PyAny>) -> AttrPatch {
    AttrPatch::replace(&audit(py), name, &constant(py, value))
}

pub fn receipt(root: &Path, name: &str, payload: &Value) {
    let directory = root.join("receipts");
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join(format!("{name}.json")), payload.to_string()).unwrap();
}

pub fn current() -> Value {
    json!({"conductor/mutation_testing.py": "a".repeat(64)})
}

pub fn value_receipt(current: &Value, value: Value, stamp: &str) -> Value {
    json!({
        "status": "PASS",
        "runner_components_sha256": current,
        "generated_at": stamp,
        "test_value": value,
    })
}

pub fn value_verdicts(
    py: Python<'_>,
    root: &Path,
    campaigns: &[Bound<'_, PyAny>],
    receipts: &Value,
    current: &Value,
) -> (Value, Value) {
    let result = audit(py)
        .getattr("_value_verdicts")
        .unwrap()
        .call1((
            PyList::new(py, campaigns).unwrap(),
            json_to_py(py, receipts),
            json_to_py(py, current),
            path(py, root),
            tree(py, root),
        ))
        .unwrap();
    (
        py_to_json(&result.get_item(0).unwrap()),
        py_to_json(&result.get_item(1).unwrap()),
    )
}

pub fn measured<'py>(py: Python<'py>, campaign: &Bound<'py, PyAny>) -> Bound<'py, PyAny> {
    let kwargs = PyDict::new(py);
    kwargs.set_item("adapter", "pytest").unwrap();
    kwargs.set_item("baseline_repetitions", 1).unwrap();
    kwargs.set_item("contracts", PyTuple::empty(py)).unwrap();
    kwargs.set_item("tests", PyTuple::empty(py)).unwrap();
    kwargs
        .set_item("mutation_contracts", PyDict::new(py))
        .unwrap();
    let spec = testing(py)
        .getattr("ValueAnalysisSpec")
        .unwrap()
        .call((), Some(&kwargs))
        .unwrap();
    let fields = PyDict::new(py);
    fields.set_item("value_analysis", spec).unwrap();
    module(py, "dataclasses")
        .getattr("replace")
        .unwrap()
        .call((campaign,), Some(&fields))
        .unwrap()
}

pub fn baseline(root: &Path, fields: &BTreeMap<&str, Vec<&str>>) -> PathBuf {
    let path = root.join("baseline.json");
    fs::write(&path, json!(fields).to_string()).unwrap();
    path
}

pub fn main(py: Python<'_>, args: &[String]) -> (i32, Value) {
    let out = module(py, "io")
        .getattr("StringIO")
        .unwrap()
        .call0()
        .unwrap();
    let sys = module(py, "sys");
    let _patch = AttrPatch::replace(sys.as_any(), "stdout", &out);
    let rc: i32 = audit(py)
        .getattr("main")
        .unwrap()
        .call1((args,))
        .unwrap()
        .extract()
        .unwrap();
    let output: String = out.call_method0("getvalue").unwrap().extract().unwrap();
    (rc, serde_json::from_str(&output).unwrap())
}
