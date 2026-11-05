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
        native_runner_pin: None,
        stamp: None,
        scratch_exclusions: &[],
        grandfather: None,
        waiver_policy_sha256: None,
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
fn strict_default_requires_registry_without_historical_anchor() {
    let f = Fixture::new();
    f.write("tests/test_new.py", "def test_new():\n    assert True\n");
    let (source, snapshot, candidate, policy) = f.captured();
    let evidence = collect(context(&source, &snapshot, &candidate, &policy)).unwrap();
    assert_eq!(evidence.result.status, CheckStatus::Failed);
    assert!(evidence
        .result
        .findings
        .iter()
        .any(|f| f.rule_id == "mutation-registry-missing"
            && f.severity == candidate_assessment::Severity::Critical));
    assert!(evidence
        .result
        .findings
        .iter()
        .all(|f| f.fingerprint.len() == 24));
}
#[test]
fn unborn_repository_strictly_gates_added_test() {
    let root = std::env::temp_dir().join(crate::native_id::new("mutation-unborn").unwrap());
    fs::create_dir(&root).unwrap();
    let repo = git2::Repository::init(root.join("repo")).unwrap();
    let f = Fixture { root, repo };
    f.write("policy.toml", POLICY);
    f.write(
        "tests/test_first.py",
        "def test_first():\n    assert True\n",
    );
    let (source, snapshot, candidate, policy) = f.captured();
    assert!(candidate.base_commit_oid.is_none());
    let evidence = collect(context(&source, &snapshot, &candidate, &policy)).unwrap();
    assert_eq!(
        evidence.evidence["gated_nodeids"]["tests/test_first.py"],
        json!(["tests/test_first.py::test_first"])
    );
    assert!(!evidence
        .result
        .findings
        .iter()
        .any(|v| v.rule_id == "grandfather-inventory-invalid"));
}
#[test]
fn strict_mode_gates_every_definition_in_changed_test_even_if_unchanged() {
    let f = Fixture::new();
    f.write(
        "tests/test_sample.py",
        "def test_old():\n    assert True\n\ndef test_changed():\n    assert True\n",
    );
    f.write("tests/legacy.test.js", "test('old', () => {});\n");
    f.write(
        "tests/test_unchanged.py",
        "def test_unchanged():\n    assert True\n",
    );
    f.commit();
    f.write(
        "tests/test_sample.py",
        "def test_old():\n    assert True\n\ndef test_changed():\n    assert 2 == 2\n",
    );
    f.write(
        "tests/legacy.test.js",
        "test('old', () => { return 1; });\n",
    );
    f.write(
        "pyproject.toml",
        "[tool.conductor.mutation_grandfather]\ncommit='candidate-cannot-set-this'\n",
    );
    let (source, snapshot, candidate, policy) = f.captured();
    let evidence = collect(context(&source, &snapshot, &candidate, &policy)).unwrap();
    assert_eq!(
        evidence.evidence["gated_nodeids"]["tests/test_sample.py"],
        json!([
            "tests/test_sample.py::test_changed",
            "tests/test_sample.py::test_old"
        ])
    );
    assert_eq!(
        evidence.evidence["gated_nodeids"]["tests/legacy.test.js"],
        json!(["tests/legacy.test.js"])
    );
    assert!(evidence.evidence["gated_nodeids"]
        .get("tests/test_unchanged.py")
        .is_none());
    assert!(!evidence
        .result
        .findings
        .iter()
        .any(|v| v.rule_id == "grandfather-inventory-invalid"));
}
#[test]
fn explicit_operator_baseline_requires_exact_inventory_and_ancestor() {
    let f = Fixture::new();
    f.write("tests/test_sample.py", "def test_old():\n    assert True\n");
    f.commit();
    let commit = f.repo.head().unwrap().peel_to_commit().unwrap();
    let inventory = json!({"schema":"conductor.candidate_review.grandfather_inventory/v1", "anchor_commit":commit.id().to_string(), "milestone":"fixture", "tests":{"tests/test_sample.py":["test_old"]}}).to_string();
    let pin = crate::candidate_execution_workspace::MutationGrandfather {
        commit: commit.id().to_string(),
        tree: commit.tree_id().to_string(),
        inventory_path: "baseline.json".into(),
        inventory_sha256: hex(inventory.as_bytes()),
    };
    f.write("baseline.json", &inventory);
    f.write(
        "tests/test_sample.py",
        "def test_old():\n    assert False\n\ndef test_new():\n    assert True\n",
    );
    let (source, snapshot, candidate, policy) = f.captured();
    let mut ctx = context(&source, &snapshot, &candidate, &policy);
    ctx.grandfather = Some(&pin);
    let evidence = collect(ctx).unwrap();
    assert_eq!(
        evidence.evidence["gated_nodeids"]["tests/test_sample.py"],
        json!(["tests/test_sample.py::test_new"])
    );
    assert!(!evidence
        .result
        .findings
        .iter()
        .any(|v| v.rule_id == "grandfather-inventory-invalid"));
    let mut wrong = pin.clone();
    wrong.inventory_sha256 = "0".repeat(64);
    let mut ctx = context(&source, &snapshot, &candidate, &policy);
    ctx.grandfather = Some(&wrong);
    let evidence = collect(ctx).unwrap();
    assert!(evidence
        .result
        .findings
        .iter()
        .any(|v| v.rule_id == "grandfather-inventory-invalid"));
    wrong = pin.clone();
    wrong.inventory_path = "missing.json".into();
    let mut ctx = context(&source, &snapshot, &candidate, &policy);
    ctx.grandfather = Some(&wrong);
    let evidence = collect(ctx).unwrap();
    assert!(evidence
        .result
        .findings
        .iter()
        .any(|v| v.rule_id == "grandfather-inventory-invalid"));
    wrong = pin.clone();
    wrong.tree = "0".repeat(40);
    let mut ctx = context(&source, &snapshot, &candidate, &policy);
    ctx.grandfather = Some(&wrong);
    let evidence = collect(ctx).unwrap();
    assert!(evidence
        .result
        .findings
        .iter()
        .any(|v| v.rule_id == "grandfather-inventory-invalid"));
    let sig = git2::Signature::now("test", "test@example.invalid").unwrap();
    let tree = f.repo.find_tree(commit.tree_id()).unwrap();
    let detached = f
        .repo
        .commit(None, &sig, &sig, "unrelated", &tree, &[])
        .unwrap();
    wrong = pin.clone();
    wrong.commit = detached.to_string();
    let mut git = GitInputs::new(&source);
    assert!(git
        .anchor_ancestor(&wrong.commit, &wrong.tree, &pin.commit)
        .is_err());
}
#[test]
fn candidate_policy_cannot_define_operator_grandfather_pins() {
    let f = Fixture::new();
    let forged = POLICY.replacen(
        "[classes]",
        "mutation_grandfather = { commit = 'fake' }\n[classes]",
        1,
    );
    f.write("policy.toml", &forged);
    let source = Source::open(&f.root.join("repo")).unwrap();
    let candidate = source.resolve(Kind::Index, "HEAD", None).unwrap();
    let snapshot = Snapshot::create(
        &source,
        &candidate.tree_oid,
        &f.root.join("forged"),
        Limits::default(),
    )
    .unwrap();
    assert!(crate::candidate_policy::load(&snapshot.path(), Some("policy.toml"), TODAY).is_err());
}
#[test]
fn untrusted_candidate_waivers_are_inactive_with_operator_pin_reason() {
    let mut states = json!([{"active":true,"reason":"","path":"tests/test_one.py"}]);
    waivers::deny_candidate_waivers(&mut states).unwrap();
    assert_eq!(states[0]["active"], false);
    assert!(states[0]["reason"]
        .as_str()
        .unwrap()
        .contains("not pinned by operator grants"));
}
#[test]
fn mutation_and_value_waivers_require_the_exact_operator_policy_pin() {
    let digest = "a".repeat(64);
    let other = "b".repeat(64);
    for document in [
        json!({"mutation_waivers":[{}],"value_waivers":[]}),
        json!({"mutation_waivers":[],"value_waivers":[{}]}),
        json!({"mutation_waivers":[{}],"value_waivers":[{}]}),
    ] {
        assert!(!waiver_authorized(&document, &digest, None).unwrap());
        assert!(!waiver_authorized(&document, &digest, Some(&other)).unwrap());
        assert!(waiver_authorized(&document, &digest, Some(&digest)).unwrap());
    }
    let no_exemptions = json!({"mutation_waivers":[],"value_waivers":[]});
    assert!(waiver_authorized(&no_exemptions, &digest, None).unwrap());
}
#[test]
fn strict_rename_gates_destination_and_deletion_adds_no_exemption() {
    let f = Fixture::new();
    f.write("tests/test_old.py", "def test_old():\n    assert True\n");
    f.write(
        "tests/test_deleted.py",
        "def test_deleted():\n    assert True\n",
    );
    f.commit();
    f.write(
        "tests/test_renamed.py",
        "def test_old():\n    assert True\n",
    );
    for path in ["tests/test_old.py", "tests/test_deleted.py"] {
        fs::remove_file(f.root.join("repo").join(path)).unwrap();
        let mut index = f.repo.index().unwrap();
        index.remove_path(Path::new(path)).unwrap();
        index.write().unwrap();
    }
    let (source, snapshot, candidate, policy) = f.captured();
    let evidence = collect(context(&source, &snapshot, &candidate, &policy)).unwrap();
    assert_eq!(
        evidence.evidence["gated_nodeids"]["tests/test_renamed.py"],
        json!(["tests/test_renamed.py::test_old"])
    );
    assert!(evidence.evidence["gated_nodeids"]
        .get("tests/test_deleted.py")
        .is_none());
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
    assert!(!waiver_authorized(&policy.document, &policy.digest, None).unwrap());
    assert!(waiver_authorized(&policy.document, &policy.digest, Some(&policy.digest)).unwrap());
    assert!(!waiver_authorized(&policy.document, &"f".repeat(64), Some(&policy.digest)).unwrap());
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

fn contextual_witness(
    source: &str,
    class: &str,
    stamp: &str,
) -> conductor_native::mutation_evidence::CapturedAttempt {
    let node = "tests/test_flow.py::test_flow";
    conductor_native::mutation_evidence::CapturedAttempt {
        campaign_id: source.into(),
        manifest: "manifest".into(),
        receipt: "receipt".into(),
        sha256: "a".repeat(64),
        valid: true,
        bindings_current: true,
        errors: json!([]),
        expanded: json!({"campaign_id": source, "manifest_sha256": "a", "source_sha256": {source: "a"},
            "test_sha256": {"tests/test_flow.py": "a"}, "runner_sha256": "a", "runner_components_sha256": {},
            "generated_at": stamp, "status": "PASS", "baseline": {"returncode": 0, "timed_out": false},
            "outcome_counts": {"KILLED": 1, "NO_COVERAGE": 0, "SURVIVED": 0, "TIMED_OUT": 0, "ERROR": 0, "UNVIABLE": 0},
            "attribution": {"status": "ATTRIBUTED", "attributed_mutants": 1, "killed_mutants": 1},
            "mutants": [{"id": "m", "path": source, "outcome": "KILLED"}],
            "test_value": {"schema_version": "llm.mutation-testing.test-value.v1", "status": "PASS",
                "tests": [{"nodeid": node, "classification": class, "killed_mutants": ["m"]}]}}),
    }
}

#[test]
fn contextual_sources_require_independent_current_positive_witnesses() {
    let node = "tests/test_flow.py::test_flow";
    let a = contextual_witness("train.py", "CORE", "2026-10-05T00:00:00+00:00");
    let b = contextual_witness("selection.py", "MERGE", "2026-10-05T01:00:00+00:00");
    assert!(context::errors(
        node,
        &["train.py".into()],
        &[
            contextual_witness("train.py", "CORE", "2026-10-05T00:00:00+00:00"),
            contextual_witness("selection.py", "MERGE", "2026-10-05T01:00:00+00:00")
        ]
    )
    .unwrap()
    .is_empty());
    let debt = context::errors(
        node,
        &["train.py".into(), "selection.py".into()],
        &[
            contextual_witness("train.py", "CORE", "2026-10-05T00:00:00+00:00"),
            b,
        ],
    )
    .unwrap();
    assert_eq!(debt.len(), 1);
    assert!(debt[0].contains("selection.py"));
    let mut stale = a;
    stale.bindings_current = false;
    assert_eq!(
        context::errors(node, &["train.py".into()], &[stale])
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn contextual_latest_failed_and_contradictory_witnesses_fail_closed() {
    let node = "tests/test_flow.py::test_flow";
    let a = contextual_witness("train.py", "CORE", "2026-10-05T00:00:00+00:00");
    let mut failed = contextual_witness("train.py", "CORE", "2026-10-05T01:00:00+00:00");
    failed.valid = false;
    assert_eq!(
        context::errors(
            node,
            &["train.py".into()],
            &[
                contextual_witness("train.py", "CORE", "2026-10-05T00:00:00+00:00"),
                failed
            ]
        )
        .unwrap()
        .len(),
        1
    );
    let contradictory = contextual_witness("train.py", "MERGE", "2026-10-05T00:00:00+00:00");
    let errors = context::errors(node, &["train.py".into()], &[a, contradictory]).unwrap();
    assert_eq!(errors.len(), 1);
    assert!(errors[0].contains("conflicting"));
}

#[test]
fn contextual_missing_node_and_incomplete_attribution_do_not_admit() {
    let node = "tests/test_flow.py::test_flow";
    let mut missing = contextual_witness("train.py", "CORE", "2026-10-05T00:00:00+00:00");
    missing.expanded["test_value"]["tests"] = json!([]);
    assert_eq!(
        context::errors(node, &["train.py".into()], &[missing])
            .unwrap()
            .len(),
        1
    );
    let mut partial = contextual_witness("train.py", "CORE", "2026-10-05T00:00:00+00:00");
    partial.expanded["outcome_counts"]["TIMED_OUT"] = json!(1);
    partial.expanded["attribution"]["status"] = json!("NO_ATTRIBUTION");
    assert!(!context::complete(&partial.expanded).is_empty());
    assert_eq!(
        context::errors(node, &["train.py".into()], &[partial])
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn mutation_waiver_reads_captured_test_and_source_hashes_and_base() {
    let f = Fixture::new();
    f.write("tests/test_a.py", "test bytes\n");
    f.write("src/dependency.py", "source bytes\n");
    let (source, snapshot, _, policy) = f.captured();
    let candidate = source.resolve(Kind::Index, "HEAD", Some("HEAD")).unwrap();
    let mut waiver_policy = Policy {
        path: policy.path.clone(),
        digest: policy.digest.clone(),
        document: policy.document.clone(),
    };
    waiver_policy.document["mutation_waivers"] = json!([{
        "waiver_id": "fixture", "path": "tests/test_a.py", "sha256": hex(b"test bytes\n"),
        "integration_base": candidate.waiver_base().unwrap(), "expires": "2099-12-31",
        "sources": [{"path": "src/dependency.py", "sha256": hex(b"source bytes\n")}]
    }]);
    let run = |waiver: &Policy| {
        let mut capture =
            inputs::Capture::new(&source, &snapshot, &candidate, &policy, TODAY).unwrap();
        waivers::receipt_states(&mut capture, &candidate, waiver).unwrap()
    };
    assert_eq!(run(&waiver_policy)[0]["active"], true);
    waiver_policy.document["mutation_waivers"][0]["sources"][0]["sha256"] = json!("0".repeat(64));
    assert_eq!(run(&waiver_policy)[0]["active"], false);
    waiver_policy.document["mutation_waivers"][0]["sources"][0]["sha256"] =
        json!(hex(b"source bytes\n"));
    waiver_policy.document["mutation_waivers"][0]["sha256"] = json!("0".repeat(64));
    assert_eq!(run(&waiver_policy)[0]["active"], false);
    waiver_policy.document["mutation_waivers"][0]["sha256"] = json!(hex(b"test bytes\n"));
    waiver_policy.document["mutation_waivers"][0]["integration_base"] = json!("0".repeat(40));
    assert_eq!(run(&waiver_policy)[0]["active"], false);
}

#[test]
fn held_candidate_native_inventory_matches_and_detects_snapshot_drift() {
    use conductor_native::source_fingerprint::{self, EXTRA_FILES, SOURCE_TREES};
    let fixture = Fixture::new();
    for name in SOURCE_TREES {
        for suffix in ["Cargo.toml", "Cargo.lock", "src/lib.rs"] {
            fixture.write(&format!("native/{name}/{suffix}"), "original");
        }
    }
    for path in EXTRA_FILES {
        fixture.write(path, "original");
    }
    fixture.write("native/forge/src/process_group.rs", "original");
    let (source, snapshot, candidate, policy) = fixture.captured();
    let root = fixture.root.join("repo");
    let paths = source_fingerprint::files(&root).unwrap();
    // String ordering puts forge-cli before forge/, unlike Path component ordering.
    let forge = paths
        .iter()
        .position(|path| path == Path::new("native/forge/src/lib.rs"))
        .unwrap();
    let cli = paths
        .iter()
        .position(|path| path == Path::new("native/forge-cli/build.rs"))
        .unwrap();
    assert!(forge < cli);
    let expected = source_fingerprint::fingerprint(&root, &paths).unwrap();
    let added = snapshot.path().join("native/forge/src/untracked.rs");
    fs::write(&added, "untracked").unwrap();
    let mut rejected =
        inputs::Capture::new(&source, &snapshot, &candidate, &policy, TODAY).unwrap();
    assert!(rejected.native_source_fingerprint().is_err());
    drop(rejected);
    fs::remove_file(&added).unwrap();
    let mut captured =
        inputs::Capture::new(&source, &snapshot, &candidate, &policy, TODAY).unwrap();
    assert_eq!(
        captured.native_source_fingerprint().unwrap(),
        Some(expected)
    );
    captured.verify().unwrap();
    fs::write(&added, "untracked").unwrap();
    assert!(captured.verify().is_err());
    fs::remove_file(added).unwrap();
    captured.verify().unwrap();
    fs::write(
        snapshot.path().join("native/forge/src/process_group.rs"),
        "changed",
    )
    .unwrap();
    assert!(captured.verify().is_err());
}

#[test]
fn native_receipt_detection_uses_decoded_runner_kind_only() {
    // These marker objects test dispatch only, and cannot pass receipt validation.
    for (text, native) in [
        (r#"{"runner_kind":"forge\u002dnative"}"#, true),
        (r#"{"runner_kind":"legacy","note":"forge-native"}"#, false),
    ] {
        let fixture = Fixture::new();
        fixture.write("receipts/marker.json", text);
        let (source, snapshot, candidate, policy) = fixture.captured();
        let mut capture = Capture::new(&source, &snapshot, &candidate, &policy, TODAY).unwrap();
        let selected = registry::Registry {
            path: "unused".into(),
            payload: json!({"receipt_directories":["receipts"]}),
            manifests: vec![],
        };
        let catalog = registry::catalog(&mut capture, &selected, &[]).unwrap();
        assert_eq!(catalog.has_native_receipts, native);
        assert_eq!(catalog.receipts.len(), 1);
        assert!(catalog.malformed.is_empty());
    }
}

#[test]
fn installed_candidate_requires_held_complete_inventory_absence() {
    let fixture = Fixture::new();
    fixture.write("conductor.toml", "schema_version=1\n");
    let (source, snapshot, candidate, policy) = fixture.captured();
    let mut capture = Capture::new(&source, &snapshot, &candidate, &policy, TODAY).unwrap();
    assert!(capture.native_source_fingerprint().unwrap().is_none());
    assert!(capture.native_inventory_absent().unwrap());
    capture.verify().unwrap();
    fs::create_dir_all(snapshot.path().join("native/forge")).unwrap();
    assert!(capture.verify().is_err());
    fixture.write("native/forge/src/lib.rs", "partial");
    let (source, snapshot, candidate, policy) = fixture.captured();
    let mut capture = Capture::new(&source, &snapshot, &candidate, &policy, TODAY).unwrap();
    assert!(capture.native_source_fingerprint().unwrap().is_none());
    assert!(!capture.native_inventory_absent().unwrap());
}
