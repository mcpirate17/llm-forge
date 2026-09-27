#![cfg(feature = "python-compat-tests")]
//! Host dependency, registry document, and path contracts.

#[path = "python_contracts/agent_comm_support.rs"]
#[allow(dead_code)]
mod comm_support;
#[path = "python_contracts/mutation_testing_fixture.rs"]
#[allow(dead_code)]
mod fixture;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use comm_support::{capture, json_value, py_json};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyList, PyTuple};
use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};
use support::{assert_error, module, path, text, Case};

fn subject(py: Python<'_>) -> Bound<'_, pyo3::types::PyModule> {
    module(py, "conductor.mutation_testing_support")
}

fn host_campaign<'py>(py: Python<'py>, host: &Path) -> Bound<'py, PyAny> {
    let screen = host.join("reports/screen");
    fs::create_dir_all(&screen).unwrap();
    fs::write(screen.join("receipt.json"), "{}").unwrap();
    fs::write(screen.join("ignored.json"), "[]").unwrap();
    let kw = PyDict::new(py);
    kw.set_item(
        "host_read_dependencies",
        PyTuple::new(py, ["reports/screen"]).unwrap(),
    )
    .unwrap();
    module(py, "dataclasses")
        .getattr("replace")
        .unwrap()
        .call((fixture::fixture_campaign(py),), Some(&kw))
        .unwrap()
}

fn link<'py>(
    py: Python<'py>,
    campaign: &Bound<'py, PyAny>,
    snapshot: &Path,
    host: &Path,
) -> PyResult<Bound<'py, PyAny>> {
    fixture::testing(py)
        .getattr("_link_host_dependencies")
        .unwrap()
        .call1((campaign, path(py, snapshot), path(py, host)))
}

fn campaign_error_end(py: Python<'_>, error: PyErr, suffix: &str) {
    assert!(error
        .matches(py, &fixture::testing(py).getattr("CampaignError").unwrap())
        .unwrap());
    assert!(error.to_string().ends_with(suffix), "{error}");
}

fn dependency_case<'py>(
    case: &Case,
    py: Python<'py>,
    kind: &str,
) -> (Bound<'py, PyAny>, PathBuf, PathBuf) {
    let host = case.root().join(kind).join("host");
    let campaign = host_campaign(py, &host);
    let snapshot = case.root().join(kind).join("snapshot");
    fs::create_dir_all(snapshot.join("reports")).unwrap();
    (campaign, snapshot, host)
}

#[test]
fn host_read_dependency_merge_fills_in_what_is_missing_and_refuses_the_rest() {
    let case = Case::new();
    Python::attach(|py| {
        let (campaign, snapshot, host) = dependency_case(&case, py, "filled");
        let screen = snapshot.join("reports/screen");
        fs::create_dir_all(&screen).unwrap();
        fs::write(screen.join("receipt.json"), "{}").unwrap();
        fs::write(screen.join("tracked_only.json"), "0").unwrap();
        link(py, &campaign, &snapshot, &host).unwrap();
        assert_eq!(
            fs::read_to_string(screen.join("ignored.json")).unwrap(),
            "[]"
        );
        assert_eq!(
            fs::read_to_string(screen.join("receipt.json")).unwrap(),
            "{}"
        );
        assert_eq!(
            fs::read_to_string(screen.join("tracked_only.json")).unwrap(),
            "0"
        );

        let (campaign, snapshot, host) = dependency_case(&case, py, "diverged");
        let screen = snapshot.join("reports/screen");
        fs::create_dir_all(&screen).unwrap();
        fs::write(screen.join("receipt.json"), "{\"uncommitted\": true}").unwrap();
        campaign_error_end(
            py,
            link(py, &campaign, &snapshot, &host).unwrap_err(),
            "differs from the host: reports/screen/receipt.json",
        );

        let (campaign, snapshot, host) = dependency_case(&case, py, "kind");
        fs::write(snapshot.join("reports/screen"), "not a directory").unwrap();
        campaign_error_end(
            py,
            link(py, &campaign, &snapshot, &host).unwrap_err(),
            "differs from the host: reports/screen",
        );

        let (campaign, snapshot, host) = dependency_case(&case, py, "symlink");
        std::os::unix::fs::symlink(host.join("reports/screen"), snapshot.join("reports/screen"))
            .unwrap();
        campaign_error_end(
            py,
            link(py, &campaign, &snapshot, &host).unwrap_err(),
            "already contains host read dependency path: reports/screen",
        );
    });
}

#[test]
fn a_published_json_document_ends_in_a_newline() {
    let case = Case::new();
    Python::attach(|py| {
        let file = case.root().join("iterations/receipt.json");
        subject(py)
            .getattr("atomic_json")
            .unwrap()
            .call1((path(py, &file), py_json(py, json!({"b":1,"a":2}))))
            .unwrap();
        let value = fs::read_to_string(&file).unwrap();
        assert!(value.ends_with('\n'));
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&value).unwrap(),
            json!({"a":2,"b":1})
        );
        assert!(!fs::read_dir(file.parent().unwrap()).unwrap().any(|entry| {
            let name = entry.unwrap().file_name().to_string_lossy().into_owned();
            name.starts_with('.') && name.ends_with(".tmp")
        }));
    });
}

#[test]
fn live_pgids_path_sits_in_the_iterations_dir_beside_receipts() {
    let case = Case::new();
    Python::attach(|py| {
        let root = path(py, case.root());
        let relative = module(py, "conductor.project_paths")
            .getattr("receipts_relative")
            .unwrap()
            .call1((&root,))
            .unwrap();
        let expected = case
            .root()
            .join(text(&relative))
            .join(".iterations")
            .join(text(&subject(py).getattr("LIVE_PGIDS_FILENAME").unwrap()));
        let actual = subject(py)
            .getattr("live_pgids_path")
            .unwrap()
            .call1((root,))
            .unwrap();
        assert!(actual.eq(path(py, &expected)).unwrap());
    });
}

#[test]
fn reap_reports_a_missing_registry_and_touches_nothing() {
    let case = Case::new();
    Python::attach(|py| {
        let absent = case.root().join("absent.json");
        let kw = PyDict::new(py);
        kw.set_item("apply", false).unwrap();
        let answer = subject(py)
            .getattr("reap_orphaned_runs")
            .unwrap()
            .call((path(py, &absent),), Some(&kw))
            .unwrap();
        assert_eq!(answer.get_item(1).unwrap().extract::<i32>().unwrap(), 0);
        let lines = answer.get_item(0).unwrap().cast_into::<PyList>().unwrap();
        assert!(lines
            .eq(PyList::new(
                py,
                [format!("no live pgid registry at {}", absent.display())]
            )
            .unwrap())
            .unwrap());
        assert!(!absent.exists());
    });
}

#[test]
fn a_malformed_registry_refuses_rather_than_guessing() {
    let case = Case::new();
    let registry = case.write("live_pgids.json", "{\"pgid\": 1}");
    Python::attach(|py| {
        let kw = PyDict::new(py);
        kw.set_item("apply", false).unwrap();
        assert_error(
            py,
            subject(py)
                .getattr("reap_orphaned_runs")
                .unwrap()
                .call((path(py, &registry),), Some(&kw))
                .unwrap_err(),
            &module(py, "builtins").getattr("ValueError").unwrap(),
            "not a JSON object with a list",
        );
    });
}

fn record(py: Python<'_>, registry: &Path, pgid: i32) {
    let kw = PyDict::new(py);
    kw.set_item("pgid", pgid).unwrap();
    kw.set_item("engine_pid", std::process::id()).unwrap();
    kw.set_item("argv0", "sleep").unwrap();
    subject(py)
        .getattr("record_live_pgid")
        .unwrap()
        .call((path(py, registry),), Some(&kw))
        .unwrap();
}

#[test]
fn forgetting_the_last_live_pgid_removes_the_registry_and_its_dir() {
    let case = Case::new();
    let dir = case.root().join(".iterations");
    let registry = dir.join("live_pgids.json");
    Python::attach(|py| {
        record(py, &registry, 4242);
        assert!(registry.exists());
        subject(py)
            .getattr("forget_live_pgid")
            .unwrap()
            .call1((path(py, &registry), 4242))
            .unwrap();
        assert!(!registry.exists());
        assert!(!dir.exists());
    });
}

#[test]
fn forgetting_one_of_several_live_pgids_keeps_the_registry() {
    let case = Case::new();
    let registry = case.root().join("live_pgids.json");
    Python::attach(|py| {
        record(py, &registry, 1);
        record(py, &registry, 2);
        subject(py)
            .getattr("forget_live_pgid")
            .unwrap()
            .call1((path(py, &registry), 1))
            .unwrap();
        assert!(registry.exists());
        let remaining = subject(py)
            .getattr("_live_pgid_entries")
            .unwrap()
            .call1((path(py, &registry),))
            .unwrap();
        assert_eq!(json_value(&remaining)[0]["pgid"], 2);
        assert_eq!(remaining.len().unwrap(), 1);
    });
}

#[test]
fn reap_on_an_empty_registry_records_no_runs() {
    let case = Case::new();
    let registry = case.write("live_pgids.json", "{\"live\": []}\n");
    Python::attach(|py| {
        let (buffer, _guard) = capture(py, "stdout");
        let code: i32 = subject(py)
            .getattr("main")
            .unwrap()
            .call1((vec!["reap", "--registry", registry.to_str().unwrap()],))
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(code, 0);
        assert!(text(&buffer.call_method0("getvalue").unwrap()).contains("records no runs"));
    });
}
