#![cfg(feature = "python-compat-tests")]
//! Fest adapter report, configuration, and child-environment contracts in Rust.

#[path = "python_contracts/mutation_adapter_support.rs"]
#[allow(dead_code)]
mod fixture;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use fixture::{
    campaign, equal, fest, fest_manifest, fest_mutant, fest_result, fest_rows, generated, py_json,
    WORKTREE,
};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyList};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use support::{module, path, Case};

fn row<'py>(rows: &Bound<'py, PyList>, index: usize) -> Bound<'py, PyAny> {
    rows.get_item(index).unwrap()
}
fn field<'py>(value: &Bound<'py, PyAny>, key: &str) -> Bound<'py, PyAny> {
    value.get_item(key).unwrap()
}
fn as_string(value: &Bound<'_, PyAny>) -> String {
    value.extract().unwrap()
}
fn expected_list<'py>(py: Python<'py>, values: &[&str]) -> Bound<'py, PyAny> {
    PyList::new(py, values).unwrap().into_any()
}
fn expected_set<'py>(py: Python<'py>, values: &[&str]) -> Bound<'py, PyAny> {
    py.import("builtins")
        .unwrap()
        .getattr("set")
        .unwrap()
        .call1((values,))
        .unwrap()
}
fn campaign_error(py: Python<'_>, error: PyErr, contains: &str) {
    let kind = module(py, "conductor.mutation_scope")
        .getattr("CampaignError")
        .unwrap();
    assert!(error.matches(py, &kind).unwrap(), "{error}");
    assert!(error.to_string().contains(contains), "{error}");
}
fn env_campaign<'py>(py: Python<'py>, environment: Value, sources: Value) -> Bound<'py, PyAny> {
    let kwargs = PyDict::new(py);
    kwargs
        .set_item("environment", py_json(py, environment))
        .unwrap();
    kwargs
        .set_item("source_sha256", py_json(py, sources))
        .unwrap();
    module(py, "types")
        .getattr("SimpleNamespace")
        .unwrap()
        .call((), Some(&kwargs))
        .unwrap()
}
fn environment<'py>(
    py: Python<'py>,
    campaign: &Bound<'py, PyAny>,
    root: &Path,
) -> Bound<'py, PyAny> {
    fest(py)
        .getattr("_environment")
        .unwrap()
        .call1((campaign, path(py, root)))
        .unwrap()
}

#[test]
fn rows_are_ordered_by_position_so_repeats_name_stably() {
    let _case = Case::new();
    Python::attach(|py| {
        let rows = fest_rows(
            py,
            vec![
                fest_result("Survived", 200, 1),
                fest_result("Killed", 100, 1),
            ],
        );
        assert_eq!(rows.len(), 2);
        let ids: Vec<String> = rows
            .iter()
            .map(|item| as_string(&field(&item, "id")))
            .collect();
        assert_eq!(
            ids.iter().collect::<std::collections::HashSet<_>>().len(),
            2
        );
        assert_eq!(as_string(&field(&row(&rows, 0), "outcome")), "KILLED");
        assert_eq!(as_string(&field(&row(&rows, 1), "outcome")), "SURVIVED");
        assert_eq!(
            as_string(&field(&row(&rows, 0), "path")),
            "conductor/gate_rollout.py"
        );
    });
}

#[test]
fn every_receipt_field_a_row_carries_is_pinned() {
    let _case = Case::new();
    Python::attach(|py| {
        let rows = fest_rows(py, vec![fest_result("Survived", 17, 42)]);
        assert_eq!(rows.len(), 1);
        let row = row(&rows, 0);
        let names = [
            "id",
            "outcome",
            "path",
            "line",
            "byte_offset",
            "byte_length",
            "operator",
            "original_text",
            "mutated_text",
            "tests_run",
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
        assert_eq!(field(&row, "line").extract::<i64>().unwrap(), 42);
        let offset = field(&row, "byte_offset").extract::<i64>().unwrap();
        let length = field(&row, "byte_length").extract::<i64>().unwrap();
        let original = as_string(&field(&row, "original_text"));
        assert_eq!((offset, length), (17, original.chars().count() as i64));
        assert_eq!(as_string(&field(&row, "operator")), "constant_replace");
        assert_eq!(original, "\"gh\"");
        assert_eq!(as_string(&field(&row, "mutated_text")), "\"\"");
        assert_eq!(as_string(&field(&row, "path")), "conductor/gate_rollout.py");
    });
}

#[test]
fn a_duration_is_recorded_in_seconds_not_in_fests_two_fields() {
    let _case = Case::new();
    Python::attach(|py| {
        let report = json!({"results":[{"mutant":fest_mutant(0,1),"status":"Killed","tests_run":["conductor/test_gate_rollout.py::test_one"],"duration":{"secs":3,"nanos":500_000_000}}]});
        let rows = fixture::as_list(
            &fest(py)
                .getattr("_rows")
                .unwrap()
                .call1((py_json(py, report), path(py, Path::new(WORKTREE))))
                .unwrap(),
        );
        assert_eq!(rows.len(), 1);
        let row = row(&rows, 0);
        assert_eq!(
            field(&row, "duration_seconds").extract::<f64>().unwrap(),
            3.5
        );
        let tests = field(&row, "tests_run");
        assert!(tests
            .is_instance(&py.import("builtins").unwrap().getattr("list").unwrap())
            .unwrap());
        equal(
            &tests,
            &expected_list(py, &["conductor/test_gate_rollout.py::test_one"]),
        );
    });
}

#[test]
fn a_duration_missing_a_field_is_read_as_zero_not_as_one_second() {
    let _case = Case::new();
    Python::attach(|py| {
        let mut first = fest_result("Killed", 0, 1);
        first["duration"] = json!({"nanos":5_000_000});
        assert_eq!(
            field(&row(&fest_rows(py, vec![first]), 0), "duration_seconds")
                .extract::<f64>()
                .unwrap(),
            0.005
        );
        let mut empty = fest_result("Killed", 0, 1);
        empty["duration"] = json!({});
        assert_eq!(
            field(&row(&fest_rows(py, vec![empty]), 0), "duration_seconds")
                .extract::<f64>()
                .unwrap(),
            0.0
        );
    });
}

#[test]
fn a_duration_is_recorded_to_microseconds() {
    let _case = Case::new();
    Python::attach(|py| {
        let mut result = fest_result("Killed", 0, 1);
        result["duration"] = json!({"secs":1,"nanos":234_567_890});
        assert_eq!(
            field(&row(&fest_rows(py, vec![result]), 0), "duration_seconds")
                .extract::<f64>()
                .unwrap(),
            1.234568
        );
    });
}

#[test]
fn an_unknown_engine_status_is_refused_not_guessed() {
    let _case = Case::new();
    Python::attach(|py| {
        let report = py_json(py, json!({"results":[fest_result("Flaky",0,1)]}));
        let error = fest(py)
            .getattr("_rows")
            .unwrap()
            .call1((report, path(py, Path::new(WORKTREE))))
            .unwrap_err();
        campaign_error(py, error, "unknown status");
        let mut missing = fest_result("Killed", 0, 1);
        missing.as_object_mut().unwrap().remove("status");
        let report = py_json(py, json!({"results":[missing]}));
        let error = fest(py)
            .getattr("_rows")
            .unwrap()
            .call1((report, path(py, Path::new(WORKTREE))))
            .unwrap_err();
        campaign_error(py, error, "unknown status ''");
    });
}

#[test]
fn coverage_is_measured_over_directories_never_a_single_file() {
    let _case = Case::new();
    Python::attach(|py| {
        let api = fest(py).getattr("_coverage_targets").unwrap();
        for (input, expected) in [
            ("conductor/gate_rollout.py", "conductor"),
            ("component_fab/**/*.py", "component_fab"),
            ("*.py", "."),
        ] {
            let patterns = PyList::new(py, [input]).unwrap();
            let actual = api.call1((patterns,)).unwrap();
            equal(&actual, &expected_list(py, &[expected]));
        }
    });
}

#[test]
fn the_campaign_under_test_is_wired_end_to_end() {
    let _case = Case::new();
    Python::attach(|py| {
        let loaded = campaign(py, &fest_manifest());
        assert_eq!(
            as_string(&loaded.getattr("mutation_engine").unwrap()),
            "fest"
        );
        equal(
            &loaded.getattr("source").unwrap(),
            &pyo3::types::PyTuple::new(py, ["src/conductor/gate_rollout.py"])
                .unwrap()
                .into_any(),
        );
        assert!(loaded
            .getattr("survivor_baseline")
            .unwrap()
            .is_truthy()
            .unwrap());
    });
}

#[test]
fn the_config_carries_the_bound_the_run_resolved() {
    let _case = Case::new();
    Python::attach(|py| {
        let loaded = campaign(py, &fest_manifest());
        loaded.setattr("mutant_timeout_seconds", py.None()).unwrap();
        generated(py)
            .getattr("resolve_mutant_timeout")
            .unwrap()
            .call1((&loaded, 0.4))
            .unwrap();
        let actual: String = fest(py)
            .getattr("_config")
            .unwrap()
            .call1((&loaded, "/v/bin/python"))
            .unwrap()
            .extract()
            .unwrap();
        let expected=[
            "[fest]",
            "source = [\"src/conductor/gate_rollout.py\"]",
            "exclude = [\"**/test_*.py\", \"**/conftest.py\"]",
            "timeout = 60","seed = 0","workers = 1",
            "test_command = [\"/v/bin/python\", \"-m\", \"pytest\", \"-q\", \"src/conductor/test_gate_rollout.py\"]",
            "output = \"json\"","backend = \"subprocess\"","",
        ].join("\n");
        assert_eq!(actual, expected);
    });
}

#[test]
fn the_environment_loads_the_eviction_plugin_for_every_child() {
    let case = Case::new();
    case.mkdir("src");
    Python::attach(|py| {
        let subject = env_campaign(
            py,
            json!({}),
            json!({"src/conductor/mutated.py":"0".repeat(64)}),
        );
        let env = environment(py, &subject, case.root());
        let plugin = module(py, "conductor.mutation_pycache_evict");
        assert_eq!(
            as_string(&field(&env, "PYTEST_ADDOPTS")),
            format!("-p {}", as_string(&plugin.getattr("PLUGIN_NAME").unwrap()))
        );
        let scratch_key = as_string(&plugin.getattr("SCRATCH_ENV").unwrap());
        let sources_key = as_string(&plugin.getattr("SOURCES_ENV").unwrap());
        let scratch = module(py, "conductor.bytecode_isolation")
            .getattr("scratch_root_for")
            .unwrap()
            .call1((path(py, case.root()),))
            .unwrap();
        assert_eq!(
            as_string(&field(&env, &scratch_key)),
            scratch.str().unwrap().to_str().unwrap()
        );
        assert_eq!(
            as_string(&field(&env, &sources_key)),
            case.root()
                .join("src/conductor/mutated.py")
                .display()
                .to_string()
        );
    });
}

#[test]
fn a_campaigns_declared_addopts_survive_with_the_plugin_appended() {
    let case = Case::new();
    case.mkdir("src");
    Python::attach(|py| {
        let subject = env_campaign(py, json!({"PYTEST_ADDOPTS":"--timeout 30"}), json!({}));
        let env = environment(py, &subject, case.root());
        let plugin = as_string(
            &module(py, "conductor.mutation_pycache_evict")
                .getattr("PLUGIN_NAME")
                .unwrap(),
        );
        assert_eq!(
            as_string(&field(&env, "PYTEST_ADDOPTS")),
            format!("--timeout 30 -p {plugin}")
        );
    });
}

#[test]
fn the_environment_binds_the_run_to_this_venv_and_snapshot() {
    let mut case = Case::new();
    case.mkdir("src");
    case.set_env("PATH", "/usr/bin");
    Python::attach(|py| {
        let subject = env_campaign(py, json!({"PYTHONPATH":"declared/extra"}), json!({}));
        let env = environment(py, &subject, case.root());
        let separator = as_string(&module(py, "os").getattr("pathsep").unwrap());
        assert_eq!(
            as_string(&field(&env, "PYTHONPATH")),
            [
                case.root().display().to_string(),
                case.root().join("src").display().to_string(),
                "declared/extra".to_owned()
            ]
            .join(&separator)
        );
        let executable = as_string(&module(py, "sys").getattr("executable").unwrap());
        let binary = PathBuf::from(&executable);
        let bin = binary.parent().unwrap().display().to_string();
        let venv = binary
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .display()
            .to_string();
        assert_eq!(as_string(&field(&env, "VIRTUAL_ENV")), venv);
        assert_eq!(
            as_string(&field(&env, "PATH")),
            [bin.clone(), "/usr/bin".to_owned()].join(&separator)
        );
        case.remove_env("PATH");
        let again = environment(py, &subject, case.root());
        assert_eq!(
            as_string(&field(&again, "PATH")),
            format!("{bin}{separator}")
        );
    });
}
