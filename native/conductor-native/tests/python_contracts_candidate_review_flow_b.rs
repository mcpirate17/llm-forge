#![cfg(feature = "python-compat-tests")]
//! Retained Python source contracts; obsolete orchestration cases removed.

#[path = "python_contracts/candidate_review_support.rs"]
#[allow(dead_code)]
mod candidate_review_support;
#[path = "python_contracts/git_fixture_support.rs"]
#[allow(dead_code)]
mod git_fixture_support;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use candidate_review_support as fixture;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict};
use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;
use support::{assert_error, attr_text, module, path};

fn tree_oid(candidate: &Bound<'_, PyAny>) -> String {
    fixture::string_attr(candidate, "tree_oid")
}
#[test]
fn index_preserves_deletion_and_rename_identity() {
    let case = fixture::isolated_case();
    let repo = case.root().join("repo");
    fixture::init_repo(&repo);
    fixture::write_fixture(&repo, "deleted.txt", "remove me\n");
    fixture::write_fixture(&repo, "old-name.txt", "rename me exactly\n");
    fixture::commit_all(&repo, "baseline");
    fixture::git(&repo, &["rm", "--quiet", "deleted.txt"]);
    fixture::git(&repo, &["mv", "old-name.txt", "new-name.txt"]);
    Python::attach(|py| {
        let candidate = fixture::resolve_candidate(py, &repo, "index", None, None);
        let mut changes = HashMap::new();
        for row in candidate.getattr("changes").unwrap().try_iter().unwrap() {
            let row = row.unwrap();
            changes.insert(attr_text(&row, "path"), row);
        }
        let deleted = &changes["deleted.txt"];
        assert_eq!(attr_text(deleted, "status"), "D");
        assert!(deleted
            .getattr("deleted")
            .unwrap()
            .extract::<bool>()
            .unwrap());
        assert_eq!(attr_text(deleted, "new_mode"), "000000");
        assert_eq!(attr_text(deleted, "new_oid"), "0".repeat(40));
        let renamed = &changes["new-name.txt"];
        assert_eq!(attr_text(renamed, "status"), "R100");
        assert_eq!(attr_text(renamed, "old_path"), "old-name.txt");
        assert!(renamed
            .getattr("old_oid")
            .unwrap()
            .eq(renamed.getattr("new_oid").unwrap())
            .unwrap());
    });
}
#[test]
fn materialize_tree_allows_internal_symlink_and_rejects_escape() {
    let case = fixture::isolated_case();
    let repo = case.root().join("repo");
    fixture::init_repo(&repo);
    fixture::write_fixture(&repo, "docs/target.txt", "inside\n");
    symlink("target.txt", repo.join("docs/link.txt")).unwrap();
    let safe_commit = fixture::commit_all(&repo, "safe link");
    Python::attach(|py| {
        let safe = fixture::resolve_candidate(py, &repo, "commit", None, Some(&safe_commit));
        fixture::with_materialized(py, &repo, &tree_oid(&safe), None, |snapshot, entries| {
            let link = snapshot.join("docs/link.txt");
            assert!(link.is_symlink());
            assert_eq!(fs::read_link(&link).unwrap(), Path::new("target.txt"));
            assert_eq!(fs::read_to_string(link).unwrap(), "inside\n");
            assert!(entries
                .try_iter()
                .unwrap()
                .any(|entry| attr_text(&entry.unwrap(), "path") == "docs/link.txt"));
        });
        symlink("../outside.txt", repo.join("escape")).unwrap();
        let unsafe_commit = fixture::commit_all(&repo, "escaping link");
        let unsafe_candidate =
            fixture::resolve_candidate(py, &repo, "commit", None, Some(&unsafe_commit));
        let source = module(py, "conductor.candidate_review.git_source");
        let manager = source
            .getattr("materialize_tree")
            .unwrap()
            .call1((path(py, &repo), tree_oid(&unsafe_candidate)))
            .unwrap();
        assert_error(
            py,
            manager.call_method0("__enter__").unwrap_err(),
            &source.getattr("GitSourceError").unwrap(),
            "symlink escapes candidate snapshot",
        );
    });
}
#[test]
fn materialize_tree_rejects_blob_before_loading_over_budget() {
    let case = fixture::isolated_case();
    let repo = case.root().join("repo");
    fixture::init_repo(&repo);
    fs::write(repo.join("payload.bin"), b"0123456789").unwrap();
    let commit = fixture::commit_all(&repo, "payload");
    Python::attach(|py| {
        let candidate = fixture::resolve_candidate(py, &repo, "commit", None, Some(&commit));
        let source = module(py, "conductor.candidate_review.git_source");
        let kwargs = PyDict::new(py);
        kwargs.set_item("max_blob_bytes", 9).unwrap();
        let manager = source
            .getattr("materialize_tree")
            .unwrap()
            .call((path(py, &repo), tree_oid(&candidate)), Some(&kwargs))
            .unwrap();
        assert_error(
            py,
            manager.call_method0("__enter__").unwrap_err(),
            &source.getattr("GitSourceError").unwrap(),
            "blob exceeds materialization limit",
        );
    });
}
#[test]
fn gitlink_is_represented_without_materializing_foreign_tree() {
    let case = fixture::isolated_case();
    let module_repo = case.root().join("module");
    fixture::init_repo(&module_repo);
    fixture::write_fixture(&module_repo, "module.txt", "module\n");
    let module_commit = fixture::commit_all(&module_repo, "module");
    let repo = case.root().join("repo");
    fixture::init_repo(&repo);
    fixture::git(
        &repo,
        &[
            "commit",
            "--quiet",
            "--allow-empty",
            "--message",
            "baseline",
        ],
    );
    fixture::git(
        &repo,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            "160000",
            &module_commit,
            "vendor/module",
        ],
    );
    fixture::git(
        &repo,
        &["commit", "--quiet", "--message", "record submodule"],
    );
    Python::attach(|py| {
        let candidate = fixture::resolve_candidate(py, &repo, "commit", None, None);
        let source = module(py, "conductor.candidate_review.git_source");
        let entries = source
            .getattr("list_tree")
            .unwrap()
            .call1((path(py, &repo), tree_oid(&candidate)))
            .unwrap();
        let entry = entries.get_item(0).unwrap();
        assert_eq!(
            (
                attr_text(&entry, "path"),
                attr_text(&entry, "mode"),
                attr_text(&entry, "object_type"),
                attr_text(&entry, "oid")
            ),
            (
                "vendor/module".into(),
                "160000".into(),
                "commit".into(),
                module_commit.clone()
            )
        );
        let change = candidate.getattr("changes").unwrap().get_item(0).unwrap();
        assert_eq!(attr_text(&change, "new_mode"), "160000");
        assert_eq!(attr_text(&change, "new_oid"), module_commit);
        fixture::with_materialized(
            py,
            &repo,
            &tree_oid(&candidate),
            None,
            |snapshot, materialized| {
                assert!(materialized.eq(&entries).unwrap());
                assert!(!snapshot.join("vendor/module").exists());
            },
        );
    });
}
#[test]
fn malformed_and_ambiguous_refs_fail_closed() {
    let case = fixture::isolated_case();
    let repo = case.root().join("repo");
    fixture::init_repo(&repo);
    fixture::write_fixture(&repo, "value.txt", "one\n");
    fixture::commit_all(&repo, "one");
    fixture::git(&repo, &["branch", "collision"]);
    fixture::write_fixture(&repo, "value.txt", "two\n");
    fixture::commit_all(&repo, "two");
    fixture::git(&repo, &["tag", "collision"]);
    Python::attach(|py| {
        let source = module(py, "conductor.candidate_review.git_source");
        let resolve = source.getattr("resolve_commit").unwrap();
        let class = source.getattr("GitSourceError").unwrap();
        assert_error(
            py,
            resolve.call1((path(py, &repo), "--verify")).unwrap_err(),
            &class,
            "invalid or ambiguous Git ref",
        );
        assert_error(
            py,
            resolve.call1((path(py, &repo), "collision")).unwrap_err(),
            &class,
            "ambiguous",
        );
    });
}
