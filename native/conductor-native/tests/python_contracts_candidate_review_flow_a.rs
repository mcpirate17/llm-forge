#![cfg(feature = "python-compat-tests")]
//! Retained Python source and ownership contracts; obsolete orchestration cases removed.

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
use std::collections::BTreeSet;
use std::fs;
use support::{assert_error, attr_text, module, path};

fn tree_oid(candidate: &Bound<'_, PyAny>) -> String {
    fixture::string_attr(candidate, "tree_oid")
}
#[test]
fn index_candidate_ignores_unstaged_and_untracked_content() {
    let case = fixture::isolated_case();
    let repo = case.root().join("repo");
    fixture::init_repo(&repo);
    fixture::write_fixture(&repo, "tracked.txt", "committed\n");
    fixture::commit_all(&repo, "baseline");
    fixture::write_fixture(&repo, "candidate.txt", "staged\n");
    fixture::git(&repo, &["add", "candidate.txt"]);
    fixture::write_fixture(&repo, "tracked.txt", "unstaged\n");
    fixture::write_fixture(&repo, "untracked.txt", "untracked\n");
    Python::attach(|py| {
        let candidate = fixture::resolve_candidate(py, &repo, "index", None, None);
        let changes = candidate.getattr("changes").unwrap();
        assert_eq!(changes.len().unwrap(), 1);
        let change = changes.get_item(0).unwrap();
        assert_eq!(
            (attr_text(&change, "status"), attr_text(&change, "path")),
            ("A".into(), "candidate.txt".into())
        );
        fixture::with_materialized(py, &repo, &tree_oid(&candidate), None, |snapshot, _| {
            assert_eq!(
                fs::read_to_string(snapshot.join("candidate.txt")).unwrap(),
                "staged\n"
            );
            assert_eq!(
                fs::read_to_string(snapshot.join("tracked.txt")).unwrap(),
                "committed\n"
            );
            assert!(!snapshot.join("untracked.txt").exists());
        });
    });
}
#[test]
fn structured_claims_reject_exact_path_overlap_and_bind_content() {
    let case = fixture::isolated_case();
    let repo = case.root().join("repo");
    fixture::init_repo(&repo);
    fixture::write_fixture(&repo, "base.txt", "base\n");
    fixture::commit_all(&repo, "baseline");
    Python::attach(|py| {
        let ownership = module(py, "conductor.candidate_review.ownership");
        let kwargs = PyDict::new(py);
        kwargs.set_item("owner", "Codex").unwrap();
        kwargs
            .set_item("paths", ["conductor/candidate_review"])
            .unwrap();
        kwargs
            .set_item("justification", "candidate governance implementation")
            .unwrap();
        kwargs.set_item("max_minutes", 60).unwrap();
        let claim = ownership
            .getattr("create_claim")
            .unwrap()
            .call((path(py, &repo),), Some(&kwargs))
            .unwrap();
        let loaded = ownership
            .getattr("load_claims")
            .unwrap()
            .call1((path(py, &repo),))
            .unwrap();
        assert_eq!(loaded.get_item(0).unwrap().len().unwrap(), 1);
        assert!(loaded
            .get_item(0)
            .unwrap()
            .get_item(0)
            .unwrap()
            .eq(&claim)
            .unwrap());
        assert_eq!(
            loaded
                .get_item(1)
                .unwrap()
                .extract::<String>()
                .unwrap()
                .len(),
            64
        );
        kwargs.set_item("owner", "Other agent").unwrap();
        kwargs
            .set_item("paths", ["conductor/candidate_review/engine.py"])
            .unwrap();
        kwargs
            .set_item("justification", "conflicting edit")
            .unwrap();
        let error = ownership
            .getattr("create_claim")
            .unwrap()
            .call((path(py, &repo),), Some(&kwargs))
            .unwrap_err();
        let class = ownership.getattr("OwnershipError").unwrap();
        assert_error(py, error, &class, "overlaps active claim");
        let release = ownership.getattr("release_claim").unwrap();
        let options = PyDict::new(py);
        options
            .set_item("claim_id", claim.getattr("claim_id").unwrap())
            .unwrap();
        options.set_item("owner", "Other agent").unwrap();
        assert_error(
            py,
            release
                .call((path(py, &repo),), Some(&options))
                .unwrap_err(),
            &class,
            "not 'Other agent'",
        );
        options.set_item("owner", "Codex").unwrap();
        assert!(release
            .call((path(py, &repo),), Some(&options))
            .unwrap()
            .extract::<bool>()
            .unwrap());
        let empty = ownership
            .getattr("load_claims")
            .unwrap()
            .call1((path(py, &repo),))
            .unwrap();
        assert_eq!(empty.get_item(0).unwrap().len().unwrap(), 0);
    });
}
#[test]
fn range_candidate_uses_merge_base_even_with_a_clean_index() {
    let case = fixture::isolated_case();
    let repo = case.root().join("repo");
    fixture::init_repo(&repo);
    fixture::write_fixture(&repo, "shared.txt", "base\n");
    let merge_base = fixture::commit_all(&repo, "base");
    fixture::git(&repo, &["switch", "--quiet", "--create", "feature"]);
    fixture::write_fixture(&repo, "shared.txt", "feature\n");
    fixture::write_fixture(&repo, "feature.txt", "feature-only\n");
    let feature = fixture::commit_all(&repo, "feature");
    fixture::git(&repo, &["switch", "--quiet", "main"]);
    fixture::write_fixture(&repo, "shared.txt", "main\n");
    fixture::commit_all(&repo, "main divergence");
    assert_eq!(fixture::git(&repo, &["status", "--porcelain"]), "");
    Python::attach(|py| {
        let candidate =
            fixture::resolve_candidate(py, &repo, "range", Some("main"), Some("feature"));
        assert_eq!(attr_text(&candidate, "base_commit_oid"), merge_base);
        assert_eq!(attr_text(&candidate, "commit_oid"), feature);
        let paths: BTreeSet<String> = candidate
            .getattr("changes")
            .unwrap()
            .try_iter()
            .unwrap()
            .map(|row| attr_text(&row.unwrap(), "path"))
            .collect();
        assert_eq!(
            paths,
            BTreeSet::from(["feature.txt".into(), "shared.txt".into()])
        );
    });
}
