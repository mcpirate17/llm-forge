#![cfg(feature = "python-compat-tests")]
//! Rust-owned parity for test_snapshot_worktree.py (seven cases).

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;
#[path = "python_contracts/worktree_migration_support.rs"]
#[allow(dead_code)]
mod workspace;

use pyo3::prelude::*;
use pyo3::types::{PyCFunction, PyDict};
use std::fs;
use std::path::Path;
use std::process::Command;
use support::{assert_error, module, path, text, AttrPatch};
use workspace::{git, object_inventory, repository, SnapshotGuard, WorkspaceCase};

fn snapshot_path(snapshot: &Bound<'_, PyAny>) -> std::path::PathBuf {
    std::path::PathBuf::from(text(&snapshot.getattr("worktree").unwrap()))
}

fn included(snapshot: &Bound<'_, PyAny>) -> Vec<String> {
    snapshot
        .getattr("included_untracked")
        .unwrap()
        .extract()
        .unwrap()
}

fn assert_host_unchanged(repo: &Path, worktrees: &str, objects: &[String]) {
    assert_eq!(git(repo, &["worktree", "list", "--porcelain"]), worktrees);
    assert_eq!(object_inventory(repo), objects);
}

#[test]
fn snapshot_reproduces_dirty_tree_without_polluting_host_objects() {
    let case = WorkspaceCase::new();
    let repo = repository(&case);
    fs::write(repo.join("changed.py"), "after\n").unwrap();
    fs::remove_file(repo.join("deleted.py")).unwrap();
    fs::write(repo.join("added.py"), "new\n").unwrap();
    let receipt = repo.join("conductor/mutation_campaigns/receipts/running.json");
    fs::create_dir_all(receipt.parent().unwrap()).unwrap();
    fs::write(&receipt, "{\"status\":\"RUNNING\"}\n").unwrap();
    let objects = object_inventory(&repo);
    let worktrees = git(&repo, &["worktree", "list", "--porcelain"]);
    Python::attach(|py| {
        let (guard, snapshot) = SnapshotGuard::enter(py, &repo);
        let tree = snapshot_path(&snapshot);
        assert_eq!(
            fs::read_to_string(tree.join("changed.py")).unwrap(),
            "after\n"
        );
        assert!(!tree.join("deleted.py").exists());
        assert_eq!(fs::read_to_string(tree.join("added.py")).unwrap(), "new\n");
        assert!(included(&snapshot).contains(&"added.py".into()));
        assert!(!included(&snapshot)
            .contains(&"conductor/mutation_campaigns/receipts/running.json".into()));
        assert!(!tree.join(receipt.strip_prefix(&repo).unwrap()).exists());
        assert_host_unchanged(&repo, &worktrees, &objects);
        drop(guard);
    });
    assert_host_unchanged(&repo, &worktrees, &objects);
}

#[test]
fn fixture_trees_survive_snapshot_regardless_of_suffix() {
    let case = WorkspaceCase::new();
    let repo = repository(&case);
    let fixtures: &[(&str, &[u8])] = &[
        ("tests/ledger.db", b"\x00sqlite payload"),
        ("fixtures/notes.txt", b"plain text\n"),
        ("tests/runnable", b"#!/bin/sh\n"),
    ];
    for (relative, body) in fixtures {
        let file = repo.join(relative);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, body).unwrap();
    }
    fs::create_dir_all(repo.join("src")).unwrap();
    fs::write(repo.join("src/kept.py"), "kept = 1\n").unwrap();
    fs::create_dir_all(repo.join("data")).unwrap();
    fs::write(repo.join("data/dropped.bin"), b"\xff").unwrap();
    fs::write(
        repo.join("data/extensionless"),
        "no suffix, no fixture dir\n",
    )
    .unwrap();
    Python::attach(|py| {
        let (_guard, snapshot) = SnapshotGuard::enter(py, &repo);
        let tree = snapshot_path(&snapshot);
        let names = included(&snapshot);
        for (relative, body) in fixtures {
            assert!(names.contains(&relative.to_string()));
            assert_eq!(fs::read(tree.join(relative)).unwrap(), *body);
        }
        assert!(names.contains(&"src/kept.py".into()));
        assert!(!names.contains(&"data/dropped.bin".into()));
        assert!(!names.contains(&"data/extensionless".into()));
        assert!(!tree.join("data/dropped.bin").exists());
    });
}

#[test]
fn extra_snapshot_suffixes_are_configurable_outside_fixture_trees() {
    let case = WorkspaceCase::new();
    let repo = repository(&case);
    fs::write(
        repo.join("pyproject.toml"),
        "[tool.conductor]\nsnapshot_extra_suffixes = [\".db\", \"dat\"]\n",
    )
    .unwrap();
    fs::create_dir_all(repo.join("data")).unwrap();
    for relative in ["data/ledger.db", "data/series.dat"] {
        fs::write(repo.join(relative), b"payload").unwrap();
    }
    Python::attach(|py| {
        let (_guard, snapshot) = SnapshotGuard::enter(py, &repo);
        let names = included(&snapshot);
        for relative in ["data/ledger.db", "data/series.dat"] {
            assert!(names.contains(&relative.into()));
        }
        assert_eq!(
            fs::read(snapshot_path(&snapshot).join("data/ledger.db")).unwrap(),
            b"payload"
        );
    });
    fs::write(
        repo.join("pyproject.toml"),
        "[tool.conductor]\nsnapshot_extra_suffixes = \"db\"\n",
    )
    .unwrap();
    Python::attach(|py| {
        let api = module(py, "conductor.snapshot_worktree");
        let error = module(py, "conductor.project_paths")
            .getattr("ProjectPathError")
            .unwrap();
        assert_error(
            py,
            api.getattr("snapshot_untracked_paths")
                .unwrap()
                .call1((path(py, &repo),))
                .unwrap_err(),
            &error,
            "snapshot_extra_suffixes",
        );
    });
}

#[test]
fn snapshot_exception_removes_temporary_repository() {
    let case = WorkspaceCase::new();
    let repo = repository(&case);
    let forced = case.root().join("forced-snapshot-root");
    let objects = object_inventory(&repo);
    let worktrees = git(&repo, &["worktree", "list", "--porcelain"]);
    Python::attach(|py| {
        let api = module(py, "conductor.snapshot_worktree");
        let made = forced.clone();
        let callback =
            PyCFunction::new_closure(py, None, None, move |_args, _kwargs| -> PyResult<String> {
                fs::create_dir(&made).unwrap();
                Ok(made.to_str().unwrap().into())
            })
            .unwrap();
        let _patch = AttrPatch::replace(
            api.getattr("tempfile").unwrap().as_any(),
            "mkdtemp",
            callback.as_any(),
        );
        let ctx = api
            .getattr("isolated_snapshot")
            .unwrap()
            .call1((path(py, &repo),))
            .unwrap();
        let _snapshot = ctx.call_method0("__enter__").unwrap();
        let error = module(py, "builtins")
            .getattr("RuntimeError")
            .unwrap()
            .call1(("stop inside snapshot",))
            .unwrap();
        let swallowed = ctx
            .call_method1(
                "__exit__",
                (
                    module(py, "builtins").getattr("RuntimeError").unwrap(),
                    error,
                    py.None(),
                ),
            )
            .unwrap();
        assert!(!swallowed.extract::<bool>().unwrap());
    });
    assert!(!forced.exists());
    assert_host_unchanged(&repo, &worktrees, &objects);
}

#[test]
fn snapshot_from_linked_worktree_keeps_shared_objects_unchanged() {
    let case = WorkspaceCase::new();
    let repo = repository(&case);
    let linked = case.root().join("linked");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "linked-test",
            linked.to_str().unwrap(),
        ],
    );
    fs::write(linked.join("changed.py"), "linked change\n").unwrap();
    let objects = object_inventory(&linked);
    let worktrees = git(&linked, &["worktree", "list", "--porcelain"]);
    Python::attach(|py| {
        let (_guard, snapshot) = SnapshotGuard::enter(py, &linked);
        assert_eq!(
            fs::read_to_string(snapshot_path(&snapshot).join("changed.py")).unwrap(),
            "linked change\n"
        );
        assert_host_unchanged(&linked, &worktrees, &objects);
    });
    assert_host_unchanged(&linked, &worktrees, &objects);
}

#[test]
fn exported_interpreter_defaults_to_running_one_and_validates_override() {
    let case = WorkspaceCase::new();
    let repo = repository(&case);
    Python::attach(|py| {
        let api = module(py, "conductor.snapshot_worktree");
        let pick = api.getattr("snapshot_python").unwrap();
        let executable = module(py, "sys").getattr("executable").unwrap();
        assert!(pick
            .call1((path(py, &repo),))
            .unwrap()
            .eq(executable)
            .unwrap());
        fs::write(
            repo.join("pyproject.toml"),
            "[tool.conductor]\nsnapshot_python = \"/opt/other/python\"\n",
        )
        .unwrap();
        assert!(pick
            .call1((path(py, &repo),))
            .unwrap()
            .eq("/opt/other/python")
            .unwrap());
        let error = module(py, "conductor.project_paths")
            .getattr("ProjectPathError")
            .unwrap();
        for bad in ["snapshot_python = \"\"", "snapshot_python = 3"] {
            fs::write(
                repo.join("pyproject.toml"),
                format!("[tool.conductor]\n{bad}\n"),
            )
            .unwrap();
            assert_error(
                py,
                pick.call1((path(py, &repo),)).unwrap_err(),
                &error,
                "snapshot_python",
            );
        }
    });
}

#[test]
fn exported_interpreter_imports_conductor_inside_snapshot_without_venv() {
    let case = WorkspaceCase::new();
    let repo = repository(&case);
    Python::attach(|py| {
        let (_guard, snapshot) = SnapshotGuard::enter(py, &repo);
        let tree = snapshot_path(&snapshot);
        assert!(!tree.join(".venv").exists());
        let exported = module(py, "conductor.mutation_engine_generated")
            .getattr("snapshot_python_environment")
            .unwrap()
            .call0()
            .unwrap()
            .cast_into::<PyDict>()
            .unwrap();
        let executable = text(
            &exported
                .get_item("CONDUCTOR_SNAPSHOT_PYTHON")
                .unwrap()
                .unwrap(),
        );
        let output = Command::new(&executable)
            .args(["-c", "import conductor"])
            .current_dir(&tree)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    });
}
