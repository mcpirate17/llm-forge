//! Rust fixtures for the legacy mutation campaign reader and receipt contracts.

use crate::comm_support::{bind_signature, py_json, signature};
use crate::support::{module, path, Case};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyCFunction, PyDict, PyList, PyTuple};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

pub fn testing<'py>(py: Python<'py>) -> Bound<'py, PyAny> {
    module(py, "conductor.mutation_testing").into_any()
}

pub fn equal(actual: &Bound<'_, PyAny>, expected: &Bound<'_, PyAny>) {
    assert!(
        actual.eq(expected).unwrap(),
        "actual={actual:?}, expected={expected:?}"
    );
}

pub fn fixture_path(py: Python<'_>) -> PathBuf {
    let root: String = testing(py)
        .getattr("REPO_ROOT")
        .unwrap()
        .str()
        .unwrap()
        .extract()
        .unwrap();
    Path::new(&root).join("src/conductor/testdata/mutation_testing/campaign.json")
}

pub fn fixture_payload(py: Python<'_>) -> Value {
    serde_json::from_slice(&fs::read(fixture_path(py)).unwrap()).unwrap()
}

pub fn fixture_campaign<'py>(py: Python<'py>) -> Bound<'py, PyAny> {
    testing(py)
        .getattr("load_campaign")
        .unwrap()
        .call1((path(py, &fixture_path(py)),))
        .unwrap()
}

fn mutation_rows<'py>(
    py: Python<'py>,
    subject: &Bound<'py, PyAny>,
    root: &Path,
    one: &Bound<'py, PyAny>,
    two: &Bound<'py, PyAny>,
) -> (Bound<'py, PyList>, Bound<'py, PyList>) {
    let planned_class = subject.getattr("PlannedMutation").unwrap();
    let mutation_class = subject.getattr("Mutation").unwrap();
    let planned = PyList::empty(py);
    let mutations = PyList::empty(py);
    for (index, ranked) in [(1, one), (2, two)] {
        let id = format!("mutation_{index}");
        let killers = PyTuple::new(py, [ranked.getattr("nodeid").unwrap()]).unwrap();
        let item = planned_class
            .call1((
                &id,
                "source.py",
                format!("contract {index}"),
                format!("description {index}"),
                &killers,
            ))
            .unwrap();
        planned.append(&item).unwrap();
        let patch = root.join(format!("mutation_{index}.patch"));
        mutations
            .append(
                mutation_class
                    .call1((
                        &id,
                        path(py, &patch),
                        "0".repeat(64),
                        PyTuple::new(py, ["source.py"]).unwrap(),
                        killers,
                    ))
                    .unwrap(),
            )
            .unwrap();
    }
    (planned, mutations)
}

pub fn temporary_campaign<'py>(py: Python<'py>, case: &Case) -> Bound<'py, PyAny> {
    let root = case.root();
    let manifest = case.write("campaign.json", "{}\n");
    let source = case.write("source.py", "VALUE = 1\n");
    let first = case.write("test_one.py", "def test_one():\n    assert True\n");
    let second = case.write("test_two.py", "def test_two():\n    assert True\n");
    let subject = testing(py);
    let sha = subject.getattr("_sha256").unwrap();
    let ranked_class = subject.getattr("RankedTest").unwrap();
    let one = ranked_class
        .call1((1, "test_one.py::test_one", "one", "one"))
        .unwrap();
    let two = ranked_class
        .call1((2, "test_two.py::test_two", "two", "two"))
        .unwrap();
    let (planned, mutations) = mutation_rows(py, &subject, root, &one, &two);
    let pins = PyDict::new(py);
    for (name, file) in [
        ("source.py", &source),
        ("test_one.py", &first),
        ("test_two.py", &second),
    ] {
        pins.set_item(name, sha.call1((path(py, file),)).unwrap())
            .unwrap();
    }
    let scope_class = subject.getattr("TestFileScope").unwrap();
    let scopes = PyDict::new(py);
    for (name, ranked) in [("test_one.py", &one), ("test_two.py", &two)] {
        let nodeid = ranked.getattr("nodeid").unwrap();
        scopes
            .set_item(
                name,
                scope_class
                    .call1((
                        name,
                        "complete",
                        "python_ast",
                        PyTuple::new(py, [nodeid]).unwrap(),
                    ))
                    .unwrap(),
            )
            .unwrap();
    }
    let kw = PyDict::new(py);
    kw.set_item("manifest_path", path(py, &manifest)).unwrap();
    kw.set_item(
        "manifest_sha256",
        sha.call1((path(py, &manifest),)).unwrap(),
    )
    .unwrap();
    kw.set_item("campaign_id", "temporary_campaign").unwrap();
    kw.set_item("title", "Temporary unit-test campaign")
        .unwrap();
    kw.set_item("language", "python").unwrap();
    kw.set_item("mutation_engine", "reviewed_unified_diff")
        .unwrap();
    kw.set_item("expected_mutations", 2).unwrap();
    kw.set_item("source_sha256", pins).unwrap();
    kw.set_item("ranked_tests", PyTuple::new(py, [&one, &two]).unwrap())
        .unwrap();
    kw.set_item(
        "planned_mutations",
        py.import("builtins")
            .unwrap()
            .getattr("tuple")
            .unwrap()
            .call1((planned,))
            .unwrap(),
    )
    .unwrap();
    kw.set_item(
        "mutations",
        py.import("builtins")
            .unwrap()
            .getattr("tuple")
            .unwrap()
            .call1((mutations,))
            .unwrap(),
    )
    .unwrap();
    kw.set_item(
        "test_argv",
        PyTuple::new(py, ["python", "-m", "pytest", "test_one.py", "test_two.py"]).unwrap(),
    )
    .unwrap();
    kw.set_item("timeout_seconds", 10).unwrap();
    kw.set_item("blocked_process_substrings", PyTuple::empty(py))
        .unwrap();
    kw.set_item("poll_seconds", 1).unwrap();
    kw.set_item("environment", PyDict::new(py)).unwrap();
    kw.set_item("host_read_dependencies", PyTuple::empty(py))
        .unwrap();
    kw.set_item("test_scopes", scopes).unwrap();
    subject
        .getattr("Campaign")
        .unwrap()
        .call((), Some(&kw))
        .unwrap()
}

pub fn registry(py: Python<'_>, case: &Case) -> PathBuf {
    let file = case.root().join("registry.json");
    let patterns: Vec<String> = testing(py)
        .getattr("CANONICAL_TEST_PATTERNS")
        .unwrap()
        .extract()
        .unwrap();
    fs::write(&file, json!({"schema_version":1,"enforcement":"changed_tests","test_patterns":patterns,"receipt_directories":["receipts"],"campaigns":[{"manifest":"campaign.json"}]}).to_string()).unwrap();
    file
}

pub fn pass_receipt(py: Python<'_>, case: &Case, campaign: &Bound<'_, PyAny>) -> PathBuf {
    let receipts = case.mkdir("receipts");
    let location = receipts.join("pass.json");
    let subject = testing(py);
    let hashes = subject
        .getattr("_runner_components_sha256")
        .unwrap()
        .call0()
        .unwrap();
    let mutations = campaign.getattr("mutations").unwrap();
    let ids = PyList::empty(py);
    let rows = PyList::empty(py);
    for item in mutations.try_iter().unwrap() {
        let item = item.unwrap();
        ids.append(item.getattr("mutation_id").unwrap()).unwrap();
        let row = PyDict::new(py);
        row.set_item("id", item.getattr("mutation_id").unwrap())
            .unwrap();
        row.set_item("outcome", "KILLED").unwrap();
        row.set_item("patch_sha256", item.getattr("patch_sha256").unwrap())
            .unwrap();
        rows.append(row).unwrap();
    }
    let data = PyDict::new(py);
    data.set_item("schema_version", subject.getattr("RECEIPT_SCHEMA").unwrap())
        .unwrap();
    data.set_item("status", "PASS").unwrap();
    data.set_item("campaign_id", campaign.getattr("campaign_id").unwrap())
        .unwrap();
    data.set_item("manifest", "campaign.json").unwrap();
    data.set_item(
        "manifest_sha256",
        campaign.getattr("manifest_sha256").unwrap(),
    )
    .unwrap();
    data.set_item(
        "runner_sha256",
        hashes.get_item("conductor/mutation_testing.py").unwrap(),
    )
    .unwrap();
    data.set_item("runner_components_sha256", hashes).unwrap();
    data.set_item("source_sha256", campaign.getattr("source_sha256").unwrap())
        .unwrap();
    data.set_item(
        "test_scopes",
        subject
            .getattr("_test_scopes_payload")
            .unwrap()
            .call1((campaign,))
            .unwrap(),
    )
    .unwrap();
    data.set_item("complete_campaign", true).unwrap();
    data.set_item("selected_mutations", ids).unwrap();
    data.set_item("mutants", rows).unwrap();
    data.set_item("mutation_score", 1.0).unwrap();
    let encoded: String = module(py, "json")
        .getattr("dumps")
        .unwrap()
        .call1((data,))
        .unwrap()
        .extract()
        .unwrap();
    fs::write(&location, encoded).unwrap();
    location
}

pub fn constant<'py>(py: Python<'py>, value: &Bound<'py, PyAny>) -> Bound<'py, PyCFunction> {
    let value = value.clone().unbind();
    PyCFunction::new_closure(py, None, None, move |args, _kwargs| {
        Ok::<_, PyErr>(value.clone_ref(args.py()))
    })
    .unwrap()
}

pub fn strict<'py, F>(
    py: Python<'py>,
    positional: &[&str],
    keyword_only: &[&str],
    body: F,
) -> Bound<'py, PyCFunction>
where
    F: Fn(Python<'_>, Bound<'_, PyAny>) -> PyResult<Py<PyAny>> + Send + Sync + 'static,
{
    let sig = signature(py, positional, keyword_only);
    PyCFunction::new_closure(py, None, None, move |args, kwargs| {
        let bound = bind_signature(&sig, args, kwargs)?;
        body(args.py(), bound.getattr("arguments")?)
    })
    .unwrap()
}

pub fn verify<'py>(
    py: Python<'py>,
    case: &Case,
    registry: &Path,
    tests: &[&str],
) -> Bound<'py, PyAny> {
    let kw = PyDict::new(py);
    kw.set_item("repo_root", path(py, case.root())).unwrap();
    testing(py)
        .getattr("verify_evidence")
        .unwrap()
        .call(
            (path(py, registry), PyList::new(py, tests).unwrap()),
            Some(&kw),
        )
        .unwrap()
}

pub fn py_expected<'py>(py: Python<'py>, value: Value) -> Bound<'py, PyAny> {
    py_json(py, value)
}

pub fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}
