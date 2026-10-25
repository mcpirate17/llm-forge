//! Shared fixture for the contracts that a duplicate-audit tool scans the host's `source_dirs`.
#![allow(dead_code)]

use super::support::{module, AttrPatch, Case};
use pyo3::prelude::*;
use pyo3::types::PyModule;
use std::path::PathBuf;
use std::process::Command;

pub const HOST_PYPROJECT: &str =
    "[tool.forge.duplicate_audit]\nsource_dirs = [\"pkg\", \"lib/pkg\"]\n";

/// A git repo whose pyproject names `pkg` and `lib/pkg` and which also holds `research/`.
pub fn host(case: &Case) -> PathBuf {
    let root = case.mkdir("host");
    case.write("host/pyproject.toml", HOST_PYPROJECT);
    case.write("host/pkg/a.py", "x = 1\n");
    case.write("host/lib/pkg/b.py", "y = 2\n");
    case.write("host/research/c.py", "z = 3\n");
    let status = Command::new("git")
        .args(["init", "-q"])
        .current_dir(&root)
        .status()
        .unwrap();
    assert!(status.success());
    root
}

/// Replace `scan` in `module_name` with a function that raises `Seen(targets)`, run
/// `main` with no `--targets`, and return the folders it was about to scan.
pub fn targets_main_scans(
    py: Python<'_>,
    module_name: &str,
    scan: &str,
    root: &PathBuf,
) -> Vec<String> {
    let probe = PyModule::from_code(
        py,
        c"class Seen(Exception):\n    pass\n\n\ndef fake(_repo, targets, *_args, **_kwargs):\n    raise Seen(list(targets))\n",
        c"source_dirs_probe.py",
        c"source_dirs_probe",
    )
    .unwrap();
    let tool = module(py, module_name);
    let _patch = AttrPatch::replace(&tool, scan, &probe.getattr("fake").unwrap());
    let argv = vec!["--repo".to_owned(), root.to_string_lossy().into_owned()];
    let error = tool.getattr("main").unwrap().call1((argv,)).unwrap_err();
    assert!(error.matches(py, probe.getattr("Seen").unwrap()).unwrap());
    error
        .value(py)
        .getattr("args")
        .unwrap()
        .get_item(0)
        .unwrap()
        .extract()
        .unwrap()
}
