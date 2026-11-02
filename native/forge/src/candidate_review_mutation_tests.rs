use super::*;
use crate::{candidate_snapshot::Limits, candidate_source::Kind};
use std::fs;
const TODAY: &str = "2026-11-01";
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
python=['**/*.py']
test=['tests/**']
native=['**/*.rs']
[risk]
high=[]
[paths]
protected_deletes=[]
hot=[]
generated=[]
[checks.mutation-evidence]
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
        let root = std::env::temp_dir().join(crate::native_id::new("mutation-collector").unwrap());
        fs::create_dir(&root).unwrap();
        let repo = git2::Repository::init(root.join("repo")).unwrap();
        let this = Self { root, repo };
        this.write("policy.toml", POLICY);
        this.commit();
        this
    }
    fn write(&self, path: &str, text: &str) {
        let output = self.root.join("repo").join(path);
        fs::create_dir_all(output.parent().unwrap()).unwrap();
        fs::write(output, text).unwrap();
        let mut index = self.repo.index().unwrap();
        index.add_path(Path::new(path)).unwrap();
        index.write().unwrap();
    }
    fn commit(&self) {
        let tree = self.repo.index().unwrap().write_tree().unwrap();
        let tree = self.repo.find_tree(tree).unwrap();
        let sig = git2::Signature::now("test", "test@example.invalid").unwrap();
        let parent = self.repo.head().ok().map(|h| h.peel_to_commit().unwrap());
        self.repo
            .commit(
                Some("HEAD"),
                &sig,
                &sig,
                "fixture",
                &tree,
                &parent.iter().collect::<Vec<_>>(),
            )
            .unwrap();
    }
    fn captured(&self) -> (Source, Snapshot, Candidate, Policy) {
        let source = Source::open(&self.root.join("repo")).unwrap();
        let candidate = source.resolve(Kind::Index, "HEAD", None).unwrap();
        let snapshot = Snapshot::create(
            &source,
            &candidate.tree_oid,
            &self.root.join(crate::native_id::new("capture").unwrap()),
            Limits::default(),
        )
        .unwrap();
        let policy =
            crate::candidate_policy::load(&snapshot.path(), Some("policy.toml"), TODAY).unwrap();
        (source, snapshot, candidate, policy)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
fn context<'a>(
    source: &'a Source,
    snapshot: &'a Snapshot,
    candidate: &'a Candidate,
    policy: &'a Policy,
) -> Context<'a> {
    Context {
        source,
        snapshot,
        candidate,
        policy,
        today: TODAY,
        runner: None,
        scratch_exclusions: &[],
    }
}
#[test]
fn non_test_candidate_needs_neither_runner_registry_nor_anchor() {
    let f = Fixture::new();
    f.write("notes.md", "documentation\n");
    let (source, snapshot, candidate, policy) = f.captured();
    let evidence = collect(context(&source, &snapshot, &candidate, &policy)).unwrap();
    assert_eq!(evidence.result.status, CheckStatus::Skipped);
    assert!(evidence.result.findings.is_empty());
    assert!(evidence.requests().is_empty());
    assert!(evidence.probe().is_none());
    evidence.verify().unwrap();
}
#[test]
fn missing_grandfather_and_registry_are_critical_and_never_pass() {
    let f = Fixture::new();
    f.write("tests/test_new.py", "def test_new():\n    assert True\n");
    let (source, snapshot, candidate, policy) = f.captured();
    let evidence = collect(context(&source, &snapshot, &candidate, &policy)).unwrap();
    assert_eq!(evidence.result.status, CheckStatus::Failed);
    for rule in ["grandfather-inventory-invalid", "mutation-registry-missing"] {
        assert!(evidence
            .result
            .findings
            .iter()
            .any(|f| f.rule_id == rule && f.severity == candidate_assessment::Severity::Critical));
    }
    assert!(evidence
        .result
        .findings
        .iter()
        .all(|f| f.fingerprint.len() == 24));
}
#[test]
fn observed_outputs_and_snapshot_bytes_are_reverified() {
    let f = Fixture::new();
    f.write("tests/test_new.py", "def test_new():\n    assert True\n");
    let (source, snapshot, candidate, policy) = f.captured();
    let mut evidence = collect(context(&source, &snapshot, &candidate, &policy)).unwrap();
    evidence.result.findings.clear();
    assert!(evidence.verify().is_err());
    let mut evidence = collect(context(&source, &snapshot, &candidate, &policy)).unwrap();
    evidence.evidence["approval"] = json!(true);
    assert!(evidence.verify().is_err());
    let evidence = collect(context(&source, &snapshot, &candidate, &policy)).unwrap();
    fs::write(snapshot.path().join("policy.toml"), "changed").unwrap();
    assert!(evidence.verify().is_err());
}
#[test]
fn captured_policy_and_candidate_are_not_caller_assertions() {
    let f = Fixture::new();
    f.write("note.md", "one\n");
    let (source, snapshot, mut candidate, mut policy) = f.captured();
    candidate.changes.clear();
    assert!(collect(context(&source, &snapshot, &candidate, &policy)).is_err());
    let candidate = source.resolve(Kind::Index, "HEAD", None).unwrap();
    policy.document["block_at"] = json!("critical");
    assert!(collect(context(&source, &snapshot, &candidate, &policy)).is_err());
}
#[test]
fn immutable_git_reader_observes_blobs_not_dirty_host_files() {
    let f = Fixture::new();
    f.write("a.txt", "captured\n");
    f.commit();
    let (source, _snapshot, candidate, _policy) = f.captured();
    fs::write(f.root.join("repo/a.txt"), "dirty host\n").unwrap();
    let mut git = GitInputs::new(&source);
    assert_eq!(
        git.read(&candidate.tree_oid, "a.txt").unwrap(),
        b"captured\n"
    );
    let commit = source
        .repository()
        .head()
        .unwrap()
        .peel_to_commit()
        .unwrap()
        .id()
        .to_string();
    git.anchor(&commit, &candidate.tree_oid).unwrap();
    assert!(git.anchor(&commit, &"0".repeat(40)).is_err());
    git.verify().unwrap();
}
#[test]
fn exact_value_waiver_preserves_visible_reason_and_expires() {
    let f = Fixture::new();
    let (_source, _snapshot, mut candidate, mut policy) = f.captured();
    candidate.integration_base_oid = Some("a".repeat(40));
    policy.document["value_waivers"] = json!([{"integration_base":"a".repeat(40),"nodeids":["tests/test_a.py::test_a"],"reason":"external engine contract",
        "approved_by":"operator","approved_on":"2026-10-01","expires":"2026-11-02"}]);
    let mut finding = finding(
        "new-test-value-not-admitted",
        "value missing".into(),
        Some("tests/test_a.py"),
    )
    .unwrap();
    finding
        .evidence
        .insert("nodeid".into(), json!("tests/test_a.py::test_a"));
    let mut rows = vec![finding.clone()];
    waivers::apply(&mut rows, &policy, &candidate, TODAY).unwrap();
    assert_eq!(rows[0].rule_id, "new-test-value-waived");
    assert!(rows[0].message.starts_with("WAIVED"));
    let mut rows = vec![finding];
    waivers::apply(&mut rows, &policy, &candidate, "2026-11-03").unwrap();
    assert_eq!(rows[0].rule_id, "new-test-value-not-admitted");
}
#[test]
fn contextual_failed_or_stale_originals_cannot_admit_source() {
    // Original automatic receipt remains unchanged and is NOT complete PASS evidence.
    let raw: Value = serde_json::from_str(include_str!(
        "../../conductor-native/tests/fixtures/mutation_verifier/original-receipt.json"
    ))
    .unwrap();
    let expanded = conductor_native::receipt_slim::expand_receipt(&raw).unwrap();
    assert!(!context::complete(&expanded).is_empty());
    let attempt = conductor_native::mutation_evidence::CapturedAttempt {
        campaign_id: raw["campaign_id"].as_str().unwrap().into(),
        manifest: "fixture".into(),
        receipt: "fixture.json".into(),
        sha256: hex(b"original"),
        expanded,
        valid: false,
        bindings_current: true,
        errors: json!([]),
    };
    let errors =
        context::errors("tests/test_a.py::test_a", &["src/a.py".into()], &[attempt]).unwrap();
    assert_eq!(errors.len(), 1);
    assert!(errors[0].contains("latest attempt"));
    assert_eq!(
        context::errors("tests/test_a.py::test_a", &["src/a.py".into()], &[])
            .unwrap()
            .len(),
        1
    );
}
