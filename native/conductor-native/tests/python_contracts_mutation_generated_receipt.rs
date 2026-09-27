#![cfg(feature = "python-compat-tests")]
//! Receipt location, baseline refusal, and slim disk encoding contracts.

#[path = "python_contracts/agent_comm_support.rs"]
#[allow(dead_code)]
mod comm_support;
#[path = "python_contracts/mutation_generated_fixture.rs"]
#[allow(dead_code)]
mod fixture;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use comm_support::py_json;
use fixture::{campaign_error, generated, load, manifest, simple_campaign};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyList};
use serde_json::{json, Value};
use std::fs;
use std::path::Path;
use support::{module, path, text, Case};

fn now(py: Python<'_>) -> String {
    let datetime = module(py, "datetime");
    let value = datetime
        .getattr("datetime")
        .unwrap()
        .call_method1("now", (datetime.getattr("UTC").unwrap(),))
        .unwrap();
    text(&value.call_method0("isoformat").unwrap())
}

fn resolve<'py>(
    py: Python<'py>,
    campaign_id: &str,
    explicit: Option<&Path>,
    root: &Path,
    stamp: &str,
) -> PyResult<Bound<'py, PyAny>> {
    let requested = explicit
        .map(|file| path(py, file))
        .unwrap_or_else(|| py.None().into_bound(py));
    let kw = PyDict::new(py);
    kw.set_item("generated_at", stamp).unwrap();
    generated(py).getattr("resolve_receipt_path")?.call(
        (simple_campaign(py, campaign_id), requested, path(py, root)),
        Some(&kw),
    )
}

#[test]
fn resolve_receipt_path_defaults_to_the_configured_receipt_root() {
    let case = Case::new();
    case.write(
        "pyproject.toml",
        "[tool.conductor]\nmutation_receipt_root = \"campaigns/receipts\"\n",
    );
    Python::attach(|py| {
        let result = resolve(py, "gen_campaign", None, case.root(), &now(py)).unwrap();
        let output = result.get_item(0).unwrap();
        let relative: String = result.get_item(1).unwrap().extract().unwrap();
        let expected = case.root().join("campaigns/receipts");
        assert!(output
            .getattr("parent")
            .unwrap()
            .eq(path(py, &expected))
            .unwrap());
        assert!(expected.is_dir());
        assert!(relative.starts_with("campaigns/receipts/gen_campaign_"));
    });
}

#[test]
fn resolve_receipt_path_falls_back_to_the_monorepo_literal_unconfigured() {
    let case = Case::new();
    Python::attach(|py| {
        let result = resolve(py, "generated_fixture", None, case.root(), &now(py)).unwrap();
        let output = result.get_item(0).unwrap();
        let relative: String = result.get_item(1).unwrap().extract().unwrap();
        assert!(output
            .getattr("parent")
            .unwrap()
            .eq(path(
                py,
                &case.root().join("research/reports/mutation_testing")
            ))
            .unwrap());
        assert!(relative.starts_with("research/reports/mutation_testing/generated_fixture_"));
    });
}

#[test]
fn resolve_receipt_path_fails_loud_when_the_directory_cannot_be_created() {
    let case = Case::new();
    case.write(
        "pyproject.toml",
        "[tool.conductor]\nmutation_receipt_root = \"blocked\"\n",
    );
    case.write("blocked", "not a directory\n");
    Python::attach(|py| {
        campaign_error(
            py,
            resolve(py, "generated_fixture", None, case.root(), &now(py)),
            "cannot create mutation receipt directory",
        );
    });
}

#[test]
fn resolve_receipt_path_still_honours_an_explicit_path() {
    let case = Case::new();
    Python::attach(|py| {
        let explicit = case.root().join("somewhere/receipt.json");
        let result = resolve(
            py,
            "generated_fixture",
            Some(&explicit),
            case.root(),
            &now(py),
        )
        .unwrap();
        let output = result.get_item(0).unwrap();
        assert!(output
            .eq(path(py, &explicit).call_method0("resolve").unwrap())
            .unwrap());
        assert!(result
            .get_item(1)
            .unwrap()
            .eq("somewhere/receipt.json")
            .unwrap());
    });
}

#[test]
fn resolve_receipt_path_filename_stamp_is_the_receipts_own_generated_at() {
    let case = Case::new();
    Python::attach(|py| {
        let result = resolve(
            py,
            "stamped",
            None,
            case.root(),
            "2026-09-13T21:17:08.126160+00:00",
        )
        .unwrap();
        let output = result.get_item(0).unwrap();
        assert!(output
            .getattr("name")
            .unwrap()
            .eq("stamped_20260913T211708Z.json")
            .unwrap());
        let relative: String = result.get_item(1).unwrap().extract().unwrap();
        assert!(relative.ends_with("stamped_20260913T211708Z.json"));
    });
}

fn failed_result<'py>(py: Python<'py>, stderr: &str) -> Bound<'py, PyAny> {
    let kw = PyDict::new(py);
    kw.set_item("returncode", 1).unwrap();
    kw.set_item("timed_out", false).unwrap();
    kw.set_item("duration_seconds", 1.0).unwrap();
    kw.set_item("stdout_tail", "").unwrap();
    kw.set_item("stderr_tail", stderr).unwrap();
    module(py, "conductor.mutation_campaign_model")
        .getattr("CommandResult")
        .unwrap()
        .call((), Some(&kw))
        .unwrap()
}

#[test]
fn a_baseline_that_died_for_want_of_python_says_so() {
    let case = Case::new();
    let file = manifest(&case, json!({}));
    let output = case.root().join("receipt.json");
    Python::attach(|py| {
        let campaign = load(py, &file);
        let receipt = PyDict::new(py);
        let note = generated(py).getattr("note_baseline").unwrap();
        for tail in [
            "python3: not found",
            "ModuleNotFoundError: No module named 'conductor'",
            "ImportError: No module named 'x'",
            "python3: not found: ModuleNotFoundError: No module named 'x'",
        ] {
            campaign_error(
                py,
                note.call1((
                    &campaign,
                    &receipt,
                    failed_result(py, tail),
                    vec!["cargo", "test"],
                    path(py, &output),
                )),
                "could not drive Python",
            );
            assert!(receipt
                .get_item("status")
                .unwrap()
                .unwrap()
                .eq("BASELINE_FAILED")
                .unwrap());
        }
        let error = note
            .call1((
                campaign,
                &receipt,
                failed_result(py, "test result: FAILED. 3 passed; 2 failed"),
                vec!["cargo", "test"],
                path(py, &output),
            ))
            .unwrap_err();
        let class = module(py, "conductor.mutation_scope")
            .getattr("CampaignError")
            .unwrap();
        assert!(error.matches(py, &class).unwrap());
        assert!(text(error.value(py)).starts_with("unmutated baseline failed"));
    });
}

#[test]
fn the_disk_copy_is_slim_while_the_returned_receipt_stays_full() {
    let case = Case::new();
    let output = case.root().join("receipt.json");
    Python::attach(|py| {
        let mutants: Vec<Value> = (0..80)
            .map(|i| json!({"id":format!("m{i}"),"outcome":"KILLED"}))
            .collect();
        let receipt = py_json(
            py,
            json!({
                "campaign_id":"gen_slim","status":"RATCHET_HELD",
                "generated_at":"2026-09-13T00:00:00+00:00","mutants":mutants
            }),
        )
        .cast_into::<PyDict>()
        .unwrap();
        let runner = generated(py);
        runner
            .getattr("write_receipt")
            .unwrap()
            .call1((path(py, &output), &receipt))
            .unwrap();
        let written = fs::read_to_string(&output).unwrap();
        let disk: Value = serde_json::from_str(&written).unwrap();
        assert!(disk.get("mutants").is_none());
        assert_eq!(disk["detail"]["encoding"], "zstd+base64");
        assert_eq!(disk["campaign_id"], "gen_slim");
        let disk_py = py_json(py, disk.clone());
        let expanded = module(py, "conductor.mutation_receipt_slim")
            .getattr("expand_receipt")
            .unwrap()
            .call1((&disk_py,))
            .unwrap();
        assert!(expanded.eq(&receipt).unwrap());
        let kw = PyDict::new(py);
        kw.set_item("indent", 2).unwrap();
        kw.set_item("sort_keys", true).unwrap();
        let encoded = module(py, "json")
            .getattr("dumps")
            .unwrap()
            .call((&disk_py,), Some(&kw))
            .unwrap();
        assert_eq!(written, format!("{}\n", text(&encoded)));
        let small = receipt.copy().unwrap();
        let first = receipt
            .get_item("mutants")
            .unwrap()
            .unwrap()
            .cast_into::<PyList>()
            .unwrap()
            .get_slice(0, 5);
        small.set_item("mutants", first).unwrap();
        runner
            .getattr("write_receipt")
            .unwrap()
            .call1((path(py, &output), &small))
            .unwrap();
        let disk: Value = serde_json::from_slice(&fs::read(&output).unwrap()).unwrap();
        assert_eq!(disk["detail"]["encoding"], "json");
        let expanded = module(py, "conductor.mutation_receipt_slim")
            .getattr("expand_receipt")
            .unwrap()
            .call1((py_json(py, disk),))
            .unwrap();
        assert!(expanded.eq(small).unwrap());
    });
}
