#![cfg(feature = "python-compat-tests")]
//! Mull configure, report, and execute orchestration contracts.

#[path = "python_contracts/agent_comm_support.rs"]
#[allow(dead_code)]
mod comm_support;
#[path = "python_contracts/mutation_mull_fixture.rs"]
#[allow(dead_code)]
mod fixture;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use fixture::{
    built_tree, campaign, campaign_error, dict, drive_execute, equal, field, installed_plugin,
    mull, mutant, recorded_runs, repo_src, signature_kwargs, DriveInput,
};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyCFunction, PyDict, PyList};
use serde_json::json;
use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};
use support::{path, AttrPatch, Case};

fn build<'py>(
    py: Python<'py>,
    case: &Case,
    version: &str,
    receipt: &Bound<'py, PyDict>,
) -> PyResult<Bound<'py, PyAny>> {
    let kw = PyDict::new(py);
    kw.set_item("receipt", receipt).unwrap();
    kw.set_item("output_path", path(py, &case.root().join("receipt.json")))
        .unwrap();
    kw.set_item("environment", PyDict::new(py)).unwrap();
    mull(py).getattr("_build").unwrap().call(
        (
            campaign(py),
            path(py, case.root()),
            path(py, &case.root().join("build")),
            version,
        ),
        Some(&kw),
    )
}

#[test]
fn a_host_with_no_pass_plugin_is_refused_before_a_configure_is_paid_for() {
    let case = Case::new();
    Python::attach(|py| {
        let (calls, _run) = recorded_runs(py, vec![]);
        let receipt = dict(py);
        campaign_error(
            py,
            build(py, &case, "99", &receipt).unwrap_err(),
            "mull-ir-frontend-99",
        );
        assert!(calls.lock().unwrap().is_empty());
        equal(&receipt, &dict(py));
        assert!(!case.root().join("receipt.json").exists());
    });
}

#[test]
fn a_failed_configure_lands_in_the_receipt_and_never_reaches_the_build() {
    let case = Case::new();
    Python::attach(|py| {
        let _plugin = installed_plugin(py, &case);
        let (calls, _run) = recorded_runs(py, vec![(1, "ninja: not found".to_owned())]);
        let receipt = dict(py);
        campaign_error(
            py,
            build(py, &case, "18", &receipt).unwrap_err(),
            "the instrumented build failed",
        );
        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert!(!calls[0].contains(&"--build".to_owned()));
        assert!(field(&receipt, "status").eq("BASELINE_FAILED").unwrap());
        let saved: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(case.root().join("receipt.json")).unwrap())
                .unwrap();
        assert_eq!(saved["status"], "BASELINE_FAILED");
    });
}

#[test]
fn a_clean_configure_and_build_are_both_recorded_in_the_order_they_ran() {
    let case = Case::new();
    Python::attach(|py| {
        let _plugin = installed_plugin(py, &case);
        let (calls, _run) = recorded_runs(py, vec![(0, String::new()), (0, String::new())]);
        let receipt = dict(py);
        build(py, &case, "18", &receipt).unwrap();
        let calls = calls.lock().unwrap();
        assert_eq!(&calls[0][..1], ["cmake"]);
        assert_eq!(
            calls[1],
            [
                "cmake",
                "--build",
                case.root().join("build").to_str().unwrap()
            ]
        );
        assert_eq!(field(&receipt, "build").len().unwrap(), 2);
        assert!(!receipt.contains("status").unwrap());
    });
}

fn engine_reports<'py>(
    py: Python<'py>,
    build: &Path,
    receipt: &Bound<'py, PyDict>,
) -> PyResult<Bound<'py, PyAny>> {
    let kw = PyDict::new(py);
    kw.set_item("receipt", receipt).unwrap();
    kw.set_item("environment", PyDict::new(py)).unwrap();
    kw.set_item("profdata_tool", "/bin/llvm-profdata-18")
        .unwrap();
    mull(py).getattr("_engine_reports").unwrap().call(
        (campaign(py), "/bin/mull-runner-18", path(py, build)),
        Some(&kw),
    )
}

#[test]
fn every_declared_executable_is_covered_and_mutated_into_its_own_report() {
    let case = Case::new();
    Python::attach(|py| {
        let (build, names) = built_tree(py, &case, true);
        let profiled = Arc::new(Mutex::new(Vec::<String>::new()));
        let seen = Arc::clone(&profiled);
        let sig = signature_kwargs(py, &["executable", "build_dir", "name"]);
        let callback = PyCFunction::new_closure(py, None, None, move |args, kw| {
            let values = comm_support::bind_signature(&sig, args, kw)?.getattr("arguments")?;
            let name: String = values.get_item("name")?.extract()?;
            seen.lock().unwrap().push(name.clone());
            Ok::<Py<PyAny>, PyErr>(
                values
                    .get_item("build_dir")?
                    .call_method1("__truediv__", (format!("{name}.profdata"),))?
                    .unbind(),
            )
        })
        .unwrap();
        let _profile = AttrPatch::replace(mull(py).as_any(), "_profile", callback.as_any());
        let (calls, _run) = recorded_runs(py, vec![(0, String::new()), (0, String::new())]);
        let receipt = dict(py);
        let reports = engine_reports(py, &build, &receipt)
            .unwrap()
            .cast_into::<PyList>()
            .unwrap();
        let expected: Vec<String> = names
            .iter()
            .map(|n| {
                Path::new(n)
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert_eq!(*profiled.lock().unwrap(), expected);
        assert_eq!(calls.lock().unwrap().len(), names.len());
        assert_eq!(reports.len(), names.len());
        assert_eq!(field(&receipt, "engine_result").len().unwrap(), names.len());
    });
}

#[test]
fn a_missing_elements_report_stops_the_run_and_names_the_path() {
    let case = Case::new();
    Python::attach(|py| {
        let (build, _names) = built_tree(py, &case, false);
        let sig = signature_kwargs(py, &["executable", "build_dir", "name"]);
        let callback = PyCFunction::new_closure(py, None, None, move |args, kw| {
            let values = comm_support::bind_signature(&sig, args, kw)?.getattr("arguments")?;
            Ok::<Py<PyAny>, PyErr>(values.get_item("build_dir")?.unbind())
        })
        .unwrap();
        let _profile = AttrPatch::replace(mull(py).as_any(), "_profile", callback.as_any());
        let (_calls, _run) = recorded_runs(py, vec![(1, String::new())]);
        let receipt = dict(py);
        let error = engine_reports(py, &build, &receipt).unwrap_err();
        campaign_error(py, error.clone_ref(py), "wrote no Elements report");
        assert!(error.to_string().contains("test_kernels.json"));
    });
}

#[test]
fn a_run_over_drifted_sources_is_refused_before_anything_is_built() {
    let case = Case::new();
    Python::attach(|py| {
        let run = drive_execute(
            py,
            &case,
            DriveInput {
                drifted: json!({"aria_core/src/cpu/norm.cpp":"0"}),
                ..Default::default()
            },
            None,
        );
        campaign_error(
            py,
            run.result.unwrap_err(),
            "snapshot source hashes drifted",
        );
        assert!(run.calls.lock().unwrap().is_empty());
        assert!(!run.seen.contains("build").unwrap());
    });
}

#[test]
fn a_runner_from_another_llvm_release_is_refused_by_name() {
    let case = Case::new();
    Python::attach(|py| {
        let run = drive_execute(
            py,
            &case,
            DriveInput {
                binary: "/bin/mull-runner-17",
                ..Default::default()
            },
            None,
        );
        campaign_error(py, run.result.unwrap_err(), "is not the manifest's");
        assert!(run.calls.lock().unwrap().is_empty());
    });
}

#[test]
fn a_red_baseline_stops_the_campaign_and_is_written_to_the_receipt() {
    let case = Case::new();
    Python::attach(|py| {
        let run = drive_execute(
            py,
            &case,
            DriveInput {
                baseline: 1,
                ..Default::default()
            },
            None,
        );
        campaign_error(py, run.result.unwrap_err(), "unmutated baseline failed");
        let saved: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(case.root().join("receipt.json")).unwrap())
                .unwrap();
        assert_eq!(saved["status"], "BASELINE_FAILED");
    });
}

#[test]
fn a_clean_run_records_the_baseline_the_engine_version_and_the_scoped_corpus() {
    let case = Case::new();
    Python::attach(|py| {
        let run = drive_execute(py, &case, DriveInput::default(), None);
        run.result.unwrap();
        let expected = PyList::new(
            py,
            campaign(py)
                .getattr("test_argv")
                .unwrap()
                .try_iter()
                .unwrap()
                .map(Result::unwrap),
        )
        .unwrap();
        let calls = run.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        let expected_strings: Vec<String> = expected.extract().unwrap();
        assert_eq!(calls[0], expected_strings);
        equal(&field(&run.receipt, "baseline_argv"), &expected);
        assert!(field(&run.receipt, "engine_version").eq("18.0.0").unwrap());
        let mutants = field(&run.receipt, "mutants")
            .cast_into::<PyList>()
            .unwrap();
        let outcomes: Vec<String> = mutants
            .iter()
            .map(|r| field(&r, "outcome").extract().unwrap())
            .collect();
        assert_eq!(outcomes, ["KILLED"]);
        let summary = field(&run.receipt, "engine_summary");
        assert!(field(&summary, "mutants_in_scope").eq(1).unwrap());
        assert!(field(&summary, "mutants_reported").eq(1).unwrap());
        equal(
            &field(&summary, "files_mutated"),
            &PyList::new(py, ["aria_core/src/cpu/norm.cpp"]).unwrap(),
        );
        assert!(!run.receipt.contains("status").unwrap());
        equal(
            &field(&run.seen, "build"),
            &path(py, &repo_src().join(".mull-build")),
        );
        let environment = field(&run.seen, "environment");
        assert!(field(&environment, "PATH")
            .eq(std::env::var("PATH").unwrap())
            .unwrap());
        assert!(field(&environment, "MULL_CONFIG")
            .eq(case.root().join("mull.yml").to_str().unwrap())
            .unwrap());
        equal(
            &field(&run.seen, "scope_source"),
            &campaign(py).getattr("source").unwrap(),
        );
        equal(&field(&run.seen, "scope_worktree"), &path(py, &repo_src()));
        assert!(field(&run.seen, "report_args")
            .contains("/bin/llvm-profdata-18")
            .unwrap());
    });
}

#[test]
fn the_build_directory_is_the_manifest_option_and_falls_back_to_mull_build() {
    let case = Case::new();
    Python::attach(|py| {
        let declared = campaign(py);
        declared
            .getattr("options")
            .unwrap()
            .set_item("build_dir", "elsewhere")
            .unwrap();
        let run = drive_execute(py, &case, DriveInput::default(), Some(declared));
        run.result.unwrap();
        equal(
            &field(&run.seen, "build"),
            &path(py, &repo_src().join("elsewhere")),
        );
        let absent = campaign(py);
        absent
            .getattr("options")
            .unwrap()
            .del_item("build_dir")
            .unwrap();
        let run = drive_execute(py, &case, DriveInput::default(), Some(absent));
        run.result.unwrap();
        equal(
            &field(&run.seen, "build"),
            &path(py, &repo_src().join(".mull-build")),
        );
    });
}

#[test]
fn a_host_with_no_path_hands_the_build_an_empty_one_not_a_fabricated_one() {
    let mut case = Case::new();
    case.remove_env("PATH");
    Python::attach(|py| {
        let run = drive_execute(py, &case, DriveInput::default(), None);
        run.result.unwrap();
        assert!(field(&field(&run.seen, "environment"), "PATH")
            .eq("")
            .unwrap());
    });
}

#[test]
fn a_corpus_that_all_falls_outside_the_declared_scope_is_refused() {
    let case = Case::new();
    Python::attach(|py| {
        let mut outside = fixture::report(
            vec![mutant("Killed", 2, 12)],
            "aria_designer/runtime/tests/t.cpp",
            fixture::SOURCE_TEXT,
        );
        outside["config"] = json!({"mullVersion":"18.0.0"});
        let run = drive_execute(
            py,
            &case,
            DriveInput {
                reports: Some(vec![outside]),
                ..Default::default()
            },
            None,
        );
        campaign_error(py, run.result.unwrap_err(), "score an empty corpus");
    });
}
