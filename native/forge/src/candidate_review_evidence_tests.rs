use super::*;
use crate::candidate_snapshot::Limits;
use crate::candidate_source::Kind;
use conductor_native::performance_receipt::{
    self as perf, Evidence as Performance, Identity, Receipt, Sample,
};
use std::{
    fs,
    path::{Path, PathBuf},
};
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
novel=['novel/**']
test=['tests/**']
[risk]
high=[]
[paths]
protected_deletes=[]
hot=['hot/**']
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
        let root = std::env::temp_dir().join(crate::native_id::new("review-evidence").unwrap());
        fs::create_dir(&root).unwrap();
        let repo = git2::Repository::init(root.join("repo")).unwrap();
        let f = Self { root, repo };
        f.write("policy.toml", POLICY);
        f.commit();
        f
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
        let signature = git2::Signature::now("test", "test@example.invalid").unwrap();
        let head = self
            .repo
            .head()
            .ok()
            .map(|head| head.peel_to_commit().unwrap());
        self.repo
            .commit(
                Some("HEAD"),
                &signature,
                &signature,
                "fixture",
                &tree,
                &head.iter().collect::<Vec<_>>(),
            )
            .unwrap();
    }
    fn captured(&self) -> (Source, Snapshot, Candidate, Policy) {
        let source = Source::open(&self.root.join("repo")).unwrap();
        let candidate = source.resolve(Kind::Index, "HEAD", None).unwrap();
        let snapshot = Snapshot::create(
            &source,
            &candidate.tree_oid,
            &self.root.join(crate::native_id::new("snapshot").unwrap()),
            Limits::default(),
        )
        .unwrap();
        let policy =
            crate::candidate_policy::load(&snapshot.path(), Some("policy.toml"), TODAY).unwrap();
        (source, snapshot, candidate, policy)
    }
    fn receipt(&self, source: &str) -> Performance {
        let samples = vec![
            Sample {
                wall_ms: 2.0,
                cpu_ms: 1.0,
                max_rss_bytes: 1024
            };
            5
        ];
        let mut current = Receipt {
            schema: perf::SCHEMA.into(),
            created_unix: 1,
            identity: Identity {
                host: self
                    .root
                    .join("repo")
                    .canonicalize()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .into(),
                head: "fixture".into(),
                source_files: BTreeMap::from([(
                    "hot/a.py".into(),
                    perf::digest(source.as_bytes()),
                )]),
                command: "bench workload".into(),
                workload_sha256: perf::digest(b"work"),
                environment_sha256: perf::digest(b"env"),
                platform: "linux".into(),
                toolchain: "native".into(),
                warmups: 1,
                timeout_seconds: 5,
            },
            summary: perf::summarize(&samples).unwrap(),
            samples,
            receipt_sha256: String::new(),
        };
        current.seal().unwrap();
        Performance {
            schema: perf::EVIDENCE_SCHEMA.into(),
            baseline: current.clone(),
            current,
            max_regression_percent: 10.0,
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
fn selection<'a>(
    source: &Source,
    snapshot: &'a Snapshot,
    candidate: &'a Candidate,
    policy: &'a Policy,
) -> Selection<'a> {
    candidate_test_selection::collect(source, snapshot, candidate, policy, TODAY, None).unwrap()
}
fn rules<'b>(evidence: &'b Evidence<'_, '_>) -> Vec<&'b str> {
    evidence
        .results
        .iter()
        .flat_map(|row| row.findings.iter().map(|finding| finding.rule_id.as_str()))
        .collect()
}

#[test]
fn no_trigger_is_clean_without_claiming_execution() {
    let f = Fixture::new();
    f.write("docs/readme.md", "ordinary documentation\n");
    let (source, snapshot, candidate, policy) = f.captured();
    let selected = selection(&source, &snapshot, &candidate, &policy);
    let evidence = collect(&source, &snapshot, &candidate, &policy, TODAY, &selected).unwrap();
    assert!(evidence
        .results
        .iter()
        .all(|row| row.status == CheckStatus::Passed && row.files.is_empty()));
    assert_eq!(evidence.evidence["execution_verified"], false);
    assert_eq!(evidence.evidence["approval"], false);
    evidence.verify().unwrap();
}

#[test]
fn changed_triggers_find_missing_budget_provenance_and_selected_device_tests() {
    let f = Fixture::new();
    f.write("hot/a.py", "# performance-critical\nvalue=1\n");
    f.write("novel/a.py", "# promote numerical score\n");
    let (source, snapshot, mut candidate, policy) = f.captured();
    // Reclassification must defeat caller-supplied empty classes.
    for change in &mut candidate.changes {
        change.classes.clear();
    }
    let selected = selection(&source, &snapshot, &candidate, &policy);
    let evidence = collect(&source, &snapshot, &candidate, &policy, TODAY, &selected).unwrap();
    assert_eq!(
        rules(&evidence),
        [
            "missing-performance-budget",
            "python-only-hotpath",
            "incomplete-result-provenance",
            "missing-numerical-device-tests"
        ]
    );
    assert!(evidence
        .results
        .iter()
        .flat_map(|r| &r.findings)
        .all(|f| f.fingerprint.len() == 24));
}

#[test]
fn valid_numerical_receipt_and_selected_captured_tests_satisfy_text_rules_only() {
    let f = Fixture::new();
    f.write("hot/a.py", "# numpy performance-critical\nvalue=1\n");
    f.write(
        "novel/a.py",
        "# promote numerical score; baſeline seed config fingerprint\n",
    );
    f.write("tests/test_device.py", "def test_device_isfinite(): pass\n");
    let receipt = f.receipt("# numpy performance-critical\nvalue=1\n");
    f.write(
        "bench/evidence.json",
        &serde_json::to_string(&receipt).unwrap(),
    );
    let (source, snapshot, candidate, policy) = f.captured();
    let selected = selection(&source, &snapshot, &candidate, &policy);
    let evidence = collect(&source, &snapshot, &candidate, &policy, TODAY, &selected).unwrap();
    assert!(rules(&evidence).is_empty());
    assert_eq!(
        evidence.evidence["selected_test_paths"],
        json!(["tests/test_device.py"])
    );
    let rows = evidence.evidence["inputs"].as_array().unwrap();
    assert!(rows
        .iter()
        .any(|row| row["path"] == "bench/evidence.json"
            && row["sha256"].as_str().unwrap().len() == 64));
    assert_eq!(evidence.evidence["test_execution_verified"], false);
    assert_eq!(evidence.evidence["review_complete"], false);
}

#[test]
fn stale_source_hash_and_corrupted_receipt_are_actionable_findings() {
    for corrupt in [false, true] {
        let f = Fixture::new();
        f.write("hot/a.py", "value=2\n");
        let mut receipt = f.receipt(if corrupt { "value=2\n" } else { "value=1\n" });
        if corrupt {
            receipt.current.receipt_sha256 = "0".repeat(64);
        }
        f.write(
            "bench/evidence.json",
            &serde_json::to_string(&receipt).unwrap(),
        );
        let (source, snapshot, candidate, policy) = f.captured();
        let selected = selection(&source, &snapshot, &candidate, &policy);
        let evidence = collect(&source, &snapshot, &candidate, &policy, TODAY, &selected).unwrap();
        assert_eq!(rules(&evidence), ["missing-performance-budget"]);
        assert!(!evidence.results[0].findings[0].evidence["receipt_errors"]
            .as_array()
            .unwrap()
            .is_empty());
    }
}

#[test]
fn unchanged_numerical_trigger_does_not_arm_unrelated_changed_lines() {
    let f = Fixture::new();
    f.write("novel/a.py", "# device numerical score\nvalue=1\n");
    f.commit();
    f.write("novel/a.py", "# device numerical score\nvalue=2\n");
    let (source, snapshot, candidate, policy) = f.captured();
    let selected = selection(&source, &snapshot, &candidate, &policy);
    let evidence = collect(&source, &snapshot, &candidate, &policy, TODAY, &selected).unwrap();
    assert!(rules(&evidence).is_empty());
    assert_eq!(evidence.results[1].metrics["armed_on"], "changed-lines");
    assert_eq!(
        evidence.evidence["changed_line_ranges"]["files"][0]["ranges"],
        json!([{"start":2,"end":2}])
    );
}

#[test]
fn held_source_test_policy_and_result_drift_are_rejected() {
    for target in ["novel/a.py", "tests/test_device.py", "policy.toml"] {
        let f = Fixture::new();
        f.write("novel/a.py", "# device\n");
        f.write("tests/test_device.py", "def test_device(): pass\n");
        let (source, snapshot, candidate, policy) = f.captured();
        let selected = selection(&source, &snapshot, &candidate, &policy);
        let mut evidence =
            collect(&source, &snapshot, &candidate, &policy, TODAY, &selected).unwrap();
        evidence.results[0].status = CheckStatus::Cached;
        assert!(evidence.verify().is_err());
        evidence.results[0].status = CheckStatus::Passed;
        evidence.verify().unwrap();
        fs::write(snapshot.path().join(target), "drift\n").unwrap();
        assert!(evidence.verify().is_err(), "{target}");
        assert!(collect(&source, &snapshot, &candidate, &policy, TODAY, &selected).is_err());
    }
}

#[test]
fn omitted_change_and_forged_policy_document_are_not_accepted() {
    let f = Fixture::new();
    f.write("novel/a.py", "# device\n");
    let (source, snapshot, mut candidate, mut policy) = f.captured();
    candidate.changes.clear();
    let selected = selection(&source, &snapshot, &candidate, &policy);
    assert!(collect(&source, &snapshot, &candidate, &policy, TODAY, &selected).is_err());
    drop(selected);
    policy.document["hot_path_globs"] = json!([]);
    assert!(Inputs::new(&source, &snapshot, &candidate, &policy, TODAY).is_err());
}

#[test]
fn dirty_originals_do_not_replace_captured_source_or_selected_test_bytes() {
    let f = Fixture::new();
    f.write("novel/a.py", "# device\n");
    f.write("tests/test_device.py", "def test_device(): pass\n");
    let (source, snapshot, candidate, policy) = f.captured();
    let selected = selection(&source, &snapshot, &candidate, &policy);
    fs::write(
        f.root.join("repo/novel/a.py"),
        "# score without provenance\n",
    )
    .unwrap();
    fs::write(f.root.join("repo/tests/test_device.py"), "no tests\n").unwrap();
    let evidence = collect(&source, &snapshot, &candidate, &policy, TODAY, &selected).unwrap();
    assert!(rules(&evidence).is_empty());
    let inputs = evidence.evidence["inputs"].as_array().unwrap();
    let row = inputs
        .iter()
        .find(|row| row["path"] == "novel/a.py")
        .unwrap();
    assert_eq!(row["sha256"], perf::digest(b"# device\n"));
    evidence.verify().unwrap();
}

#[test]
fn duplicate_receipt_json_is_actionable_and_oversized_inputs_are_incomplete() {
    let f = Fixture::new();
    f.write("hot/a.py", "value=1\n");
    f.write(
        "bench/evidence.json",
        "{\"schema\":\"forge.performance-evidence.v1\",\"schema\":\"other\"}",
    );
    let (source, snapshot, candidate, policy) = f.captured();
    let selected = selection(&source, &snapshot, &candidate, &policy);
    let evidence = collect(&source, &snapshot, &candidate, &policy, TODAY, &selected).unwrap();
    assert_eq!(rules(&evidence), ["missing-performance-budget"]);
    assert!(!evidence.results[0].findings[0].evidence["receipt_errors"]
        .as_array()
        .unwrap()
        .is_empty());
    drop(evidence);
    drop(selected);
    snapshot.discard().unwrap();
    f.write("bench/evidence.json", &" ".repeat(2 * 1024 * 1024 + 1));
    let (source, snapshot, candidate, policy) = f.captured();
    let selected = selection(&source, &snapshot, &candidate, &policy);
    assert!(collect(&source, &snapshot, &candidate, &policy, TODAY, &selected).is_err());
}
