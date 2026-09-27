#![cfg(feature = "python-compat-tests")]
//! Rust-owned contracts for protected staged-deletion root selection.

#[path = "python_contracts/git_fixture_support.rs"]
#[allow(dead_code)]
mod git_fixture;
#[path = "python_contracts/protected_deletes_support.rs"]
mod protected_support;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use git_fixture::{git, init_protected_repo, write};
use protected_support::{captured_main, checker, main};
use pyo3::prelude::*;
use std::fs;
use support::{path, Case};

fn stage_protected_deletion(repo: &std::path::Path) {
    let protected = "research/runtime/champion_example.json";
    write(repo, protected, "{}\n");
    git(repo, &["add", "--all"]);
    git(repo, &["commit", "-m", "base"]);
    fs::remove_file(repo.join(protected)).unwrap();
    git(repo, &["add", "--update"]);
}

#[test]
fn explicit_root_scans_named_repo_not_cwd() {
    let case = Case::new();
    let target = case.root().join("target");
    let decoy = case.root().join("decoy");
    init_protected_repo(&target);
    init_protected_repo(&decoy);
    stage_protected_deletion(&target);
    let _cwd = case.chdir("decoy");
    Python::attach(|py| {
        assert_eq!(main(py, &["--root", target.to_str().unwrap()]), 1);
    });
}

#[test]
fn default_root_uses_cwd_toplevel_not_module_location() {
    let case = Case::new();
    let repo = case.root().join("repo");
    init_protected_repo(&repo);
    stage_protected_deletion(&repo);
    let _cwd = case.chdir("repo");
    Python::attach(|py| {
        assert_eq!(main(py, &[]), 1);
        let resolved = path(py, &repo).call_method0("resolve").unwrap();
        assert!(checker(py).getattr("ROOT").unwrap().eq(resolved).unwrap());
    });
}

#[test]
fn cwd_outside_worktree_refuses_instead_of_fallback() {
    let case = Case::new();
    let _cwd = case.chdir("not_a_repo");
    Python::attach(|py| assert_eq!(main(py, &[]), 2));
}

#[test]
fn resolved_root_is_printed() {
    let case = Case::new();
    let repo = case.root().join("repo");
    init_protected_repo(&repo);
    git(&repo, &["commit", "--allow-empty", "-m", "base"]);
    let _cwd = case.chdir("repo");
    Python::attach(|py| {
        let (status, out, _) = captured_main(py, &[]);
        assert_eq!(status, 0);
        assert!(out.contains(&format!("root={}", repo.canonicalize().unwrap().display())));
    });
}

#[test]
fn root_mismatch_warns() {
    let case = Case::new();
    let target = case.root().join("target");
    let decoy = case.root().join("decoy");
    init_protected_repo(&target);
    init_protected_repo(&decoy);
    git(&target, &["commit", "--allow-empty", "-m", "base"]);
    let _cwd = case.chdir("decoy");
    Python::attach(|py| {
        let (status, _, err) = captured_main(py, &["--root", target.to_str().unwrap()]);
        assert_eq!(status, 0);
        assert!(err.contains("WARNING"));
        assert!(err.contains(target.canonicalize().unwrap().to_str().unwrap()));
    });
}
