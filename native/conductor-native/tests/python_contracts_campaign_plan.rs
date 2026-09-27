#![cfg(feature = "python-compat-tests")]
//! Public Python planner, write, and refresh contracts backed by Rust fixtures.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyModule};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use support::{assert_error, module, path, text, Case};

fn py_json<'py>(py: Python<'py>, value: &Value) -> Bound<'py, PyAny> {
    py.import("json")
        .unwrap()
        .getattr("loads")
        .unwrap()
        .call1((value.to_string(),))
        .unwrap()
}

fn json_value(value: &Bound<'_, PyAny>) -> Value {
    let encoded: String = value
        .py()
        .import("json")
        .unwrap()
        .getattr("dumps")
        .unwrap()
        .call1((value,))
        .unwrap()
        .extract()
        .unwrap();
    serde_json::from_str(&encoded).unwrap()
}

fn copy_tree(source: &Path, destination: &Path) {
    fs::create_dir_all(destination).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let target = destination.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mutation_plan")
}

fn plan<'py>(
    py: Python<'py>,
    campaign: &Bound<'py, PyModule>,
    root: &Path,
    request: &Value,
) -> PyResult<Bound<'py, PyAny>> {
    let kwargs = PyDict::new(py);
    kwargs.set_item("repo_root", path(py, root)).unwrap();
    for key in [
        "owner",
        "day",
        "jobs",
        "run_timeout_seconds",
        "only_sources",
        "include_covered",
        "extra_tests",
    ] {
        kwargs.set_item(key, py_json(py, &request[key])).unwrap();
    }
    campaign
        .getattr("plan")
        .unwrap()
        .call((request["language"].as_str().unwrap(),), Some(&kwargs))
}

fn request(language: &str, scope: Value) -> Value {
    json!({
        "language": language,
        "owner": "owner",
        "day": "20260910",
        "jobs": 2,
        "run_timeout_seconds": 91,
        "only_sources": scope,
        "include_covered": false,
        "extra_tests": {},
    })
}

fn write_manifest(
    py: Python<'_>,
    campaign: &Bound<'_, PyModule>,
    root: &Path,
    manifest: &Value,
) -> String {
    let kwargs = PyDict::new(py);
    kwargs.set_item("repo_root", path(py, root)).unwrap();
    let written: Vec<String> = campaign
        .getattr("write")
        .unwrap()
        .call((py_json(py, &json!([manifest])),), Some(&kwargs))
        .unwrap()
        .extract()
        .unwrap();
    assert_eq!(written.len(), 1);
    written[0].clone()
}

#[test]
fn public_plan_matches_all_fifteen_frozen_python_cases() {
    let case = Case::new();
    Python::attach(|py| {
        let campaign = module(py, "conductor.mutation_campaign_generate");
        let mut count = 0;
        for entry in fs::read_dir(fixtures()).unwrap() {
            let entry = entry.unwrap();
            if !entry.file_type().unwrap().is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            let fixture = entry.path();
            let request_path = fixture.join("request.json");
            if !request_path.is_file() {
                continue;
            }
            count += 1;
            let root = case.root().join(&name);
            copy_tree(&fixture.join("tree"), &root);
            let request: Value = serde_json::from_slice(&fs::read(request_path).unwrap()).unwrap();
            assert_eq!(
                request["campaigns_root"], "conductor/mutation_campaigns",
                "{name}: frozen fixture assumes the default campaigns root"
            );
            let actual = plan(py, &campaign, &root, &request);
            let error_path = fixture.join("expected_error.txt");
            if error_path.is_file() {
                let error = actual.expect_err("frozen refusal");
                let expected = fs::read_to_string(error_path).unwrap();
                assert_error(
                    py,
                    error.clone_ref(py),
                    &campaign.getattr("CampaignError").unwrap(),
                    expected.trim(),
                );
                assert_eq!(text(error.value(py)), expected.trim(), "{name}");
            } else {
                let expected: Value =
                    serde_json::from_slice(&fs::read(fixture.join("expected.json")).unwrap())
                        .unwrap();
                let value = actual.unwrap_or_else(|error| panic!("{name}: {error}"));
                value.cast::<PyDict>().expect("public plan returns dict");
                assert_eq!(json_value(&value), expected, "{name}");
            }
        }
        assert_eq!(count, 15);
    });
}

#[test]
fn public_write_persists_newline_and_refuses_baseline_replacement() {
    let case = Case::new();
    case.write("pkg/subject.py", "x = 1\n");
    case.write("pkg/test_subject.py", "def test_subject(): pass\n");
    Python::attach(|py| {
        let campaign = module(py, "conductor.mutation_campaign_generate");
        let result = plan(py, &campaign, case.root(), &request("python", Value::Null)).unwrap();
        let manifest = json_value(&result)["manifests"][0].clone();
        let relative = write_manifest(py, &campaign, case.root(), &manifest);
        let saved = fs::read_to_string(case.root().join(&relative)).unwrap();
        assert!(saved.ends_with('\n'));
        assert_eq!(serde_json::from_str::<Value>(&saved).unwrap(), manifest);

        let kwargs = PyDict::new(py);
        kwargs.set_item("repo_root", path(py, case.root())).unwrap();
        let error = campaign
            .getattr("write")
            .unwrap()
            .call((py_json(py, &json!([manifest])),), Some(&kwargs))
            .expect_err("baseline overwrite must be refused");
        assert_error(
            py,
            error,
            &campaign.getattr("CampaignError").unwrap(),
            "refusing to replace",
        );
    });
}

#[test]
fn public_python_refresh_rebinds_pins_and_retains_engine_ratchet() {
    let case = Case::new();
    case.write("pkg/subject.py", "x = 1\n");
    case.write("pkg/test_subject.py", "def test_subject(): pass\n");
    Python::attach(|py| {
        let campaign = module(py, "conductor.mutation_campaign_generate");
        let result = plan(py, &campaign, case.root(), &request("python", Value::Null)).unwrap();
        let mut manifest = json_value(&result)["manifests"][0].clone();
        let relative = write_manifest(py, &campaign, case.root(), &manifest);
        manifest["survivor_baseline"] = json!(["engine-recorded"]);
        manifest["survivor_baseline_recorded"] = json!(true);
        fs::write(case.root().join(&relative), manifest.to_string()).unwrap();
        case.write("pkg/subject.py", "x = 2\n");
        case.write("pkg/test_subject.py", "def test_subject(): assert True\n");

        let kwargs = PyDict::new(py);
        kwargs.set_item("repo_root", path(py, case.root())).unwrap();
        let returned: String = campaign
            .getattr("refresh_python_campaign")
            .unwrap()
            .call((manifest["campaign_id"].as_str().unwrap(),), Some(&kwargs))
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(returned, relative);
        let saved = fs::read_to_string(case.root().join(&relative)).unwrap();
        assert!(saved.ends_with('\n'));
        let refreshed: Value = serde_json::from_str(&saved).unwrap();
        assert_eq!(refreshed["survivor_baseline"], json!(["engine-recorded"]));
        assert_eq!(refreshed["survivor_baseline_recorded"], true);
        assert_ne!(refreshed["source_sha256"], manifest["source_sha256"]);
        assert_ne!(refreshed["test_sha256"], manifest["test_sha256"]);
    });
}

#[test]
fn public_rust_refresh_keeps_exact_source_scope_and_baseline() {
    let case = Case::new();
    case.write("crate/Cargo.toml", "[package]\nname = 'widget'\n");
    case.write("crate/src/lib.rs", "#[cfg(test)]\nmod tests {}\n");
    case.write("crate/src/extra.rs", "pub fn extra() {}\n");
    Python::attach(|py| {
        let campaign = module(py, "conductor.mutation_campaign_generate");
        let request = request("rust", json!(["crate/src/lib.rs"]));
        let result = plan(py, &campaign, case.root(), &request).unwrap();
        let mut manifest = json_value(&result)["manifests"][0].clone();
        let relative = write_manifest(py, &campaign, case.root(), &manifest);
        manifest["survivor_baseline"] = json!(["rust-engine-recorded"]);
        fs::write(case.root().join(&relative), manifest.to_string()).unwrap();
        case.write("crate/src/extra.rs", "pub fn extra() { assert!(true); }\n");

        let kwargs = PyDict::new(py);
        kwargs.set_item("repo_root", path(py, case.root())).unwrap();
        kwargs
            .set_item("sources", vec!["crate/src/extra.rs"])
            .unwrap();
        let returned: String = campaign
            .getattr("refresh_rust_campaign")
            .unwrap()
            .call((manifest["campaign_id"].as_str().unwrap(),), Some(&kwargs))
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(returned, relative);
        let saved = fs::read_to_string(case.root().join(&relative)).unwrap();
        assert!(saved.ends_with('\n'));
        let refreshed: Value = serde_json::from_str(&saved).unwrap();
        assert_eq!(refreshed["generator"]["source"], json!(["src/extra.rs"]));
        assert_eq!(refreshed["source_sha256"], refreshed["test_sha256"]);
        assert_eq!(
            refreshed["survivor_baseline"],
            json!(["rust-engine-recorded"])
        );
        assert_ne!(refreshed["source_sha256"], manifest["source_sha256"]);
    });
}
