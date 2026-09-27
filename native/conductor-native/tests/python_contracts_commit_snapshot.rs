#![cfg(feature = "python-compat-tests")]
//! Rust-owned contracts for Git snapshot safety and private-index behavior.

#[path = "python_contracts/git_fixture_support.rs"]
#[allow(dead_code)]
mod git_fixture;
#[path = "python_contracts/commit_snapshot_support.rs"]
mod snapshot_support;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use git_fixture::{git, init_snapshot_repo, write};
use pyo3::prelude::*;
use snapshot_support::snapshot;
use support::Case;

fn with_repo(check: impl FnOnce(Python<'_>, &std::path::Path)) {
    let case = Case::new();
    let repo = case.root().join("repo");
    init_snapshot_repo(&repo);
    Python::attach(|py| check(py, &repo));
}

fn expect_ref(py: Python<'_>, repo: &std::path::Path, owner: &str) -> String {
    snapshot(py, repo, owner).expect("snapshot reference")
}

#[test]
fn snapshot_returns_a_ref_that_exists() {
    with_repo(|py, repo| {
        let reference = expect_ref(py, repo, "agent");
        assert!(reference.starts_with("refs/snapshots/agent/"));
        assert_eq!(git(repo, &["cat-file", "-t", &reference]), "commit");
    });
}

#[test]
fn snapshot_captures_an_unstaged_modification() {
    with_repo(|py, repo| {
        write(repo, "tracked.py", "VALUE = 999\n");
        let reference = expect_ref(py, repo, "agent");
        assert_eq!(
            git(
                repo,
                &["cat-file", "-p", &format!("{reference}:tracked.py")]
            ),
            "VALUE = 999"
        );
    });
}

#[test]
fn snapshot_captures_an_untracked_file() {
    with_repo(|py, repo| {
        write(repo, "untracked.py", "NEW = 2\n");
        let reference = expect_ref(py, repo, "agent");
        assert_eq!(
            git(
                repo,
                &["cat-file", "-p", &format!("{reference}:untracked.py")]
            ),
            "NEW = 2"
        );
    });
}

#[test]
fn snapshot_excludes_gitignored_files() {
    with_repo(|py, repo| {
        write(repo, ".gitignore", "*.log\n");
        write(repo, "noise.log", "x\n");
        let reference = expect_ref(py, repo, "agent");
        let listing = git(repo, &["ls-tree", "-r", "--name-only", &reference]);
        let names: Vec<_> = listing.lines().collect();
        assert!(!names.contains(&"noise.log"));
        assert!(names.contains(&".gitignore"));
    });
}

#[test]
fn snapshot_does_not_touch_the_shared_index() {
    with_repo(|py, repo| {
        write(repo, "staged.py", "S = 1\n");
        git(repo, &["add", "staged.py"]);
        let before = git(repo, &["diff", "--cached", "--name-only"]);
        write(repo, "untracked.py", "U = 1\n");
        expect_ref(py, repo, "agent");
        assert_eq!(git(repo, &["diff", "--cached", "--name-only"]), before);
    });
}

#[test]
fn snapshot_leaves_no_index_file_behind() {
    with_repo(|py, repo| {
        expect_ref(py, repo, "agent");
        let git_dir = git(repo, &["rev-parse", "--absolute-git-dir"]);
        let leftovers: Vec<_> = std::fs::read_dir(git_dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with("governance-snapshot-index-"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "leftover private index: {leftovers:?}"
        );
    });
}

#[test]
fn snapshot_parents_the_current_head() {
    with_repo(|py, repo| {
        let head = git(repo, &["rev-parse", "HEAD"]);
        let reference = expect_ref(py, repo, "agent");
        assert_eq!(git(repo, &["rev-parse", &format!("{reference}^")]), head);
    });
}

#[test]
fn snapshot_of_a_clean_tree_still_produces_a_ref() {
    with_repo(|py, repo| {
        let reference = expect_ref(py, repo, "agent");
        assert_eq!(
            git(
                repo,
                &["cat-file", "-p", &format!("{reference}:tracked.py")]
            ),
            "VALUE = 1"
        );
    });
}

#[test]
fn snapshot_returns_none_outside_a_repository() {
    let case = Case::new();
    Python::attach(|py| assert_eq!(snapshot(py, case.root(), "agent"), None));
}

#[test]
fn owner_appears_in_the_ref() {
    with_repo(|py, repo| {
        let reference = expect_ref(py, repo, "glm-flash-04");
        assert!(reference.contains("/glm-flash-04/"));
    });
}
