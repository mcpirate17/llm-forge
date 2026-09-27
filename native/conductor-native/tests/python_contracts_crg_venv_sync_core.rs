#![cfg(feature = "python-compat-tests")]
//! Rust-owned PyO3 contracts for CRG interpreter sync and repair.

#[path = "python_contracts/agent_comm_support.rs"]
#[allow(dead_code)]
mod comm_support;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;
#[path = "python_contracts/crg_venv_sync_support.rs"]
#[allow(dead_code)]
mod venv_support;

use comm_support::{bind_signature, buffer_text, capture, signature};
use pyo3::prelude::*;
use pyo3::types::{PyCFunction, PyTuple};
use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};
use support::{module, path, AttrPatch, Case};
use venv_support::{
    clear_import_error, consumer, consumer_venv, crate_dir, declare_server, declared_demo,
    git_origin, installed, interpreter, packages_of, probe_error, set_import_error, sync, tree,
    verdict, VERSION,
};

fn has(lines: &[String], part: &str) {
    assert!(
        lines.iter().any(|line| line.contains(part)),
        "missing {part:?} in {lines:?}"
    );
}

#[test]
fn matching_version_and_a_clean_import_passes() {
    let mut case = Case::new();
    tree(&mut case);
    installed(&packages_of(case.root()), "conductor-native", VERSION);
    Python::attach(|py| {
        let (verdict, detail) = verdict(py, case.root(), true);
        assert_eq!(verdict, "PASS");
        assert!(!detail.is_empty());
    });
}

#[test]
fn version_mismatch_is_drift() {
    let mut case = Case::new();
    tree(&mut case);
    installed(&packages_of(case.root()), "conductor-native", "0.1.25");
    Python::attach(|py| {
        let (verdict, detail) = verdict(py, case.root(), true);
        assert_eq!(verdict, "FAIL");
        has(&detail, "0.1.25 installed, 0.1.30 declared");
    });
}

#[test]
fn absent_crate_is_not_drift_while_the_server_imports() {
    let mut case = Case::new();
    tree(&mut case);
    Python::attach(|py| {
        let (verdict, detail) = verdict(py, case.root(), true);
        assert_eq!(verdict, "PASS");
        has(&detail, "absent from the server's interpreter");
    });
}

#[test]
fn a_failing_import_is_drift_at_a_matching_version() {
    let mut case = Case::new();
    tree(&mut case);
    installed(&packages_of(case.root()), "conductor-native", VERSION);
    set_import_error(&mut case);
    Python::attach(|py| {
        let (verdict, detail) = verdict(py, case.root(), true);
        assert_eq!(verdict, "FAIL");
        has(&detail, "new_symbol_native");
    });
}

#[test]
fn check_only_never_installs() {
    let mut case = Case::new();
    tree(&mut case);
    installed(&packages_of(case.root()), "conductor-native", "0.1.25");
    Python::attach(|py| {
        let callback = PyCFunction::new_closure(py, None, None, |_args, _kwargs| -> PyResult<()> {
            panic!("--check must not install")
        })
        .unwrap();
        let _patch = AttrPatch::replace(sync(py).as_any(), "install", callback.as_any());
        assert_eq!(verdict(py, case.root(), true).0, "FAIL");
    });
}

#[test]
fn sync_repairs_the_mismatch_and_reports_what_it_installed() {
    let mut case = Case::new();
    tree(&mut case);
    let packages = packages_of(case.root());
    installed(&packages, "conductor-native", "0.1.25");
    set_import_error(&mut case);
    Python::attach(|py| {
        let expected = signature(py, &["_", "one"], &[]);
        let callback =
            PyCFunction::new_closure(py, None, None, move |args, kwargs| -> PyResult<()> {
                let bound = bind_signature(&expected, args, kwargs)?;
                let one = bound.getattr("arguments")?.get_item("one")?;
                let distribution: String = one.getattr("distribution")?.extract()?;
                let version: String = one.getattr("version")?.extract()?;
                clear_import_error(args.py());
                installed(&packages, &distribution, &version);
                Ok(())
            })
            .unwrap();
        let _patch = AttrPatch::replace(sync(py).as_any(), "install", callback.as_any());
        let (verdict, detail) = verdict(py, case.root(), false);
        assert_eq!(verdict, "SYNCED");
        has(&detail, "reinstalled conductor-native");
    });
}

#[test]
fn absent_crates_are_installed_only_after_the_mismatches_fail() {
    let mut case = Case::new();
    tree(&mut case);
    crate_dir(case.root(), "slop-core", "0.1.6");
    installed(&packages_of(case.root()), "conductor-native", "0.1.25");
    set_import_error(&mut case);
    Python::attach(|py| {
        let order = Arc::new(Mutex::new(Vec::<String>::new()));
        let recorded = Arc::clone(&order);
        let expected = signature(py, &["_", "one"], &[]);
        let callback =
            PyCFunction::new_closure(py, None, None, move |args, kwargs| -> PyResult<()> {
                let bound = bind_signature(&expected, args, kwargs)?;
                let name: String = bound
                    .getattr("arguments")?
                    .get_item("one")?
                    .getattr("distribution")?
                    .extract()?;
                let length = {
                    let mut order = recorded.lock().unwrap();
                    order.push(name);
                    order.len()
                };
                if length == 2 {
                    clear_import_error(args.py());
                }
                Ok(())
            })
            .unwrap();
        let _patch = AttrPatch::replace(sync(py).as_any(), "install", callback.as_any());
        assert_eq!(verdict(py, case.root(), false).0, "SYNCED");
        assert_eq!(*order.lock().unwrap(), ["conductor-native", "slop-core"]);
    });
}

#[test]
fn an_import_that_stays_broken_fails_loud() {
    let mut case = Case::new();
    tree(&mut case);
    installed(&packages_of(case.root()), "conductor-native", "0.1.25");
    set_import_error(&mut case);
    Python::attach(|py| {
        let callback = PyCFunction::new_closure(py, None, None, |_args, kwargs| -> PyResult<()> {
            assert!(kwargs.is_none());
            Ok(())
        })
        .unwrap();
        let _patch = AttrPatch::replace(sync(py).as_any(), "install", callback.as_any());
        let (verdict, detail) = verdict(py, case.root(), false);
        assert_eq!(verdict, "FAIL");
        has(&detail, "still broken");
    });
}

#[test]
fn a_missing_interpreter_is_a_skip() {
    let case = Case::new();
    crate_dir(case.root(), "conductor-native", VERSION);
    declare_server(case.root(), &case.root().join("nowhere/python"));
    Python::attach(|py| {
        let (verdict, detail) = verdict(py, case.root(), true);
        assert_eq!(verdict, "SKIP");
        has(&detail, "does not exist");
    });
}

#[test]
fn the_checkouts_own_venv_is_a_skip() {
    let mut case = Case::new();
    tree(&mut case);
    let own = case.root().join(".venv/lib/python3.12/site-packages");
    fs::create_dir_all(&own).unwrap();
    case.set_env("FAKE_PURELIB", own.to_str().unwrap());
    Python::attach(|py| {
        let (verdict, detail) = verdict(py, case.root(), true);
        assert_eq!(verdict, "SKIP");
        has(&detail, "uv sync");
    });
}

#[test]
fn a_tree_with_neither_sources_nor_an_install_is_a_skip() {
    let case = Case::new();
    declare_server(case.root(), &interpreter(case.root(), "fake-python"));
    Python::attach(|py| {
        let (verdict, detail) = verdict(py, case.root(), true);
        assert_eq!(verdict, "SKIP");
        has(&detail, "no extension crates");
        assert!(detail
            .iter()
            .any(|line| line.contains("tooling/native") && line.contains(".venv")));
    });
}

#[test]
fn a_consumer_checkout_is_compared_against_what_it_installed() {
    let mut case = Case::new();
    let _declared = consumer(&mut case);
    installed(&packages_of(case.root()), "demo-native", "0.1.25");
    Python::attach(|py| {
        let (verdict, detail) = verdict(py, case.root(), true);
        assert_eq!(verdict, "FAIL");
        has(&detail, "0.1.25 installed, 0.1.30 declared");
    });
}

#[test]
fn a_consumer_repair_installs_the_reference_not_a_directory() {
    let mut case = Case::new();
    let _declared = consumer(&mut case);
    installed(&packages_of(case.root()), "demo-native", "0.1.25");
    set_import_error(&mut case);
    Python::attach(|py| {
        let sources = Arc::new(Mutex::new(Vec::<String>::new()));
        let recorded = Arc::clone(&sources);
        let expected = signature(py, &["_", "one"], &[]);
        let callback =
            PyCFunction::new_closure(py, None, None, move |args, kwargs| -> PyResult<()> {
                let bound = bind_signature(&expected, args, kwargs)?;
                let source: String = bound
                    .getattr("arguments")?
                    .get_item("one")?
                    .getattr("source")?
                    .extract()?;
                recorded.lock().unwrap().push(source);
                clear_import_error(args.py());
                Ok(())
            })
            .unwrap();
        let _install = AttrPatch::replace(sync(py).as_any(), "install", callback.as_any());
        assert_eq!(verdict(py, case.root(), false).0, "SYNCED");
        assert_eq!(*sources.lock().unwrap(), [
            "demo-native @ git+https://github.com/example/forge@c0ffee#subdirectory=native/demo-native"
        ]);
    });
}

#[test]
fn an_index_installed_requirement_is_not_a_crate() {
    let case = Case::new();
    consumer_venv(case.root(), "demo-native", VERSION, None, true);
    Python::attach(|py| {
        let _declared = declared_demo(py);
        let required = sync(py)
            .getattr("required")
            .unwrap()
            .call1((path(py, case.root()),))
            .unwrap();
        assert!(required.eq(PyTuple::empty(py)).unwrap());
    });
}

#[test]
fn a_pure_python_requirement_is_not_a_crate() {
    let case = Case::new();
    consumer_venv(
        case.root(),
        "demo-native",
        VERSION,
        Some(git_origin()),
        false,
    );
    Python::attach(|py| {
        let _declared = declared_demo(py);
        let required = sync(py)
            .getattr("required")
            .unwrap()
            .call1((path(py, case.root()),))
            .unwrap();
        assert!(required.eq(PyTuple::empty(py)).unwrap());
    });
}

#[test]
fn crate_sources_in_the_tree_win_over_what_is_installed() {
    let case = Case::new();
    let directory = crate_dir(case.root(), "demo-native", "0.2.0");
    consumer_venv(
        case.root(),
        "demo-native",
        VERSION,
        Some(git_origin()),
        true,
    );
    Python::attach(|py| {
        let _declared = declared_demo(py);
        let sync = sync(py);
        let required = sync
            .getattr("required")
            .unwrap()
            .call1((path(py, case.root()),))
            .unwrap();
        let expected = sync
            .getattr("Requirement")
            .unwrap()
            .call1(("demo-native", "0.2.0", directory.to_str().unwrap()))
            .unwrap();
        assert!(required.eq(PyTuple::new(py, [expected]).unwrap()).unwrap());
    });
}

#[test]
fn the_requirement_names_come_from_this_packages_own_metadata() {
    let _case = Case::new();
    Python::attach(|py| {
        let names = sync(py).getattr("declared_names").unwrap().call0().unwrap();
        assert!(names.cast::<PyTuple>().is_ok());
        let names: Vec<String> = names.extract().unwrap();
        assert!(names.contains(&"conductor-native".to_owned()));
        assert!(!names
            .iter()
            .any(|name| name.starts_with("code-review-graph")));
    });
}

#[test]
fn an_undeclared_server_fails_loud() {
    let case = Case::new();
    Python::attach(|py| {
        probe_error(
            py,
            sync(py)
                .getattr("run")
                .unwrap()
                .call1((path(py, case.root()), true))
                .unwrap_err(),
        );
    });
}

#[test]
fn an_interpreter_that_cannot_report_site_packages_fails_loud() {
    let mut case = Case::new();
    tree(&mut case);
    let broken = interpreter(case.root(), "broken-python");
    Python::attach(|py| {
        probe_error(
            py,
            sync(py)
                .getattr("purelib")
                .unwrap()
                .call1((path(py, &broken),))
                .unwrap_err(),
        );
    });
}

fn main_args(root: &Path) -> Vec<String> {
    vec![
        "--repo".to_owned(),
        root.to_str().unwrap().to_owned(),
        "--check".to_owned(),
    ]
}

#[test]
fn main_prints_the_verdict_and_exits_nonzero_on_drift() {
    let mut case = Case::new();
    tree(&mut case);
    installed(&packages_of(case.root()), "conductor-native", "0.1.25");
    Python::attach(|py| {
        let (out, _stdout) = capture(py, "stdout");
        let code: i32 = sync(py)
            .getattr("main")
            .unwrap()
            .call1((main_args(case.root()),))
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(code, 1);
        assert!(buffer_text(&out).contains("crg-venv-sync | FAIL"));
    });
}

#[test]
fn main_reports_a_probe_error_as_a_failure() {
    let case = Case::new();
    Python::attach(|py| {
        let (out, _stdout) = capture(py, "stdout");
        let code: i32 = sync(py)
            .getattr("main")
            .unwrap()
            .call1((main_args(case.root()),))
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(code, 1);
        assert!(buffer_text(&out).contains("crg-venv-sync | FAIL |"));
    });
}

#[test]
fn uv_env_drops_the_inherited_virtualenv() {
    let mut case = Case::new();
    case.set_env("VIRTUAL_ENV", "/somewhere/else");
    let path_value = std::env::var("PATH").unwrap();
    case.set_env("PATH", &path_value);
    Python::attach(|py| {
        let env = sync(py).getattr("uv_env").unwrap().call0().unwrap();
        assert!(!env.contains("VIRTUAL_ENV").unwrap());
        let original = module(py, "os")
            .getattr("environ")
            .unwrap()
            .get_item("PATH")
            .unwrap();
        assert!(env.get_item("PATH").unwrap().eq(original).unwrap());
    });
}
