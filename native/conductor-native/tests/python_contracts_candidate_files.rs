#![cfg(feature = "python-compat-tests")]
//! Retained duplicate-audit source_dirs contracts; review command expansion removed.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

#[path = "python_contracts/source_dirs_support.rs"]
mod source_dirs_support;

use pyo3::prelude::*;
use pyo3::types::{PyCFunction, PyDict, PyModule};
use std::ffi::CStr;
use std::fs;
use std::path::Path;
use support::{module, path, AttrPatch, Case};

const RUN_PROBE: &CStr = c"import subprocess\n\n\ncalls = []\ncodes = []\n\n\ndef run(cmd, **kwargs):\n    calls.append((list(cmd), str(kwargs['cwd'])))\n    return subprocess.CompletedProcess(cmd, codes.pop(0) if codes else 0)\n";

fn probe(py: Python<'_>, codes: Vec<i32>) -> Bound<'_, PyModule> {
    let probe = PyModule::from_code(py, RUN_PROBE, c"run_probe.py", c"run_probe").unwrap();
    probe.setattr("codes", codes).unwrap();
    probe
}
fn call_audit<'py>(
    py: Python<'py>,
    entry: &str,
    root: &Path,
    check: bool,
    codes: Vec<i32>,
) -> Bound<'py, PyModule> {
    let audit = module(py, "conductor.run_duplicate_audit");
    let probe = probe(py, codes);
    let find = PyCFunction::new_closure(py, None, None, |_args, _kwargs| "/fake/nicad").unwrap();
    let _command = AttrPatch::replace(&audit, "command_path", &find);
    let _run = AttrPatch::replace(
        &audit.getattr("subprocess").unwrap(),
        "run",
        &probe.getattr("run").unwrap(),
    );
    let kwargs = PyDict::new(py);
    kwargs.set_item("check", check).unwrap();
    kwargs.set_item("root", path(py, root)).unwrap();
    let code: i32 = audit
        .getattr(entry)
        .unwrap()
        .call((), Some(&kwargs))
        .unwrap()
        .extract()
        .unwrap();
    probe.setattr("code", code).unwrap();
    probe
}
#[test]
fn pylint_file_list_comes_from_pyproject_source_dirs() {
    let case = Case::new();
    let root = source_dirs_support::host(&case);
    Python::attach(|py| {
        let probe = call_audit(py, "run_pylint", &root, true, vec![]);
        assert_eq!(probe.getattr("code").unwrap().extract::<i32>().unwrap(), 0);
    });
    let listed = fs::read_to_string(root.join("tasks/audit/duplication-python-files.txt")).unwrap();
    assert!(
        listed.contains("a.py") && listed.contains("b.py"),
        "{listed}"
    );
    assert!(
        !listed.contains("c.py") && !listed.contains("research"),
        "{listed}"
    );
}
#[test]
fn nicad_runs_once_per_source_dir_in_its_own_output_directory_and_returns_the_worst_exit() {
    let case = Case::new();
    let root = source_dirs_support::host(&case);
    Python::attach(|py| {
        let probe = call_audit(py, "run_nicad_python", &root, true, vec![0, 3]);
        assert_eq!(probe.getattr("code").unwrap().extract::<i32>().unwrap(), 3);
        let calls: Vec<(Vec<String>, String)> = probe.getattr("calls").unwrap().extract().unwrap();
        let scanned: Vec<&str> = calls.iter().map(|(cmd, _)| cmd[3].as_str()).collect();
        assert_eq!(
            scanned,
            [
                root.join("pkg").to_str().unwrap(),
                root.join("lib/pkg").to_str().unwrap()
            ]
        );
        assert!(calls
            .iter()
            .all(|(cmd, _)| cmd[..3] == ["/fake/nicad", "functions", "py"]));
        // `pkg` and `lib/pkg` share a basename, and NiCad keys its results on it.
        let nicad = root.join("tasks/audit/nicad");
        let cwds: Vec<&str> = calls.iter().map(|(_, cwd)| cwd.as_str()).collect();
        assert_eq!(
            cwds,
            [
                nicad.join("pkg").to_str().unwrap(),
                nicad.join("lib__pkg").to_str().unwrap()
            ]
        );
        assert!(nicad.join("pkg").is_dir() && nicad.join("lib__pkg").is_dir());
        let probe = call_audit(py, "run_nicad_python", &root, false, vec![3, 4]);
        assert_eq!(probe.getattr("code").unwrap().extract::<i32>().unwrap(), 0);
        assert_eq!(probe.getattr("calls").unwrap().len().unwrap(), 2);
    });
}
