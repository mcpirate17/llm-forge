use super::*;
use crate::mutation_run_test_support::{campaign, write};
use serde_json::json;
use std::collections::BTreeMap;

fn fixture() -> (tempfile::TempDir, Campaign, Receipt) {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "src/a.rs", "fn a() {}\n");
    write(dir.path(), "src/b.rs", "fn b() {}\n");
    let mut campaign = campaign(dir.path(), json!({"mutation_engine": "cargo-mutants"}));
    campaign.source = vec!["src/a.rs".into(), "src/b.rs".into()];
    campaign.source_sha256 = BTreeMap::from([
        ("src/a.rs".into(), "a".into()),
        ("src/b.rs".into(), "b".into()),
    ]);
    let receipt = Receipt::from_iter([("mutants".into(), json!([]))]);
    (dir, campaign, receipt)
}

#[test]
fn mixed_sources_are_incomplete_when_one_has_no_scored_mutants() {
    let (dir, mut campaign, mut receipt) = fixture();
    campaign.exclude = vec!["src/b.rs".into()];
    receipt.insert(
        "mutants".into(),
        json!([
            {"path":"src/a.rs", "outcome":"KILLED"},
            {"path":"src/b.rs", "outcome":"TIMED_OUT"}
        ]),
    );
    let error = require(&campaign, dir.path(), &mut receipt).unwrap_err();
    assert!(format!("{error:?}").contains("src/b.rs"));
    assert_eq!(receipt["status"], "INCOMPLETE");
    assert_eq!(receipt["source_coverage"]["missing"], json!(["src/b.rs"]));
    assert_eq!(
        receipt["source_coverage"]["scored_by_source"]["src/a.rs"],
        1
    );
}

#[test]
fn every_source_with_a_scored_mutant_is_complete() {
    for engine in ["cargo-mutants", "fest", "mull"] {
        let (dir, mut campaign, mut receipt) = fixture();
        campaign.mutation_engine = engine.into();
        receipt.insert(
            "mutants".into(),
            json!([
                {"path":"./src/a.rs", "outcome":"KILLED"},
                {"path":"src/b.rs", "outcome":"SURVIVED"}
            ]),
        );
        require(&campaign, dir.path(), &mut receipt).unwrap();
        assert_eq!(receipt["source_coverage"]["complete"], true, "{engine}");
        assert_eq!(receipt["source_coverage"]["missing"], json!([]));
    }
}

#[test]
fn unexpected_missing_or_escaping_row_paths_are_incomplete() {
    for path in ["src/missing.rs", "../outside.rs", "/tmp/outside.rs", ""] {
        let (dir, campaign, mut receipt) = fixture();
        receipt.insert("mutants".into(), json!([{"path":path,"outcome":"KILLED"}]));
        assert!(
            require(&campaign, dir.path(), &mut receipt).is_err(),
            "{path}"
        );
        assert_eq!(receipt["status"], "INCOMPLETE");
    }
    let (dir, campaign, mut receipt) = fixture();
    write(dir.path(), "src/other.rs", "fn other() {}\n");
    receipt.insert(
        "mutants".into(),
        json!([{"path":"src/other.rs","outcome":"KILLED"}]),
    );
    assert!(require(&campaign, dir.path(), &mut receipt).is_err());
}

#[cfg(unix)]
#[test]
fn distinct_symlink_aliases_are_refused() {
    let (dir, mut campaign, mut receipt) = fixture();
    std::os::unix::fs::symlink("a.rs", dir.path().join("src/alias.rs")).unwrap();
    campaign.source = vec!["src/a.rs".into(), "./src/alias.rs".into()];
    campaign.source_sha256 = BTreeMap::from([
        ("src/a.rs".into(), "a".into()),
        ("src/alias.rs".into(), "a".into()),
    ]);
    receipt.insert(
        "mutants".into(),
        json!([{"path":"src/a.rs","outcome":"KILLED"}]),
    );
    assert!(require(&campaign, dir.path(), &mut receipt).is_err());
    assert_eq!(receipt["status"], "INCOMPLETE");

    campaign.source = vec!["src/a.rs".into()];
    campaign.source_sha256 = BTreeMap::from([("src/a.rs".into(), "a".into())]);
    receipt.insert(
        "mutants".into(),
        json!([{"path":"src/alias.rs","outcome":"KILLED"}]),
    );
    assert!(require(&campaign, dir.path(), &mut receipt).is_err());
}
