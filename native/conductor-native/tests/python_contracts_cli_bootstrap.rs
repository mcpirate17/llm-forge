#![cfg(feature = "python-compat-tests")]
//! Rust-owned contracts for the conductor entry point and bootstrap alias.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::prelude::*;
use pyo3::types::{PyAny, PyCFunction, PyDict, PyModule, PyTuple};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::sync::{Arc, Mutex};
use support::{module, text, AttrPatch, Case};

fn capture<'py>(py: Python<'py>, stream: &str) -> (Bound<'py, PyAny>, AttrPatch) {
    let buffer = py
        .import("io")
        .unwrap()
        .getattr("StringIO")
        .unwrap()
        .call0()
        .unwrap();
    let patch = AttrPatch::replace(&py.import("sys").unwrap(), stream, &buffer);
    (buffer, patch)
}

fn exit_code(entry: &Bound<'_, PyModule>, args: Vec<&str>) -> i32 {
    entry
        .getattr("main")
        .unwrap()
        .call1((args,))
        .unwrap()
        .extract()
        .unwrap()
}

#[test]
fn no_arguments_prints_usage_and_exits_two() {
    let _case = Case::new();
    Python::attach(|py| {
        let entry = module(py, "conductor.__main__");
        let (stdout, _patch) = capture(py, "stdout");
        assert_eq!(exit_code(&entry, vec![]), 2);
        assert!(
            text(&stdout.call_method0("getvalue").unwrap()).contains("usage: python -m conductor")
        );
    });
}

#[test]
fn help_exits_zero() {
    let _case = Case::new();
    Python::attach(|py| {
        let entry = module(py, "conductor.__main__");
        let (stdout, _patch) = capture(py, "stdout");
        assert_eq!(exit_code(&entry, vec!["-h"]), 0);
        assert!(
            text(&stdout.call_method0("getvalue").unwrap()).contains("usage: python -m conductor")
        );
    });
}

#[test]
fn unknown_subcommand_exits_two() {
    let _case = Case::new();
    Python::attach(|py| {
        let entry = module(py, "conductor.__main__");
        let (stderr, _patch) = capture(py, "stderr");
        assert_eq!(exit_code(&entry, vec!["frobnicate"]), 2);
        assert!(text(&stderr.call_method0("getvalue").unwrap())
            .contains("unknown subcommand 'frobnicate'"));
    });
}

#[test]
fn every_subcommand_maps_to_a_module_with_a_main() {
    let _case = Case::new();
    Python::attach(|py| {
        let entry = module(py, "conductor.__main__");
        let importlib = py.import("importlib").unwrap();
        let items = entry
            .getattr("SUBCOMMANDS")
            .unwrap()
            .call_method0("items")
            .unwrap();
        for item in items.try_iter().unwrap() {
            let item = item.unwrap().cast_into::<PyTuple>().unwrap();
            let name: String = item.get_item(0).unwrap().extract().unwrap();
            let module_name: String = item.get_item(1).unwrap().extract().unwrap();
            let target = importlib
                .call_method1("import_module", (module_name.as_str(),))
                .unwrap();
            assert!(
                target.getattr("main").unwrap().is_callable(),
                "{name} -> {module_name} has no main()"
            );
        }
    });
}

#[test]
fn doctor_is_reachable_through_the_entry_point() {
    let case = Case::new();
    Python::attach(|py| {
        let registry = module(py, "tooling.hooks.dispatch.registry");
        let healthy = py
            .import("builtins")
            .unwrap()
            .getattr("dict")
            .unwrap()
            .call1((registry.getattr("settings_block").unwrap().call0().unwrap(),))
            .unwrap()
            .cast_into::<PyDict>()
            .unwrap();
        let env = PyDict::new(py);
        env.set_item("BASH_QUIET_LIMIT_BYTES", "8000").unwrap();
        healthy.set_item("env", env).unwrap();
        healthy.set_item("subagentPromptCacheTtl", "1h").unwrap();
        let settings: String = py
            .import("json")
            .unwrap()
            .getattr("dumps")
            .unwrap()
            .call1((healthy,))
            .unwrap()
            .extract()
            .unwrap();
        case.write(".claude/settings.json", &format!("{settings}\n"));
        let entry = module(py, "conductor.__main__");
        let root = case.root().to_str().unwrap();
        let home = case.root().join("home");
        assert_eq!(
            exit_code(
                &entry,
                vec![
                    "doctor",
                    "--harness",
                    "--project-dir",
                    root,
                    "--home",
                    home.to_str().unwrap()
                ]
            ),
            0
        );
    });
}

#[test]
fn bootstrap_main_delegates_argv_to_project_init_main() {
    let _case = Case::new();
    Python::attach(|py| {
        let bootstrap = module(py, "conductor.bootstrap");
        let calls = Arc::new(Mutex::new(Vec::<Vec<String>>::new()));
        let observed = Arc::clone(&calls);
        let stub = PyCFunction::new_closure(
            py,
            None,
            None,
            move |args: &Bound<'_, PyTuple>, _kwargs| -> PyResult<i32> {
                let argv = args.get_item(0)?.extract::<Vec<String>>()?;
                observed.lock().unwrap().push(argv);
                Ok(0)
            },
        )
        .unwrap();
        let _patch = AttrPatch::replace(&bootstrap, "_init_main", stub.as_any());
        assert_eq!(exit_code(&bootstrap, vec!["/some/host", "--force"]), 0);
        assert_eq!(
            *calls.lock().unwrap(),
            vec![vec!["/some/host".to_owned(), "--force".to_owned()]]
        );
    });
}

#[test]
fn bootstrap_main_returns_project_init_exit_code() {
    let _case = Case::new();
    Python::attach(|py| {
        let bootstrap = module(py, "conductor.bootstrap");
        let stub = PyCFunction::new_closure(py, None, None, |_, _| Ok::<i32, PyErr>(2)).unwrap();
        let _patch = AttrPatch::replace(&bootstrap, "_init_main", stub.as_any());
        assert_eq!(exit_code(&bootstrap, vec![]), 2);
    });
}

#[test]
fn bootstrap_subcommand_is_wired_in_conductor_main() {
    let _case = Case::new();
    Python::attach(|py| {
        let entry = module(py, "conductor.__main__");
        let target = entry
            .getattr("SUBCOMMANDS")
            .unwrap()
            .get_item("bootstrap")
            .unwrap();
        assert_eq!(text(&target), "conductor.bootstrap");
    });
}

#[test]
fn bootstrap_cli_scaffolds_a_real_working_dispatcher() {
    let case = Case::new();
    case.mkdir("proj/.git");
    Python::attach(|py| {
        let bootstrap = module(py, "conductor.bootstrap");
        let project = case.root().join("proj");
        assert_eq!(exit_code(&bootstrap, vec![project.to_str().unwrap()]), 0);
        let init = module(py, "conductor.project_init");
        let launcher_relative: String = init.getattr("LAUNCHER").unwrap().extract().unwrap();
        let launcher = project.join(launcher_relative);
        assert_ne!(
            fs::metadata(&launcher).unwrap().permissions().mode() & 0o100,
            0
        );
        let contents = fs::read_to_string(launcher).unwrap();
        let executable: String = py
            .import("sys")
            .unwrap()
            .getattr("executable")
            .unwrap()
            .extract()
            .unwrap();
        assert!(contents.starts_with(&format!("#!{executable}\n")));
        assert!(contents.contains("from tooling.hooks.dispatch.__main__ import main"));
    });
}
