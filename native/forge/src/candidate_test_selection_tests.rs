use super::*;
use crate::{
    candidate_snapshot::Limits,
    candidate_source::{Change, Kind},
};
use std::{fs, path::PathBuf};
const POLICY: &str = r#"schema_version=1
block_at='high'
max_workers=2
cache_ttl_days=7
claim_max_age_hours=24
max_file_bytes=100000
max_binary_bytes=200000
coverage_threshold=80.0
high_risk_coverage_threshold=90.0
baseline_expires=2026-11-30
[classes]
python=['pkg/*.py']
[risk]
high=['pkg/security/*']
[paths]
protected_deletes=[]
hot=[]
generated=[]
[checks.unit]
kind='builtin'
profiles=['fast']
always=true
"#;
struct Fixture {
    root: PathBuf,
    repo: git2::Repository,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(crate::native_id::new("test-selection").unwrap());
        fs::create_dir(&root).unwrap();
        let repo = git2::Repository::init(root.join("repo")).unwrap();
        let fixture = Self { root, repo };
        fixture.write("candidate.toml", POLICY);
        fixture
    }
    fn write(&self, path: &str, text: &str) {
        let output = self.root.join("repo").join(path);
        fs::create_dir_all(output.parent().unwrap()).unwrap();
        fs::write(output, text).unwrap();
        let mut index = self.repo.index().unwrap();
        index.add_path(Path::new(path)).unwrap();
        index.write().unwrap();
    }
    fn captured(&self) -> (Source, Snapshot, Candidate, Policy) {
        let tree = self.repo.index().unwrap().write_tree().unwrap().to_string();
        let source = Source::open(&self.root.join("repo")).unwrap();
        let snapshot = Snapshot::create(
            &source,
            &tree,
            &self.root.join("snapshot"),
            Limits::default(),
        )
        .unwrap();
        let policy =
            crate::candidate_policy::load(&snapshot.path(), Some("candidate.toml"), "2026-11-01")
                .unwrap();
        let candidate = Candidate {
            kind: Kind::Index,
            tree_oid: tree,
            base_tree_oid: "4b825dc642cb6eb9a060e54bf8d69288fbee4904".into(),
            base_commit_oid: None,
            commit_oid: None,
            target_ref: None,
            changes: vec![],
            integration_base_oid: None,
            integration_base_detail: String::new(),
        };
        (source, snapshot, candidate, policy)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
fn change(path: &str, risk: &str) -> Change {
    Change {
        status: "modified".into(),
        path: path.into(),
        old_path: None,
        old_mode: "100644".into(),
        new_mode: "100644".into(),
        old_oid: "1".repeat(40),
        new_oid: "2".repeat(40),
        classes: vec!["python".into()],
        risk: risk.into(),
    }
}
#[test]
fn missing_graph_uses_full_candidate_inventory_and_held_files_detect_drift() {
    let f = Fixture::new();
    f.write("pkg/source.py", "value=1\n");
    f.write("tests/test_a.py", "def test_a(): pass\n");
    f.write("tests/test_b.py", "def test_b(): pass\n");
    let (source, snapshot, mut candidate, policy) = f.captured();
    candidate.changes.push(change("pkg/source.py", "low"));
    let selection = collect(&source, &snapshot, &candidate, &policy, "2026-11-01", None).unwrap();
    assert_eq!(
        selection.python_tests,
        ["tests/test_a.py", "tests/test_b.py"]
    );
    assert_eq!(selection.evidence["graph"]["complete"], false);
    assert_eq!(
        selection.evidence["graph"]["scope"],
        "candidate_tracked_conventional_test_inventory"
    );
    assert_eq!(selection.evidence["test_execution_verified"], false);
    assert_eq!(selection.result.status, CheckStatus::Passed);
    fs::write(
        snapshot.path().join("tests/test_b.py"),
        "changed after capture",
    )
    .unwrap();
    assert!(selection.verify().is_err());
}
#[test]
fn high_risk_requires_captured_marker_or_verified_mutation_evidence() {
    let f = Fixture::new();
    f.write("pkg/security/source.py", "value=1\n");
    f.write("tests/test_a.py", "def test_a(): pass\n");
    let (source, snapshot, mut candidate, policy) = f.captured();
    // The collector must recompute risk from the captured policy.
    candidate
        .changes
        .push(change("pkg/security/source.py", "normal"));
    let selection = collect(&source, &snapshot, &candidate, &policy, "2026-11-01", None).unwrap();
    assert_eq!(selection.result.status, CheckStatus::Error);
    assert!(selection
        .result
        .findings
        .iter()
        .any(|f| f.rule_id == "mutation-evidence-unverified"));
    drop(selection);
    snapshot.discard().unwrap();
    f.write("tests/test_a.py", "from hypothesis import given\n");
    let (source, snapshot, mut candidate, policy) = f.captured();
    candidate
        .changes
        .push(change("pkg/security/source.py", "high"));
    let selection = collect(&source, &snapshot, &candidate, &policy, "2026-11-01", None).unwrap();
    assert_eq!(
        selection.evidence["property_marker_path"],
        "tests/test_a.py"
    );
    assert_eq!(
        selection.evidence["property_marker_scope"],
        "legacy_text_heuristic_not_execution_proof"
    );
    assert_eq!(selection.result.status, CheckStatus::Passed);
}
#[test]
fn candidate_registry_selects_all_contract_targets_without_embedded_tsv() {
    let f = Fixture::new();
    f.write("src/engine.py", "value=1\n");
    f.write(
        "native/conductor-native/Cargo.toml",
        "[package]\nname='conductor-native'\nversion='0.1.0'\n",
    );
    f.write(
        "native/conductor-native/src/python_contract_targets.tsv",
        "src/engine.py\tpython_contracts_one\nsrc/other.py\tpython_contracts_two\n",
    );
    f.write(
        "native/conductor-native/tests/python_contracts_one.rs",
        "#[test] fn a() {}\n",
    );
    f.write(
        "native/conductor-native/tests/python_contracts_two.rs",
        "#[test] fn b() {}\n",
    );
    let (source, snapshot, mut candidate, policy) = f.captured();
    candidate.changes.push(change("src/engine.py", "low"));
    let selection = collect(&source, &snapshot, &candidate, &policy, "2026-11-01", None).unwrap();
    assert_eq!(
        selection.contract_targets,
        ["python_contracts_one", "python_contracts_two"]
    );
    assert_eq!(selection.evidence["registry_scope"], "candidate_tree");
    assert_eq!(selection.contract_test_paths.len(), 2);
}
#[test]
fn unavailable_registry_and_bad_snapshots_fail_closed() {
    let f = Fixture::new();
    f.write("src/engine.py", "value=1\n");
    f.write("tests/test_a.py", "def test_a(): pass\n");
    let (source, snapshot, mut candidate, policy) = f.captured();
    candidate.changes.push(change("src/engine.py", "low"));
    let selection = collect(&source, &snapshot, &candidate, &policy, "2026-11-01", None).unwrap();
    assert_eq!(selection.result.status, CheckStatus::Error);
    assert!(selection
        .result
        .findings
        .iter()
        .any(|f| f.rule_id == "contract-registry-unavailable"));
    drop(selection);
    candidate.tree_oid = "1".repeat(40);
    assert!(collect(&source, &snapshot, &candidate, &policy, "2026-11-01", None).is_err());
}

#[test]
fn rust_source_selects_captured_crate_tests_without_claiming_execution() {
    let f = Fixture::new();
    f.write(
        "crate/Cargo.toml",
        "[package]\nname='fixture'\nversion='0.1.0'\n",
    );
    f.write(
        "crate/src/lib.rs",
        "pub fn f() {}\n#[cfg(test)] mod tests { #[test] fn works() {} }\n",
    );
    let (source, snapshot, mut candidate, policy) = f.captured();
    candidate.changes.push(change("crate/src/lib.rs", "normal"));
    let selection = collect(&source, &snapshot, &candidate, &policy, "2026-11-01", None).unwrap();
    assert_eq!(
        selection.native_tests["crate/src/lib.rs"],
        json!(["crate/src/lib.rs"])
    );
    assert_eq!(selection.evidence["test_execution_verified"], false);
    assert_eq!(selection.result.status, CheckStatus::Passed);
}

#[test]
fn native_source_without_crate_mapping_does_not_get_credit_for_broad_python_fallback() {
    let f = Fixture::new();
    f.write("crate/missing.rs", "pub fn f() {}\n");
    f.write("tests/test_a.py", "def test_a(): pass\n");
    let (source, snapshot, mut candidate, policy) = f.captured();
    candidate.changes.push(change("crate/missing.rs", "normal"));
    let selection = collect(&source, &snapshot, &candidate, &policy, "2026-11-01", None).unwrap();
    assert_eq!(selection.result.status, CheckStatus::Error);
    assert!(selection
        .result
        .findings
        .iter()
        .any(|f| f.rule_id == "native-test-targets-unverified"));
}

#[test]
fn nested_crate_tests_do_not_claim_coverage_for_parent_crate() {
    let f = Fixture::new();
    f.write(
        "crate/Cargo.toml",
        "[package]\nname='parent'\nversion='0.1.0'\n",
    );
    f.write("crate/src/lib.rs", "pub fn f() {}\n");
    f.write(
        "crate/nested/Cargo.toml",
        "[package]\nname='nested'\nversion='0.1.0'\n",
    );
    f.write(
        "crate/nested/src/lib.rs",
        "#[cfg(test)] mod tests { #[test] fn works() {} }\n",
    );
    let (source, snapshot, mut candidate, policy) = f.captured();
    candidate.changes.push(change("crate/src/lib.rs", "normal"));
    let selection = collect(&source, &snapshot, &candidate, &policy, "2026-11-01", None).unwrap();
    assert_eq!(selection.result.status, CheckStatus::Error);
    assert!(selection
        .result
        .findings
        .iter()
        .any(|f| f.rule_id == "native-test-targets-unverified"));
}

#[test]
fn selected_symlink_test_is_refused_instead_of_silently_omitted() {
    use std::os::unix::fs::symlink;
    let f = Fixture::new();
    f.write("pkg/source.py", "value=1\n");
    f.write("tests/test_real.py", "def test_real(): pass\n");
    symlink("test_real.py", f.root.join("repo/tests/test_link.py")).unwrap();
    let mut index = f.repo.index().unwrap();
    index.add_path(Path::new("tests/test_link.py")).unwrap();
    index.write().unwrap();
    let (source, snapshot, mut candidate, policy) = f.captured();
    candidate.changes.push(change("pkg/source.py", "normal"));
    assert!(collect(&source, &snapshot, &candidate, &policy, "2026-11-01", None).is_err());
}

#[test]
fn confined_git_policy_symlink_is_supported_and_link_drift_refused() {
    use std::os::unix::fs::symlink;
    let f = Fixture::new();
    f.write("policies/actual.toml", POLICY);
    f.write("policies/other.toml", POLICY);
    f.write("pkg/source.py", "value=1\n");
    f.write("tests/test_a.py", "def test_a(): pass\n");
    let link = f.root.join("repo/candidate.toml");
    fs::remove_file(&link).unwrap();
    symlink("policies/actual.toml", &link).unwrap();
    let mut index = f.repo.index().unwrap();
    index.add_path(Path::new("candidate.toml")).unwrap();
    index.write().unwrap();
    let (source, snapshot, mut candidate, policy) = f.captured();
    candidate.changes.push(change("pkg/source.py", "normal"));
    let selection = collect(&source, &snapshot, &candidate, &policy, "2026-11-01", None).unwrap();
    assert_eq!(selection.result.status, CheckStatus::Passed);
    let snapshot_link = snapshot.path().join("candidate.toml");
    fs::remove_file(&snapshot_link).unwrap();
    symlink("policies/other.toml", snapshot_link).unwrap();
    assert!(selection.verify().is_err());
}

#[test]
fn public_selection_output_mutation_is_rejected_for_every_consumer() {
    let f = Fixture::new();
    f.write("pkg/source.py", "value=1\n");
    f.write("tests/test_a.py", "def test_a(): pass\n");
    let (source, snapshot, mut candidate, policy) = f.captured();
    candidate.changes.push(change("pkg/source.py", "low"));
    let mut selection =
        collect(&source, &snapshot, &candidate, &policy, "2026-11-01", None).unwrap();
    selection.verify().unwrap();
    let paths = selection
        .selected_evidence_paths(&source, &snapshot, &candidate, &policy)
        .unwrap();
    assert_eq!(paths, ["tests/test_a.py"]);
    let original = selection.python_tests.clone();
    selection.python_tests.clear();
    assert!(selection.verify().is_err());
    assert!(selection
        .selected_evidence_paths(&source, &snapshot, &candidate, &policy)
        .is_err());
    selection.python_tests = original;
    let original = selection.evidence.clone();
    selection.evidence["test_execution_verified"] = json!(true);
    assert!(selection.verify().is_err());
    selection.evidence = original;
    let original = selection.result.clone();
    selection.result.status = CheckStatus::Cached;
    assert!(selection.verify().is_err());
    selection.result = original;
    selection.verify().unwrap();
    let mut other = candidate.clone();
    other.base_tree_oid = "a".repeat(40);
    assert!(selection
        .selected_evidence_paths(&source, &snapshot, &other, &policy)
        .is_err());
}
