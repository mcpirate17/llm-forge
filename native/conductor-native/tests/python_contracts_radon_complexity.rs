#![cfg(feature = "python-compat-tests")]
//! Rust-owned contracts for the shipped Radon complexity ratchet.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::prelude::*;
use pyo3::types::{PyAny, PyModule};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use support::{assert_error, module, path, text, AttrPatch, Case};

const KEY: &str = "conductor/example.py::widget";

fn radon<'py>(py: Python<'py>) -> Bound<'py, PyModule> {
    module(py, "conductor.radon_complexity")
}

fn finding(key: &str, complexity: i32, rank: &str) -> Value {
    let (source, name) = key.split_once("::").unwrap();
    json!({
        "key": key, "path": source, "name": name, "type": "F",
        "line": 1, "complexity": complexity, "rank": rank,
    })
}

fn baseline(case: &Case, entries: Vec<Value>) -> PathBuf {
    case.write(
        "baseline.json",
        &json!({"minimum_rank":"D","findings":entries}).to_string(),
    )
}

fn py_json<'py>(py: Python<'py>, value: Value) -> Bound<'py, PyAny> {
    module(py, "json")
        .getattr("loads")
        .unwrap()
        .call1((value.to_string(),))
        .unwrap()
}

fn json_value(py: Python<'_>, value: &Bound<'_, PyAny>) -> Value {
    let raw: String = module(py, "json")
        .getattr("dumps")
        .unwrap()
        .call1((value,))
        .unwrap()
        .extract()
        .unwrap();
    serde_json::from_str(&raw).unwrap()
}

fn capture_stdout<'py>(py: Python<'py>) -> (Bound<'py, PyAny>, AttrPatch) {
    let output = module(py, "io")
        .getattr("StringIO")
        .unwrap()
        .call0()
        .unwrap();
    let patch = AttrPatch::replace(module(py, "sys").as_any(), "stdout", &output);
    (output, patch)
}

fn check(py: Python<'_>, baseline: &Path, findings: Vec<Value>, rank: &str) -> (i32, String) {
    let (output, _stdout) = capture_stdout(py);
    let code = radon(py)
        .getattr("_run_check")
        .unwrap()
        .call1((path(py, baseline), py_json(py, json!(findings)), rank))
        .unwrap()
        .extract()
        .unwrap();
    let printed = output.call_method0("getvalue").unwrap().extract().unwrap();
    (code, printed)
}

fn root(py: Python<'_>) -> PathBuf {
    PathBuf::from(text(&radon(py).getattr("REPO_ROOT").unwrap()))
}

fn resolved(py: Python<'_>, raw: Option<&str>) -> PathBuf {
    PathBuf::from(text(
        &radon(py)
            .getattr("_resolve_baseline")
            .unwrap()
            .call1((raw,))
            .unwrap(),
    ))
}

#[test]
fn conductor_is_scanned_by_default() {
    let _case = Case::new();
    Python::attach(|py| {
        let body = radon(py);
        let paths = body.getattr("DEFAULT_PATHS").unwrap();
        assert!(paths.contains("conductor").unwrap());
        let package = module(py, "conductor.project_paths")
            .getattr("package_relative")
            .unwrap()
            .call1((body.getattr("REPO_ROOT").unwrap(),))
            .unwrap();
        let own_module = text(
            &package
                .call_method1("__truediv__", ("radon_complexity.py",))
                .unwrap(),
        );
        let scan = body
            .getattr("_scan")
            .unwrap()
            .call1((vec![own_module.as_str()], Vec::<String>::new()))
            .unwrap();
        let findings = json_value(py, &scan.get_item(0).unwrap());
        let errors = json_value(py, &scan.get_item(1).unwrap());
        assert_eq!(errors, json!([]));
        let rows = findings.as_array().unwrap();
        assert!(
            !rows.is_empty(),
            "scanning a conductor file must produce blocks"
        );
        assert!(rows.iter().all(|row| row["path"] == own_module));
    });
}

#[test]
fn grandfathered_symbol_that_worsens_fails() {
    let case = Case::new();
    let baseline = baseline(&case, vec![finding(KEY, 21, "D")]);
    Python::attach(|py| {
        let (code, output) = check(py, &baseline, vec![finding(KEY, 25, "D")], "D");
        assert_eq!(code, 1);
        assert!(output.contains("blocks worsened"));
        assert!(output.contains("(21 -> 25)"));
    });
}

#[test]
fn grandfathered_symbol_holding_its_score_passes() {
    let case = Case::new();
    let baseline = baseline(&case, vec![finding(KEY, 21, "D")]);
    Python::attach(|py| {
        let (code, output) = check(py, &baseline, vec![finding(KEY, 21, "D")], "D");
        assert_eq!(code, 0);
        assert!(!output.contains("blocks worsened"));
    });
}

#[test]
fn improved_symbol_passes_and_asks_for_a_tighter_baseline() {
    let case = Case::new();
    let baseline = baseline(&case, vec![finding(KEY, 21, "D")]);
    Python::attach(|py| {
        let (code, output) = check(py, &baseline, vec![finding(KEY, 15, "C")], "D");
        assert_eq!(code, 0);
        assert!(output.contains("refresh-baseline"));
        assert!(output.contains(KEY));
    });
}

#[test]
fn new_block_above_minimum_rank_still_fails() {
    let case = Case::new();
    let baseline = baseline(&case, vec![]);
    Python::attach(|py| {
        let (code, output) = check(py, &baseline, vec![finding(KEY, 25, "D")], "D");
        assert_eq!(code, 1);
        assert!(output.contains("new D-F blocks"));
    });
}

#[test]
fn repeated_baseline_key_is_held_to_lowest_score() {
    let case = Case::new();
    let baseline = baseline(&case, vec![finding(KEY, 30, "D"), finding(KEY, 21, "D")]);
    Python::attach(|py| {
        let scores = radon(py)
            .getattr("_load_baseline")
            .unwrap()
            .call1((path(py, &baseline),))
            .unwrap();
        let scores = json_value(py, &scores);
        assert_eq!(scores.as_object().unwrap().len(), 1);
        assert_eq!(scores[KEY], 21);
    });
}

#[test]
fn blocks_below_minimum_rank_are_not_ratcheted() {
    let case = Case::new();
    let baseline = baseline(&case, vec![]);
    Python::attach(|py| {
        let (code, output) = check(
            py,
            &baseline,
            vec![finding("conductor/example.py::small", 3, "A")],
            "D",
        );
        assert_eq!(code, 0);
        assert!(!output.contains("new D-F blocks"));
    });
}

#[test]
fn ratchet_runs_in_pre_commit_profile() {
    let _case = Case::new();
    Python::attach(|py| {
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
        let checks = policy.getattr("checks").unwrap();
        let complexity = checks
            .try_iter()
            .unwrap()
            .map(Result::unwrap)
            .find(|check| {
                check
                    .getattr("check_id")
                    .unwrap()
                    .extract::<String>()
                    .unwrap()
                    == "complexity"
            })
            .expect("candidate_policy.toml declares no complexity check");
        let profiles = complexity.getattr("profiles").unwrap();
        assert!(profiles.contains("fast").unwrap());
        assert!(profiles.contains("full").unwrap());
    });
}

#[test]
fn unflagged_default_baseline_lives_in_host_tree() {
    let _case = Case::new();
    Python::attach(|py| {
        let host = root(py);
        let baseline = resolved(py, None);
        assert!(baseline.starts_with(&host), "{}", baseline.display());
        let configured = module(py, "conductor.project_paths")
            .getattr("radon_baseline_path")
            .unwrap()
            .call1((path(py, &host),))
            .unwrap()
            .call_method0("resolve")
            .unwrap();
        assert_eq!(baseline, PathBuf::from(text(&configured)));
    });
}

#[test]
fn default_follows_host_and_not_package() {
    let case = Case::new();
    case.write(
        "pyproject.toml",
        "[tool.conductor]\nradon_complexity_baseline = \"ratchet/base.json\"\n",
    );
    let bare = case.mkdir("bare");
    Python::attach(|py| {
        let baseline_path = module(py, "conductor.project_paths")
            .getattr("radon_baseline_path")
            .unwrap();
        assert_eq!(
            text(&baseline_path.call1((path(py, case.root()),)).unwrap()),
            case.root().join("ratchet/base.json").to_str().unwrap()
        );
        assert_eq!(
            text(&baseline_path.call1((path(py, &bare),)).unwrap()),
            bare.join("conductor/radon_complexity_baseline.json")
                .to_str()
                .unwrap()
        );
    });
}

#[test]
fn relative_baseline_flag_resolves_against_host_root() {
    let _case = Case::new();
    Python::attach(|py| {
        assert_eq!(
            resolved(py, Some("campaigns/x.json")),
            root(py).join("campaigns/x.json")
        );
    });
}

#[test]
fn absolute_baseline_flag_is_taken_as_given() {
    let case = Case::new();
    let target = case.root().join("elsewhere.json");
    Python::attach(|py| assert_eq!(resolved(py, target.to_str()), target));
}

#[test]
fn missing_baseline_is_refused_by_name() {
    let case = Case::new();
    let missing = case.root().join("absent.json");
    Python::attach(|py| {
        let error = radon(py)
            .getattr("_load_baseline")
            .unwrap()
            .call1((path(py, &missing),))
            .unwrap_err();
        let file_not_found = module(py, "builtins").getattr("FileNotFoundError").unwrap();
        let message = error.to_string();
        assert_error(py, error, &file_not_found, "radon_complexity_baseline");
        assert!(message.contains(missing.to_str().unwrap()));
    });
}
