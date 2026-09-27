#![cfg(feature = "python-compat-tests")]
//! Rust-owned parity for test_worktree_runtime.py (three original cases).

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::prelude::*;
use pyo3::types::PyList;
use std::fs;
use std::os::unix::fs::{symlink, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use support::{assert_error, module, path, Case};

fn venvs(case: &Case, name: &str) -> (PathBuf, PathBuf) {
    let source = case.mkdir(&format!("{name}/source-venv/bin"));
    let destination = case.mkdir(&format!("{name}/destination-venv/bin"));
    fs::write(source.join("python3"), "interpreter placeholder\n").unwrap();
    fs::write(destination.join("python3"), "interpreter placeholder\n").unwrap();
    (
        source.parent().unwrap().to_path_buf(),
        destination.parent().unwrap().to_path_buf(),
    )
}

fn linked_script(source: &Path, destination: &Path, name: &str, body: &[u8]) -> (PathBuf, PathBuf) {
    let left = source.join("bin").join(name);
    let right = destination.join("bin").join(name);
    fs::write(&left, body).unwrap();
    fs::set_permissions(&left, fs::Permissions::from_mode(0o751)).unwrap();
    fs::hard_link(&left, &right).unwrap();
    (left, right)
}

fn relocate<'py>(py: Python<'py>, source: &Path, destination: &Path) -> Bound<'py, PyList> {
    module(py, "conductor.worktree_runtime")
        .getattr("relocate_console_scripts")
        .unwrap()
        .call1((path(py, source), path(py, destination)))
        .unwrap()
        .cast_into::<PyList>()
        .unwrap()
}

fn relocated_one(py: Python<'_>, source: &Path, destination: &Path, entry: &Path) {
    let result = relocate(py, source, destination);
    assert_eq!(result.len(), 1);
    assert!(result.get_item(0).unwrap().eq(path(py, entry)).unwrap());
}

fn private_entrypoint_preserves_source_and_destination_mode(case: &Case) {
    let (source, destination) = venvs(case, "private-mode");
    let original = format!(
        "#!{}/bin/python3\nfrom package import main\n",
        source.display()
    );
    let (left, right) = linked_script(&source, &destination, "pytest", original.as_bytes());
    let original_inode = fs::metadata(&left).unwrap().ino();
    Python::attach(|py| relocated_one(py, &source, &destination, &right));
    assert_eq!(fs::metadata(&left).unwrap().ino(), original_inode);
    assert_eq!(fs::read(&left).unwrap(), original.as_bytes());
    assert_ne!(fs::metadata(&right).unwrap().ino(), original_inode);
    assert_eq!(
        fs::read(&right).unwrap(),
        format!(
            "#!{}/bin/python3\nfrom package import main\n",
            destination.display()
        )
        .as_bytes()
    );
    assert_eq!(
        fs::metadata(&right).unwrap().permissions().mode() & 0o777,
        0o751
    );
    let mode_left = source.join("bin/mode-check");
    let mode_right = destination.join("bin/mode-check");
    fs::write(&mode_left, &original).unwrap();
    fs::set_permissions(&mode_left, fs::Permissions::from_mode(0o751)).unwrap();
    fs::write(&mode_right, &original).unwrap();
    fs::set_permissions(&mode_right, fs::Permissions::from_mode(0o711)).unwrap();
    Python::attach(|py| {
        relocate(py, &source, &destination);
    });
    assert_eq!(
        fs::metadata(&mode_right).unwrap().permissions().mode() & 0o777,
        0o711
    );
}

fn skipped_entries_do_not_stop_relocation(case: &Case) {
    let (source, destination) = venvs(case, "skipped-first");
    let payload = format!("#!{}/bin/python3\nrun()\n", source.display());
    symlink("python3", source.join("bin/source-link")).unwrap();
    fs::write(destination.join("bin/source-link"), &payload).unwrap();
    fs::write(source.join("bin/destination-link"), &payload).unwrap();
    symlink("python3", destination.join("bin/destination-link")).unwrap();
    fs::create_dir(source.join("bin/source-directory")).unwrap();
    fs::write(destination.join("bin/source-directory"), &payload).unwrap();
    fs::write(source.join("bin/destination-directory"), &payload).unwrap();
    fs::create_dir(destination.join("bin/destination-directory")).unwrap();
    let (_, right) = linked_script(&source, &destination, "pytest", payload.as_bytes());
    Python::attach(|py| relocated_one(py, &source, &destination, &right));
    assert!(fs::read(&right)
        .unwrap()
        .starts_with(format!("#!{}/bin/python3\n", destination.display()).as_bytes()));
}

#[test]
fn relocation_is_idempotent_and_leaves_non_python_or_nonregular_entries_linked() {
    let case = Case::new();
    private_entrypoint_preserves_source_and_destination_mode(&case);
    skipped_entries_do_not_stop_relocation(&case);
    let (source, destination) = venvs(&case, "main");
    let original = format!("#!{}/bin/python3\nrun()\n", source.display());
    let (_, entry) = linked_script(&source, &destination, "pytest", original.as_bytes());
    let (foreign_left, foreign_right) = linked_script(
        &source,
        &destination,
        "foreign",
        b"#!/usr/bin/env python3\nrun()\n",
    );
    let (binary_left, binary_right) =
        linked_script(&source, &destination, "binary", b"\x7fELF\0not a script");
    for bin in [&source, &destination] {
        symlink("pytest", bin.join("bin/linked")).unwrap();
    }
    symlink("python3", source.join("bin/source-link")).unwrap();
    fs::write(destination.join("bin/source-link"), &original).unwrap();
    fs::write(source.join("bin/destination-link"), &original).unwrap();
    symlink("python3", destination.join("bin/destination-link")).unwrap();
    fs::create_dir(source.join("bin/source-directory")).unwrap();
    fs::write(destination.join("bin/source-directory"), &original).unwrap();
    fs::write(source.join("bin/destination-directory"), &original).unwrap();
    fs::create_dir(destination.join("bin/destination-directory")).unwrap();
    Python::attach(|py| {
        relocated_one(py, &source, &destination, &entry);
        let once = fs::read(&entry).unwrap();
        assert_eq!(relocate(py, &source, &destination).len(), 0);
        assert_eq!(fs::read(&entry).unwrap(), once);
    });
    assert_eq!(
        fs::metadata(foreign_left).unwrap().ino(),
        fs::metadata(foreign_right).unwrap().ino()
    );
    assert_eq!(
        fs::metadata(binary_left).unwrap().ino(),
        fs::metadata(binary_right).unwrap().ino()
    );
    assert!(fs::symlink_metadata(destination.join("bin/linked"))
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(
        fs::read(destination.join("bin/source-link")).unwrap(),
        original.as_bytes()
    );
    assert!(
        fs::symlink_metadata(destination.join("bin/destination-link"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        fs::read(destination.join("bin/source-directory")).unwrap(),
        original.as_bytes()
    );
    assert!(destination.join("bin/destination-directory").is_dir());
}

#[test]
fn relocation_refuses_same_or_incomplete_venv() {
    let case = Case::new();
    let (source, destination) = venvs(&case, "main");
    Python::attach(|py| {
        let api = module(py, "conductor.worktree_runtime");
        let relocate = api.getattr("relocate_console_scripts").unwrap();
        let error = api.getattr("WorktreeRuntimeError").unwrap();
        assert_error(
            py,
            relocate
                .call1((path(py, &source), path(py, &source)))
                .unwrap_err(),
            &error,
            "must differ",
        );
        fs::remove_file(destination.join("bin/python3")).unwrap();
        fs::remove_dir(destination.join("bin")).unwrap();
        assert_error(
            py,
            relocate
                .call1((path(py, &source), path(py, &destination)))
                .unwrap_err(),
            &error,
            "bin directory",
        );
    });
}

#[test]
fn relocation_refuses_destination_bin_symlink_without_touching_source() {
    let case = Case::new();
    let (source, destination) = venvs(&case, "main");
    let entry = source.join("bin/pytest");
    let body = format!("#!{}/bin/python3\nrun()\n", source.display());
    fs::write(&entry, &body).unwrap();
    let inode = fs::metadata(&entry).unwrap().ino();
    fs::remove_file(destination.join("bin/python3")).unwrap();
    fs::remove_dir(destination.join("bin")).unwrap();
    symlink(source.join("bin"), destination.join("bin")).unwrap();
    Python::attach(|py| {
        let api = module(py, "conductor.worktree_runtime");
        assert_error(
            py,
            api.getattr("relocate_console_scripts")
                .unwrap()
                .call1((path(py, &source), path(py, &destination)))
                .unwrap_err(),
            &api.getattr("WorktreeRuntimeError").unwrap(),
            "must not be symlinks",
        );
    });
    assert_eq!(fs::metadata(&entry).unwrap().ino(), inode);
    assert_eq!(fs::read_to_string(&entry).unwrap(), body);
}
