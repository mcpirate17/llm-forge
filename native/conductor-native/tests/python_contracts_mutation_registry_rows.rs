#![cfg(feature = "python-compat-tests")]
//! Rust-owned contracts for Python readers of a registry whose campaigns live in `registry.d/`.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::prelude::*;
use std::path::Path;
use std::process::Command;
use support::{module, path, text, AttrPatch, Case};

const CAMPAIGNS: &str = "conductor/mutation_campaigns";
const TEST_FILE: &str = "research/tests/test_thing.py";

fn fragment_only_registry(case: &Case, directory: &str, manifest: &str) {
    case.write(
        &format!("{directory}/registry.json"),
        r#"{"schema_version": 1, "campaigns": []}"#,
    );
    case.write(
        &format!("{directory}/registry.d/one.json"),
        &format!(r#"{{"manifest": "{manifest}"}}"#),
    );
}

fn git(root: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(root)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.invalid")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.invalid")
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?} failed");
}

#[test]
fn review_resolves_a_fragment_registered_campaign() {
    let case = Case::new();
    let manifest = format!("{CAMPAIGNS}/c.json");
    fragment_only_registry(&case, CAMPAIGNS, &manifest);
    case.write(&manifest, r#"{"campaign_id": "frag-only"}"#);
    Python::attach(|py| {
        let found = module(py, "conductor.candidate_review.verification")
            .getattr("_manifest_for_campaign")
            .unwrap()
            .call1((path(py, case.root()), "frag-only"))
            .unwrap();
        assert_eq!(text(&found.get_item("campaign_id").unwrap()), "frag-only");
    });
}

#[test]
fn drift_scans_a_fragment_registered_campaign() {
    let case = Case::new();
    let manifest = format!("{CAMPAIGNS}/c.json");
    fragment_only_registry(&case, CAMPAIGNS, &manifest);
    case.write(
        TEST_FILE,
        "def test_a():\n    assert True\n\ndef test_b():\n    assert True\n",
    );
    case.write(
        &manifest,
        &format!(
            r#"{{"test_scopes": {{"{TEST_FILE}": {{"mode": "complete",
            "inventory": "python_ast", "nodeids": ["{TEST_FILE}::test_a"]}}}}}}"#
        ),
    );
    case.write(
        &format!("{CAMPAIGNS}/reproducibility_baseline.json"),
        r#"{"unloadable_manifests": []}"#,
    );
    Python::attach(|py| {
        let drifts = module(py, "conductor.complete_scope_drift")
            .getattr("scan")
            .unwrap()
            .call1((path(py, case.root()), py.None()))
            .unwrap();
        assert_eq!(drifts.len().unwrap(), 1);
        let missing: Vec<String> = drifts
            .get_item(0)
            .unwrap()
            .getattr("missing")
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(missing, [format!("{TEST_FILE}::test_b")]);
    });
}

#[test]
fn hygiene_does_not_orphan_fragment_registered_manifests() {
    let case = Case::new();
    Python::attach(|py| {
        let subject = module(py, "conductor.workspace_hygiene");
        let directory = text(&subject.getattr("CAMPAIGN_DIR").unwrap());
        fragment_only_registry(&case, &directory, &format!("{directory}/reg.json"));
        case.write(&format!("{directory}/reg.json"), "{}");
        case.write(&format!("{directory}/stray.json"), "{}");
        let _root = AttrPatch::replace(subject.as_any(), "ROOT", path(py, case.root()).as_any());
        let state = subject.getattr("manifest_state").unwrap().call0().unwrap();
        let orphans: Vec<String> = state.get_item(0).unwrap().extract().unwrap();
        let dangling: Vec<String> = state.get_item(1).unwrap().extract().unwrap();
        assert_eq!(orphans, [format!("{directory}/stray.json")]);
        assert!(dangling.is_empty());
    });
}

#[test]
fn anchor_reads_fragments_from_the_anchor_commit_only() {
    let case = Case::new();
    let manifest = format!("{CAMPAIGNS}/c.json");
    fragment_only_registry(&case, CAMPAIGNS, &manifest);
    case.write(&manifest, "{}");
    git(case.root(), &["init", "-q"]);
    git(case.root(), &["add", "-A"]);
    git(case.root(), &["commit", "-q", "-m", "anchor"]);
    let head = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(case.root())
        .output()
        .unwrap();
    let anchor = String::from_utf8(head.stdout).unwrap().trim().to_owned();
    // The fragment is gone from disk and from HEAD's successors; only the anchor tree has it.
    std::fs::remove_file(case.root().join(CAMPAIGNS).join("registry.d/one.json")).unwrap();
    Python::attach(|py| {
        let subject = module(py, "conductor.mutation_testing_support");
        let check = |manifest_path: &str| -> Option<String> {
            let kwargs = pyo3::types::PyDict::new(py);
            kwargs.set_item("anchor_commit", &anchor).unwrap();
            kwargs.set_item("manifest_path", manifest_path).unwrap();
            kwargs.set_item("manifest_sha256", "0".repeat(64)).unwrap();
            let result = subject
                .getattr("_registered_anchor_manifest_error")
                .unwrap()
                .call((path(py, case.root()),), Some(&kwargs))
                .unwrap();
            result.extract().unwrap()
        };
        // Registered through the anchor's fragment: reaches the manifest-hash check.
        assert_eq!(
            check(&manifest).as_deref(),
            Some("legacy receipt anchor manifest hash mismatch")
        );
        assert_eq!(
            check(&format!("{CAMPAIGNS}/other.json")).as_deref(),
            Some("legacy receipt campaign was not registered at the anchor")
        );
    });
}
