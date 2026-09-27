#![cfg(feature = "python-compat-tests")]
//! Cargo-mutants adapter report, bound, and environment contracts in Rust.

#[path = "python_contracts/mutation_adapter_support.rs"]
#[allow(dead_code)]
mod fixture;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use fixture::{
    campaign, cargo, cargo_baseline, cargo_manifest, cargo_outcome, cargo_outcome_with, cargo_rows,
    equal, generated, py_json, root, CARGO_CRATE,
};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyList};
use serde_json::json;
use std::path::Path;
use support::{module, path, Case};

fn subject<'py>(py: Python<'py>) -> Bound<'py, PyAny> {
    campaign(py, &cargo_manifest())
}
fn row<'py>(rows: &Bound<'py, PyList>, index: usize) -> Bound<'py, PyAny> {
    rows.get_item(index).unwrap()
}
fn field<'py>(value: &Bound<'py, PyAny>, key: &str) -> Bound<'py, PyAny> {
    value.get_item(key).unwrap()
}
fn as_string(value: &Bound<'_, PyAny>) -> String {
    value.extract().unwrap()
}
fn as_strings(value: &Bound<'_, PyAny>) -> Vec<String> {
    value.extract().unwrap()
}
fn argv<'py>(py: Python<'py>, subject: &Bound<'py, PyAny>) -> Bound<'py, PyList> {
    fixture::as_list(
        &cargo(py)
            .getattr("_engine_argv")
            .unwrap()
            .call1((subject, "/bin/cargo-mutants", path(py, Path::new("/out"))))
            .unwrap(),
    )
}
fn campaign_error(py: Python<'_>, error: PyErr, contains: &str) {
    let kind = module(py, "conductor.mutation_scope")
        .getattr("CampaignError")
        .unwrap();
    assert!(error.matches(py, &kind).unwrap(), "{error}");
    assert!(error.to_string().contains(contains), "{error}");
}
fn expected_set<'py>(py: Python<'py>, names: &[&str]) -> Bound<'py, PyAny> {
    py.import("builtins")
        .unwrap()
        .getattr("set")
        .unwrap()
        .call1((names,))
        .unwrap()
}

#[test]
fn a_mutant_that_never_compiled_is_not_a_kill() {
    let _case = Case::new();
    Python::attach(|py| {
        let rows = cargo_rows(
            py,
            vec![
                cargo_baseline("Success"),
                cargo_outcome_with("Unviable", 437, "Default::default()"),
                cargo_outcome("CaughtMutant"),
            ],
        );
        let outcomes: Vec<String> = rows
            .iter()
            .map(|item| as_string(&field(&item, "outcome")))
            .collect();
        let actual = py_json(py, json!(outcomes));
        equal(
            &py.import("builtins")
                .unwrap()
                .getattr("set")
                .unwrap()
                .call1((actual,))
                .unwrap(),
            &expected_set(py, &["UNVIABLE", "KILLED"]),
        );
        assert_eq!(rows.len(), 2);
    });
}

#[test]
fn rows_carry_the_crate_relative_path_not_the_cargo_one() {
    let _case = Case::new();
    Python::attach(|py| {
        let rows = cargo_rows(
            py,
            vec![cargo_baseline("Success"), cargo_outcome("MissedMutant")],
        );
        let row = row(&rows, 0);
        assert_eq!(
            as_string(&field(&row, "path")),
            format!("{CARGO_CRATE}/src/lib.rs")
        );
        assert_eq!(as_string(&field(&row, "outcome")), "SURVIVED");
        assert_eq!(
            as_string(&field(&row, "function")),
            "snapshot_stale_branches"
        );
    });
}

#[test]
fn mutants_are_ordered_and_named_independently_of_their_line() {
    let _case = Case::new();
    Python::attach(|py| {
        let early = cargo_rows(
            py,
            vec![cargo_outcome_with("MissedMutant", 100, "Ok(vec![])")],
        );
        let late = cargo_rows(
            py,
            vec![cargo_outcome_with("MissedMutant", 900, "Ok(vec![])")],
        );
        equal(&field(&row(&early, 0), "id"), &field(&row(&late, 0), "id"));
        let two = cargo_rows(
            py,
            vec![
                cargo_outcome_with("CaughtMutant", 800, "Ok(vec![])"),
                cargo_outcome_with("MissedMutant", 100, "Ok(vec![])"),
            ],
        );
        let lines: Vec<i64> = two
            .iter()
            .map(|item| field(&item, "line").extract().unwrap())
            .collect();
        assert_eq!(lines, [100, 800]);
        let ids: Vec<String> = two
            .iter()
            .map(|item| as_string(&field(&item, "id")))
            .collect();
        assert_eq!(
            ids.iter().collect::<std::collections::HashSet<_>>().len(),
            2
        );
    });
}

#[test]
fn an_unknown_summary_is_refused_not_guessed() {
    let _case = Case::new();
    Python::attach(|py| {
        let report = py_json(py, json!({"outcomes":[cargo_outcome("Flaky")]}));
        let error = cargo(py)
            .getattr("_rows")
            .unwrap()
            .call1((report, CARGO_CRATE))
            .unwrap_err();
        campaign_error(py, error, "unknown summary");
    });
}

#[test]
fn a_scenario_that_is_neither_baseline_nor_mutant_is_refused() {
    let _case = Case::new();
    Python::attach(|py| {
        let report = py_json(
            py,
            json!({"outcomes":[{"scenario":{"Something":{}},"summary":"Success"}]}),
        );
        let error = cargo(py)
            .getattr("_rows")
            .unwrap()
            .call1((report, CARGO_CRATE))
            .unwrap_err();
        campaign_error(py, error, "unknown scenario");
    });
}

#[test]
fn a_red_unmutated_baseline_stops_the_run() {
    let _case = Case::new();
    Python::attach(|py| {
        let api = cargo(py).getattr("_require_baseline").unwrap();
        let failed = py_json(
            py,
            json!({"outcomes":[cargo_baseline("Failure"),cargo_outcome("CaughtMutant")]}),
        );
        campaign_error(py, api.call1((failed,)).unwrap_err(), "baseline failed");
        let missing = py_json(py, json!({"outcomes":[cargo_outcome("CaughtMutant")]}));
        campaign_error(
            py,
            api.call1((missing,)).unwrap_err(),
            "no baseline scenario",
        );
        let passed = py_json(
            py,
            json!({"outcomes":[cargo_baseline("Success"),cargo_outcome("CaughtMutant")]}),
        );
        assert!(api.call1((passed,)).unwrap().is_none());
    });
}

#[test]
fn the_invocation_pins_every_bound_from_the_manifest() {
    let _case = Case::new();
    Python::attach(|py| {
        let subject = subject(py);
        subject.setattr("exclude", ("src/generated/**",)).unwrap();
        let raw = argv(py, &subject);
        let items: Vec<String> = raw.extract().unwrap();
        assert_eq!(&items[..2], ["/bin/cargo-mutants", "mutants"]);
        let after =
            |flag: &str| items[items.iter().position(|item| item == flag).unwrap() + 1].as_str();
        assert_eq!(after("--output"), "/out");
        assert_eq!(
            after("--jobs"),
            subject
                .getattr("jobs")
                .unwrap()
                .str()
                .unwrap()
                .to_str()
                .unwrap()
        );
        assert_eq!(after("--timeout"), "30");
        assert_eq!(after("--package"), "conductor-native");
        assert!(after("--manifest-path").ends_with("Cargo.toml"));
        let values = |flag: &str| {
            items
                .windows(2)
                .filter(|pair| pair[0] == flag)
                .map(|pair| pair[1].clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            values("--file"),
            as_strings(&subject.getattr("source").unwrap())
        );
        assert_eq!(values("--exclude"), ["src/generated/**"]);
    });
}

#[test]
fn a_crate_without_a_package_is_not_narrowed_to_one() {
    let _case = Case::new();
    Python::attach(|py| {
        let subject = subject(py);
        subject
            .setattr(
                "options",
                py_json(py, json!({"manifest_path":"x/Cargo.toml"})),
            )
            .unwrap();
        let items: Vec<String> = argv(py, &subject).extract().unwrap();
        assert!(!items.iter().any(|item| item == "--package"));
    });
}

#[test]
fn an_unpinned_bound_is_derived_before_the_invocation_is_built() {
    let _case = Case::new();
    Python::attach(|py| {
        let subject = subject(py);
        assert_eq!(
            subject
                .getattr("mutant_timeout_seconds")
                .unwrap()
                .extract::<i64>()
                .unwrap(),
            30
        );
        subject
            .setattr("mutant_timeout_seconds", py.None())
            .unwrap();
        let resolved: i64 = generated(py)
            .getattr("resolve_mutant_timeout")
            .unwrap()
            .call1((&subject, 41.2))
            .unwrap()
            .extract()
            .unwrap();
        let items: Vec<String> = argv(py, &subject).extract().unwrap();
        assert_eq!(resolved, 124);
        assert_eq!(
            items[items.iter().position(|item| item == "--timeout").unwrap() + 1],
            "124"
        );
    });
}

#[test]
fn every_receipt_field_a_row_carries_is_pinned() {
    let _case = Case::new();
    Python::attach(|py| {
        let rows = cargo_rows(py, vec![cargo_outcome("MissedMutant")]);
        assert_eq!(rows.len(), 1);
        let row = row(&rows, 0);
        let names = [
            "id",
            "outcome",
            "path",
            "line",
            "operator",
            "original_text",
            "mutated_text",
            "function",
            "package",
            "duration_seconds",
        ];
        equal(
            &py.import("builtins")
                .unwrap()
                .getattr("set")
                .unwrap()
                .call1((&row,))
                .unwrap(),
            &expected_set(py, &names),
        );
        assert_eq!(field(&row, "line").extract::<i64>().unwrap(), 437);
        assert_eq!(as_string(&field(&row, "operator")), "FnValue");
        assert_eq!(as_string(&field(&row, "package")), "snapshot-retention");
        assert_eq!(as_string(&field(&row, "mutated_text")), "Ok(vec![])");
        assert_eq!(
            as_string(&field(&row, "original_text")),
            "snapshot_stale_branches-> Result<Vec<SnapshotAction>, Error>"
        );
        assert_eq!(
            field(&row, "duration_seconds").extract::<f64>().unwrap(),
            1.75
        );
    });
}

#[test]
fn a_mutant_outside_any_function_is_still_named() {
    let _case = Case::new();
    Python::attach(|py| {
        let mut record = cargo_outcome("MissedMutant");
        record["scenario"]["Mutant"]
            .as_object_mut()
            .unwrap()
            .remove("function");
        let rows = cargo_rows(py, vec![record]);
        assert_eq!(rows.len(), 1);
        let row = row(&rows, 0);
        assert!(field(&row, "function").is_none());
        assert_eq!(as_string(&field(&row, "original_text")), "<file>");
        assert!(field(&row, "id").is_truthy().unwrap());
    });
}

#[test]
fn a_duration_is_recorded_to_microseconds() {
    let _case = Case::new();
    Python::attach(|py| {
        let mut record = cargo_outcome("CaughtMutant");
        record["phase_results"] = json!([{"phase":"Build","duration":1.234_567_8}]);
        let rows = cargo_rows(py, vec![record]);
        assert_eq!(
            field(&row(&rows, 0), "duration_seconds")
                .extract::<f64>()
                .unwrap(),
            1.234568
        );
    });
}

#[test]
fn a_crate_without_a_manifest_path_is_refused() {
    let _case = Case::new();
    Python::attach(|py| {
        let subject = subject(py);
        subject.setattr("options", PyDict::new(py)).unwrap();
        let error = cargo(py)
            .getattr("_engine_argv")
            .unwrap()
            .call1((&subject, "/bin/cargo-mutants", path(py, Path::new("/out"))))
            .unwrap_err();
        campaign_error(py, error, "manifest_path");
    });
}

#[test]
fn the_campaign_under_test_is_wired_end_to_end() {
    let _case = Case::new();
    Python::attach(|py| {
        let loaded = subject(py);
        assert_eq!(
            as_string(&loaded.getattr("mutation_engine").unwrap()),
            "cargo-mutants"
        );
        assert_eq!(as_string(&loaded.getattr("language").unwrap()), "rust");
        assert!(loaded
            .getattr("survivor_baseline")
            .unwrap()
            .is_truthy()
            .unwrap());
        let pins = loaded.getattr("source_sha256").unwrap();
        for relative in pins.call_method0("keys").unwrap().try_iter().unwrap() {
            let relative: String = relative.unwrap().extract().unwrap();
            assert!(root().join(&relative).is_file(), "{relative}");
        }
    });
}

#[test]
fn parallel_workers_may_not_share_one_cargo_build_directory() {
    let _case = Case::new();
    Python::attach(|py| {
        let subject = subject(py);
        subject
            .setattr(
                "environment",
                py_json(py, json!({"CARGO_TARGET_DIR":"/home/tim/.cargo/shared"})),
            )
            .unwrap();
        subject.setattr("jobs", 4).unwrap();
        let error = cargo(py)
            .getattr("_engine_argv")
            .unwrap()
            .call1((&subject, "/bin/cargo-mutants", path(py, Path::new("/out"))))
            .unwrap_err();
        campaign_error(py, error, "share one");
        subject.setattr("jobs", 1).unwrap();
        argv(py, &subject);
    });
}

#[test]
fn the_committed_campaign_does_not_pin_a_shared_target_dir() {
    let _case = Case::new();
    Python::attach(|py| {
        let subject = subject(py);
        assert!(subject.getattr("jobs").unwrap().extract::<i64>().unwrap() > 1);
        assert!(!subject
            .getattr("environment")
            .unwrap()
            .contains("CARGO_TARGET_DIR")
            .unwrap());
    });
}

#[test]
fn the_run_environment_exports_the_snapshot_interpreter() {
    let mut case = Case::new();
    Python::attach(|py| {
        let subject = subject(py);
        subject.setattr("environment",py_json(py,json!({"RUST_LOG":"debug","CONDUCTOR_SNAPSHOT_PYTHON":"/stale/pin/from/the/manifest"}))).unwrap();
        let built = cargo(py)
            .getattr("_environment")
            .unwrap()
            .call1((&subject,))
            .unwrap();
        let executable = module(py, "sys").getattr("executable").unwrap();
        equal(&field(&built, "CONDUCTOR_SNAPSHOT_PYTHON"), &executable);
        assert_eq!(as_string(&field(&built, "RUST_LOG")), "debug");
        assert_eq!(
            as_string(&field(&built, "PATH")),
            std::env::var("PATH").unwrap_or_default()
        );
        assert_eq!(as_string(&field(&built, "CARGO_TERM_COLOR")), "never");
        case.remove_env("PATH");
        let again = cargo(py)
            .getattr("_environment")
            .unwrap()
            .call1((&subject,))
            .unwrap();
        assert_eq!(as_string(&field(&again, "PATH")), "");
    });
}
