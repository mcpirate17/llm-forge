#![cfg(feature = "python-compat-tests")]
//! Retained source selection and ownership contracts; retired policy cases removed.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::prelude::*;
use pyo3::types::PyDict;
use std::path::Path;
use std::process::Command;
use support::{attr_text, module, path, text, Case};

fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {}: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}
fn branched_repo(case: &Case) -> (std::path::PathBuf, String, String) {
    let repo = case.mkdir("repo");
    git(&repo, &["init", "-q", "-b", "master"]);
    git(&repo, &["config", "user.email", "gate@example.invalid"]);
    git(&repo, &["config", "user.name", "gate"]);
    git(&repo, &["config", "commit.gpgsign", "false"]);
    git(&repo, &["config", "core.hooksPath", "/dev/null"]);
    std::fs::write(repo.join("a.txt"), "one\n").unwrap();
    git(&repo, &["add", "a.txt"]);
    git(&repo, &["commit", "-qm", "integration base"]);
    let integration = git(&repo, &["rev-parse", "HEAD"]);
    git(&repo, &["checkout", "-q", "-b", "lane"]);
    std::fs::write(repo.join("a.txt"), "two\n").unwrap();
    git(&repo, &["commit", "-qam", "lane commit"]);
    let head = git(&repo, &["rev-parse", "HEAD"]);
    std::fs::write(repo.join("b.txt"), "staged\n").unwrap();
    git(&repo, &["add", "b.txt"]);
    (repo, integration, head)
}
#[test]
fn index_range_and_commit_candidates_bind_waivers_to_the_integration_base() {
    let mut case = Case::new();
    case.set_env("CONDUCTOR_INTEGRATION_BRANCH", "master");
    let (repo, integration, head) = branched_repo(&case);
    Python::attach(|py| {
        let source = module(py, "conductor.candidate_review.git_source");
        let resolve = source.getattr("resolve_candidate").unwrap();
        let kwargs = PyDict::new(py);
        kwargs.set_item("kind", "index").unwrap();
        let index = resolve.call((path(py, &repo),), Some(&kwargs)).unwrap();
        assert_eq!(attr_text(&index, "base_commit_oid"), head);
        assert_eq!(attr_text(&index, "integration_base_oid"), integration);
        assert_eq!(attr_text(&index, "waiver_base"), integration);
        assert!(attr_text(&index, "integration_base_detail").contains("master"));
        let changes = index.getattr("changes").unwrap();
        assert_eq!(changes.len().unwrap(), 1);
        assert_eq!(attr_text(&changes.get_item(0).unwrap(), "path"), "b.txt");
        let ranged = PyDict::new(py);
        for (key, value) in [
            ("kind", "range"),
            ("target_ref", "lane"),
            ("base_ref", "master"),
        ] {
            ranged.set_item(key, value).unwrap();
        }
        let range = resolve.call((path(py, &repo),), Some(&ranged)).unwrap();
        assert_eq!(attr_text(&range, "waiver_base"), integration);
        assert_eq!(
            attr_text(&range, "integration_base_detail"),
            "range candidate base"
        );
        let committed = PyDict::new(py);
        committed.set_item("kind", "commit").unwrap();
        committed.set_item("target_ref", "lane").unwrap();
        let commit = resolve.call((path(py, &repo),), Some(&committed)).unwrap();
        assert_eq!(attr_text(&commit, "waiver_base"), integration);
        assert_eq!(
            attr_text(&commit, "integration_base_detail"),
            "commit candidate base"
        );
    });
}
#[test]
fn unrelated_history_cannot_be_a_value_waiver_base() {
    let case = Case::new();
    let (repo, _integration, _head) = branched_repo(&case);
    git(&repo, &["checkout", "-q", "--orphan", "unrelated"]);
    std::fs::write(repo.join("c.txt"), "elsewhere\n").unwrap();
    git(&repo, &["add", "c.txt"]);
    git(&repo, &["commit", "-qm", "unrelated root"]);
    let unrelated = git(&repo, &["rev-parse", "HEAD"]);
    git(&repo, &["checkout", "-q", "lane"]);
    Python::attach(|py| {
        let resolve = module(py, "conductor.candidate_review.git_source")
            .getattr("resolve_candidate")
            .unwrap();
        for kind in ["index", "commit"] {
            let kwargs = PyDict::new(py);
            kwargs.set_item("kind", kind).unwrap();
            kwargs.set_item("base_ref", &unrelated).unwrap();
            if kind == "commit" {
                kwargs.set_item("target_ref", "lane").unwrap();
            }
            let candidate = resolve.call((path(py, &repo),), Some(&kwargs)).unwrap();
            assert_eq!(attr_text(&candidate, "base_commit_oid"), unrelated);
            assert!(candidate.getattr("integration_base_oid").unwrap().is_none());
            assert!(candidate.getattr("waiver_base").unwrap().is_none());
            assert!(
                attr_text(&candidate, "integration_base_detail").contains("is not an ancestor of")
            );
        }
    });
}
#[test]
fn ownership_rejects_broad_paths_overlong_claims_and_tampered_store() {
    let case = Case::new();
    let (repo, _, _) = branched_repo(&case);
    Python::attach(|py| {
        let ownership = module(py, "conductor.candidate_review.ownership");
        let error_class = ownership.getattr("OwnershipError").unwrap();
        let normalize = ownership.getattr("normalize_claim_path").unwrap();
        for invalid in [
            "research",
            "../escape",
            "/absolute",
            "conductor/**",
            "conductor/gate.py,conductor/kb_retrieve.py",
            "conductor/gate.py conductor/kb_retrieve.py",
        ] {
            support::assert_error(
                py,
                normalize.call1((invalid,)).unwrap_err(),
                &error_class,
                "narrow and repository-relative",
            );
        }
        let create = ownership.getattr("create_claim").unwrap();
        let kwargs = PyDict::new(py);
        kwargs.set_item("owner", "Codex").unwrap();
        kwargs.set_item("paths", ["conductor/probe.py"]).unwrap();
        kwargs
            .set_item("justification", "tamper-evident ownership test")
            .unwrap();
        kwargs.set_item("max_minutes", 25 * 60).unwrap();
        support::assert_error(
            py,
            create.call((path(py, &repo),), Some(&kwargs)).unwrap_err(),
            &error_class,
            "max time must be",
        );
        kwargs.set_item("max_minutes", 60).unwrap();
        let claim = create.call((path(py, &repo),), Some(&kwargs)).unwrap();
        assert!(!attr_text(&claim, "claim_id").is_empty());
        let store = ownership
            .getattr("claim_store_path")
            .unwrap()
            .call1((path(py, &repo),))
            .unwrap();
        let store_path = std::path::PathBuf::from(text(&store));
        let mut payload: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&store_path).unwrap()).unwrap();
        payload["claims"][0]["owner"] = serde_json::json!("Mallory");
        std::fs::write(&store_path, serde_json::to_vec(&payload).unwrap()).unwrap();
        let error = ownership
            .getattr("load_claims")
            .unwrap()
            .call1((path(py, &repo),))
            .unwrap_err();
        support::assert_error(py, error, &error_class, "not bound to its content");
    });
}
