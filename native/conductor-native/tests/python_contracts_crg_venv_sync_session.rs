#![cfg(feature = "python-compat-tests")]
//! Rust-owned PyO3 contracts for CRG venv-sync session-start findings.

#[path = "python_contracts/agent_comm_support.rs"]
#[allow(dead_code)]
mod comm_support;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;
#[path = "python_contracts/crg_venv_sync_support.rs"]
#[allow(dead_code)]
mod venv_support;

use pyo3::prelude::*;
use pyo3::types::{PyAny, PyCFunction, PyList, PyTuple};
use support::{path, AttrPatch, Case};
use venv_support::{crate_dir, installed, packages_of, set_import_error, sync, tree};

fn findings(py: Python<'_>, root: &std::path::Path) -> Vec<String> {
    let output = sync(py)
        .getattr("session_findings")
        .unwrap()
        .call1((path(py, root),))
        .unwrap();
    assert!(output.cast::<PyTuple>().is_ok());
    output.extract().unwrap()
}

#[test]
fn a_checkout_in_step_is_silent_at_session_start() {
    let mut case = Case::new();
    tree(&mut case);
    installed(&packages_of(case.root()), "conductor-native", "0.1.30");
    Python::attach(|py| {
        assert!(sync(py)
            .getattr("session_report")
            .unwrap()
            .call1((path(py, case.root()),))
            .unwrap()
            .eq("")
            .unwrap());
    });
}

#[test]
fn a_version_skew_names_the_crate_and_the_remedy() {
    let mut case = Case::new();
    tree(&mut case);
    installed(&packages_of(case.root()), "conductor-native", "0.1.25");
    Python::attach(|py| {
        let report: String = sync(py)
            .getattr("session_report")
            .unwrap()
            .call1((path(py, case.root()),))
            .unwrap()
            .extract()
            .unwrap();
        assert!(report.starts_with("GRAPH SERVER natives out of date in "));
        assert!(report
            .lines()
            .any(|line| line.starts_with("- conductor-native:")
                && line.ends_with("; `make crg-sync`")));
    });
}

#[test]
fn a_skew_the_server_still_survives_says_so() {
    let mut case = Case::new();
    tree(&mut case);
    installed(&packages_of(case.root()), "conductor-native", "0.1.25");
    Python::attach(|py| {
        assert!(findings(py, case.root())
            .iter()
            .any(|line| line.contains("still imports")));
    });
}

#[test]
fn a_skew_that_killed_the_server_names_connection_closed() {
    let mut case = Case::new();
    tree(&mut case);
    installed(&packages_of(case.root()), "conductor-native", "0.1.25");
    set_import_error(&mut case);
    Python::attach(|py| {
        let lines = findings(py, case.root());
        assert!(lines.iter().any(|line| line.contains("CONNECTION_CLOSED")));
        assert!(lines.iter().any(|line| line.contains("new_symbol_native")));
    });
}

#[test]
fn an_absent_crate_never_buys_the_import_probe() {
    let mut case = Case::new();
    tree(&mut case);
    crate_dir(case.root(), "slop-core", "0.1.6");
    installed(&packages_of(case.root()), "conductor-native", "0.1.30");
    Python::attach(|py| {
        let callback = PyCFunction::new_closure(py, None, None, |_args, _kwargs| -> PyResult<()> {
            panic!("probed for an absent crate")
        })
        .unwrap();
        let _patch = AttrPatch::replace(sync(py).as_any(), "imports_server", callback.as_any());
        assert!(sync(py)
            .getattr("session_findings")
            .unwrap()
            .call1((path(py, case.root()),))
            .unwrap()
            .eq(PyTuple::empty(py))
            .unwrap());
    });
}

#[test]
fn an_undeclared_server_is_silent_at_session_start() {
    let case = Case::new();
    Python::attach(|py| {
        assert!(sync(py)
            .getattr("session_findings")
            .unwrap()
            .call1((path(py, case.root()),))
            .unwrap()
            .eq(PyTuple::empty(py))
            .unwrap());
    });
}

#[test]
fn the_session_probes_never_wait_the_install_timeout() {
    let mut case = Case::new();
    tree(&mut case);
    installed(&packages_of(case.root()), "conductor-native", "0.1.25");
    Python::attach(|py| {
        let sync = sync(py);
        let subprocess = sync.getattr("subprocess").unwrap();
        let spawn = subprocess.getattr("run").unwrap().unbind();
        let waits = PyList::empty(py);
        let recorded = waits.clone().unbind();
        let callback =
            PyCFunction::new_closure(py, None, None, move |args, kwargs| -> PyResult<Py<PyAny>> {
                let timeout = kwargs
                    .expect("probe must supply timeout")
                    .get_item("timeout")?
                    .expect("timeout key");
                recorded.bind(args.py()).append(timeout)?;
                Ok(spawn.bind(args.py()).call(args, kwargs)?.unbind())
            })
            .unwrap();
        let _patch = AttrPatch::replace(&subprocess, "run", callback.as_any());
        sync.getattr("session_findings")
            .unwrap()
            .call1((path(py, case.root()),))
            .unwrap();
        let session: i32 = sync
            .getattr("SESSION_TIMEOUT_SECONDS")
            .unwrap()
            .extract()
            .unwrap();
        let install: i32 = sync.getattr("TIMEOUT_SECONDS").unwrap().extract().unwrap();
        assert!(session < install);
        assert!(waits
            .eq(PyList::new(py, [session, session]).unwrap())
            .unwrap());
    });
}
