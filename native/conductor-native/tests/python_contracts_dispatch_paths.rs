#![cfg(feature = "python-compat-tests")]
//! Hook-body resolution and real venv symlink fixtures, asserted in Rust.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::prelude::*;
use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use support::{module, path, AttrPatch, Case};

const RELATIVE: &str = "tooling/hooks/agent/crg_gate.py";

fn body<'py>(py: Python<'py>, root: &Path) -> Bound<'py, PyAny> {
    module(py, "tooling.hooks.dispatch.paths")
        .getattr("body_path")
        .unwrap()
        .call1((path(py, root), RELATIVE))
        .unwrap()
}

fn fallback(py: Python<'_>) -> Bound<'_, PyAny> {
    module(py, "tooling.hooks.dispatch.paths")
        .getattr("TOOLING_ROOT")
        .unwrap()
        .call_method1("joinpath", (RELATIVE,))
        .unwrap()
}

fn base_interpreter(case: &Case) -> PathBuf {
    let base = case.write("usr/bin/python3", "#!/bin/sh\nexit 0\n");
    fs::set_permissions(&base, fs::Permissions::from_mode(0o755)).unwrap();
    base
}

fn venv(root: &Path, base: &Path) -> PathBuf {
    let bin = root.join(".venv/bin");
    fs::create_dir_all(&bin).unwrap();
    symlink(base, bin.join("python")).unwrap();
    symlink("python", bin.join("python3")).unwrap();
    bin.join("python")
}

fn own<'py>(py: Python<'py>, root: &Path, caller: &str) -> Bound<'py, PyAny> {
    module(py, "tooling.hooks.dispatch.paths")
        .getattr("own_interpreter")
        .unwrap()
        .call1((path(py, root), caller))
        .unwrap()
}

#[test]
fn the_projects_own_copy_wins() {
    let case = Case::new();
    let file = case.write(RELATIVE, "# project copy\n");
    Python::attach(|py| assert!(body(py, case.root()).eq(path(py, &file)).unwrap()));
}

#[test]
fn the_installed_package_answers_when_the_project_has_none() {
    let case = Case::new();
    Python::attach(|py| {
        let found = body(py, case.root());
        assert!(found.eq(fallback(py)).unwrap());
        assert!(!found
            .call_method1("is_relative_to", (path(py, case.root()),))
            .unwrap()
            .is_truthy()
            .unwrap());
    });
}

#[test]
fn a_directory_is_not_a_body() {
    let case = Case::new();
    fs::create_dir_all(case.root().join(RELATIVE)).unwrap();
    Python::attach(|py| assert!(body(py, case.root()).eq(fallback(py)).unwrap()));
}

#[test]
fn the_tooling_root_holds_the_tooling_package() {
    let _case = Case::new();
    Python::attach(|py| {
        let root = module(py, "tooling.hooks.dispatch.paths")
            .getattr("TOOLING_ROOT")
            .unwrap();
        assert!(root
            .call_method1("joinpath", ("tooling", "hooks", "dispatch"))
            .unwrap()
            .call_method0("is_dir")
            .unwrap()
            .is_truthy()
            .unwrap());
    });
}

#[test]
fn the_interpreter_bin_does_not_follow_the_venv_symlink() {
    let case = Case::new();
    let base = case.write("usr/bin/python3.12", "");
    let bin = case.root().join("venv/bin");
    fs::create_dir_all(&bin).unwrap();
    let link = bin.join("python3");
    symlink(base, &link).unwrap();
    Python::attach(|py| {
        let executable = link.to_str().unwrap().into_pyobject(py).unwrap();
        let sys = module(py, "sys");
        let _executable = AttrPatch::replace(&sys, "executable", executable.as_any());
        let actual = module(py, "tooling.hooks.dispatch.paths")
            .getattr("interpreter_bin")
            .unwrap()
            .call0()
            .unwrap();
        assert!(actual.eq(bin.to_str().unwrap()).unwrap());
    });
}

#[test]
fn own_interpreter_names_each_checkouts_python_for_a_foreign_caller() {
    let case = Case::new();
    let base = base_interpreter(&case);
    let main = case.root().join("main");
    let worktree = case.root().join("worktree");
    let main_python = venv(&main, &base);
    let worktree_python = venv(&worktree, &base);
    Python::attach(|py| {
        assert!(own(py, &main, "/some/other/venv/bin/python")
            .eq(path(py, &main_python))
            .unwrap());
        assert!(own(py, &worktree, "/some/other/venv/bin/python")
            .eq(path(py, &worktree_python))
            .unwrap());
    });
}

#[test]
fn own_interpreter_is_none_when_the_caller_already_runs_it() {
    let case = Case::new();
    let root = case.root().join("checkout");
    let python = venv(&root, &base_interpreter(&case));
    Python::attach(|py| assert!(own(py, &root, python.to_str().unwrap()).is_none()));
}

#[test]
fn own_interpreter_compares_directories_not_resolved_interpreters() {
    let case = Case::new();
    let root = case.root().join("checkout");
    let base = base_interpreter(&case);
    let python = venv(&root, &base);
    assert_eq!(python.canonicalize().unwrap(), base);
    Python::attach(|py| {
        assert!(own(py, &root, base.to_str().unwrap())
            .eq(path(py, &python))
            .unwrap());
        let alias = python.parent().unwrap().join("python3");
        assert!(own(py, &root, alias.to_str().unwrap()).is_none());
    });
}

#[test]
fn own_interpreter_is_none_when_the_checkout_has_no_venv() {
    let case = Case::new();
    let root = case.root().join("checkout");
    fs::create_dir(&root).unwrap();
    Python::attach(|py| assert!(own(py, &root, "/some/other/venv/bin/python").is_none()));
}
