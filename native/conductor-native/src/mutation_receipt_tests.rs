use std::path::Path;
use std::path::PathBuf;

use serde_json::{json, Map, Value};

use super::{
    campaign_scope_error, default_anchor_registry_path, exact_parse_error,
    generated_receipt_errors, legacy_receipt_anchor_errors, lineage_accepts, load_receipts,
    receipt_errors, rejection_details, runner_errors, scope_error, sha256_file,
    value_receipt_errors, AnchorConfig, Receipt, Rejection, RejectionKind, RunnerState,
    ValidationContext,
};
use crate::mutation_manifest::{CampaignContract, ValueContract};

const MANIFEST: &str = "conductor/mutation_campaigns/subject_fest_20260906.json";

fn repo_root() -> PathBuf {
    std::env::temp_dir().join(format!(
        "conductor-native-receipt-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ))
}

#[test]
fn binding_fixtures_are_isolated_between_parallel_tests() {
    let current = repo_root();
    assert_eq!(current, repo_root());
    let other = std::thread::spawn(repo_root)
        .join()
        .expect("fixture thread");
    assert_ne!(current, other);
}

fn ensure_binding_files(root: &Path) {
    std::fs::create_dir_all(root.join("conductor")).expect("binding directory");
    for relative in [
        "conductor/mutation_engine_generated.py",
        "conductor/mutation_engine_fest.py",
        "conductor/mutation_run_scope.py",
    ] {
        let path = root.join(relative);
        if !path.exists() {
            std::fs::write(path, "fixture binding\n").expect("binding file");
        }
    }
}

fn campaign() -> CampaignContract {
    serde_json::from_value(json!({
        "campaign_id": "subject_fest_20260906",
        "title": "Subject under fest",
        "language": "python",
        "mutation_engine": "fest",
        "expected_mutations": 0,
        "manifest": MANIFEST,
        "manifest_sha256": "a".repeat(64),
        "source_sha256": {"conductor/subject.py": "b".repeat(64)},
        "source_symbols": {},
        "test_scopes": {},
        "ranked_tests": [],
        "ranked_test_paths": ["conductor/test_subject.py"],
        "planned_mutations": [],
        "mutations": [],
        "test_argv": ["python", "-m", "pytest", "-q", "conductor/test_subject.py"],
        "timeout_seconds": 900,
        "blocked_process_substrings": [],
        "poll_seconds": 0,
        "environment": {},
        "host_read_dependencies": [],
        "value_analysis_payload": null,
        "value_analysis": null,
        "source_drifted": false,
        "generated": true,
        "survivor_baseline": ["constant_replace-abc123456789-0"],
        "test_sha256": {"conductor/test_subject.py": "c".repeat(64)}
    }))
    .expect("campaign contract")
}

/// A receipt that agrees with the campaign on every pinned digest.
fn receipt(overrides: Value) -> Map<String, Value> {
    let campaign = campaign();
    let root = repo_root();
    ensure_binding_files(&root);
    let mut base = json!({
        "schema_version": "llm.mutation-testing.receipt.v3",
        "mutants_are_generated": true,
        "status": "PASS",
        "campaign_id": campaign.campaign_id,
        "manifest": MANIFEST,
        "manifest_sha256": campaign.manifest_sha256,
        "source_sha256": campaign.source_sha256,
        "test_sha256": campaign.test_sha256,
        "core_sha256": sha256_file(&root.join("conductor/mutation_engine_generated.py")),
        "adapter_sha256": sha256_file(&root.join("conductor/mutation_engine_fest.py")),
        "scope_guard_sha256": sha256_file(&root.join("conductor/mutation_run_scope.py")),
        "survivors": ["constant_replace-abc123456789-0"],
        "complete_campaign": true,
        "baseline": {"returncode": 0, "timed_out": false},
        "mutants": [{"id": "fixture-kill", "outcome": "KILLED"}],
        "outcome_counts": {"KILLED": 1, "NO_COVERAGE": 0, "UNVIABLE": 0, "ERROR": 0, "SURVIVED": 0, "TIMED_OUT": 0},
        "mutation_score": 1.0
    });
    let object = base.as_object_mut().expect("object");
    for (key, value) in overrides.as_object().expect("overrides object") {
        if value.is_null() {
            object.remove(key);
        } else {
            object.insert(key.clone(), value.clone());
        }
    }
    object.clone()
}

fn rejections(overrides: Value) -> Vec<Rejection> {
    // No runner components on disk means the runner check has nothing to compare against,
    // which isolates these tests to the generated-campaign rule they are about.
    let runner = RunnerState {
        components: None,
        error: None,
        mutation_testing_sha256: None,
    };
    let anchor = AnchorConfig {
        repo: String::new(),
        commit: String::new(),
        tree: String::new(),
        receipt_prefix: String::new(),
        registry_path: default_anchor_registry_path(),
    };
    let root = repo_root();
    ensure_binding_files(&root);
    let context = ValidationContext {
        repo_root: &root,
        package_root: &root,
        anchor_repo: Path::new("/nonexistent"),
        runner: &runner,
        anchor: &anchor,
    };
    generated_receipt_errors(&receipt(overrides), &campaign(), &context)
}

fn errors(overrides: Value) -> Vec<String> {
    rejection_details(rejections(overrides))
}

fn manual_errors(overrides: Value) -> Vec<String> {
    let mut campaign = campaign();
    campaign.generated = false;
    campaign.mutation_engine = "reviewed_unified_diff".to_owned();
    manual_errors_for(overrides, campaign)
}

fn manual_errors_for(overrides: Value, campaign: CampaignContract) -> Vec<String> {
    let mut payload = receipt(json!({
        "complete_campaign": true,
        "selected_mutations": [],
        "mutants": [],
        "mutation_score": 1.0
    }));
    for (key, value) in overrides.as_object().expect("overrides object") {
        if value.is_null() {
            payload.remove(key);
        } else {
            payload.insert(key.clone(), value.clone());
        }
    }
    let runner = RunnerState {
        components: None,
        error: None,
        mutation_testing_sha256: None,
    };
    let anchor = AnchorConfig {
        repo: String::new(),
        commit: String::new(),
        tree: String::new(),
        receipt_prefix: String::new(),
        registry_path: default_anchor_registry_path(),
    };
    let root = repo_root();
    let context = ValidationContext {
        repo_root: &root,
        package_root: &root,
        anchor_repo: Path::new("/nonexistent"),
        runner: &runner,
        anchor: &anchor,
    };
    let receipt = Receipt {
        path: PathBuf::new(),
        relative: String::new(),
        name: String::new(),
        value: Value::Object(payload),
        bytes: Vec::new(),
        parsed_bytes_available: false,
    };
    rejection_details(receipt_errors(&receipt, &campaign, &context))
}

#[test]
fn generated_completeness_requires_consistent_baseline_and_mutant_accounting() {
    let valid = errors(json!({"survivors": []}));
    assert!(valid.is_empty(), "{valid:?}");
    for (field, value, expected) in [
        (
            "baseline",
            json!({"returncode": 1, "timed_out": false}),
            "baseline must pass",
        ),
        (
            "baseline",
            json!({"returncode": 0, "timed_out": true}),
            "baseline must pass",
        ),
        ("baseline", json!({}), "baseline must pass"),
        ("mutants", json!([]), "at least one killed"),
        ("mutants", json!({}), "non-empty list"),
        (
            "mutants",
            json!([{"id":"a", "outcome":"SURVIVED"}]),
            "non-killed mutant outcome",
        ),
        (
            "mutants",
            json!([{"id":"", "outcome":"KILLED"}]),
            "unique non-empty IDs",
        ),
        (
            "mutants",
            json!([{"outcome":"KILLED"}]),
            "unique non-empty IDs",
        ),
        (
            "mutants",
            json!([{"id":"a", "outcome":"KILLED"},{"id":"a", "outcome":"KILLED"}]),
            "unique non-empty IDs",
        ),
        (
            "mutants",
            json!([{"id":"a", "outcome":"NO_COVERAGE"}]),
            "at least one killed",
        ),
        ("outcome_counts", json!({}), "outcome_counts do not match"),
        ("mutation_score", json!(0.5), "mutation score is not 1.0"),
    ] {
        let found = errors(json!({"survivors": [], field: value}));
        assert!(
            found.iter().any(|error| error.contains(expected)),
            "{field}: {found:?}"
        );
    }
    let mut counts =
        json!({"KILLED":1,"NO_COVERAGE":0,"UNVIABLE":0,"ERROR":0,"SURVIVED":0,"TIMED_OUT":0});
    counts["KILLED"] = json!(2);
    assert!(errors(json!({"survivors": [], "outcome_counts":counts}))
        .iter()
        .any(|error| error.contains("outcome_counts do not match")));
    assert!(errors(json!({"survivors": [], "outcome_counts":counts, "mutants":[{"id":"a","outcome":"KILLED"},{"id":"b","outcome":"KILLED"}]})).is_empty());
}

#[test]
fn a_timed_out_mutant_cannot_supply_complete_pass_evidence() {
    // A timeout is an unknown observation, never evidence that the mutant died.
    assert!(!errors(json!({
        "survivors": [],
        "mutants": [{"id":"a", "outcome":"KILLED"}, {"id":"b", "outcome":"TIMED_OUT"}],
        "outcome_counts": {"KILLED":1,"NO_COVERAGE":0,"UNVIABLE":0,"ERROR":0,"SURVIVED":0,"TIMED_OUT":1}
    }))
    .is_empty());
    // The count still has to agree with the rows it summarizes.
    assert!(errors(json!({
        "survivors": [],
        "mutants": [{"id":"a", "outcome":"KILLED"}, {"id":"b", "outcome":"TIMED_OUT"}],
        "outcome_counts": {"KILLED":1,"NO_COVERAGE":0,"UNVIABLE":0,"ERROR":0,"SURVIVED":0,"TIMED_OUT":0}
    }))
    .iter()
    .any(|error| error.contains("outcome_counts do not match")));
}

#[test]
fn a_missing_process_output_is_not_success() {
    assert!(!super::successful(&None));
    let failed = std::process::Command::new("git")
        .arg("--invalid-receipt-fixture-option")
        .output()
        .expect("git executable");
    assert!(!super::successful(&Some(failed)));
    let passed = std::process::Command::new("git")
        .arg("--version")
        .output()
        .expect("git executable");
    assert!(super::successful(&Some(passed)));
}

#[test]
fn legacy_mutant_rows_bind_the_kill_and_patch_digest() {
    for (outcome, digest, expected) in [
        ("KILLED", "a", None),
        ("SURVIVED", "a", Some("mutant fixture was not killed")),
        ("KILLED", "b", Some("mutant fixture patch hash mismatch")),
    ] {
        let mut contract = campaign();
        contract.generated = false;
        contract.mutations = serde_json::from_value(json!([{
            "id":"fixture", "patch_file":"parser-only.patch", "patch_sha256":"a",
            "allowed_paths":[], "expected_killers":[]
        }]))
        .expect("parser-only mutation contract");
        let found = manual_errors_for(
            json!({
                "selected_mutations":["fixture"],
                "mutants":[{"id":"fixture","outcome":outcome,"patch_sha256":digest}]
            }),
            contract,
        );
        if let Some(expected) = expected {
            assert!(found.iter().any(|error| error == expected), "{found:?}");
        } else {
            assert!(found.is_empty(), "{found:?}");
        }
    }
}

/// The same anchor with one binding varied -- the shape every negative case here
/// needs, now that the bindings travel together.
fn rebound(anchor: &AnchorConfig, commit: &str, tree: &str) -> AnchorConfig {
    AnchorConfig {
        commit: commit.to_owned(),
        tree: tree.to_owned(),
        ..anchor.clone()
    }
}

fn legacy_anchor_fixture() -> (PathBuf, Receipt, CampaignContract, AnchorConfig) {
    let root = std::env::temp_dir().join(format!(
        "conductor-native-anchor-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    std::fs::create_dir_all(root.join("conductor/mutation_campaigns")).expect("campaign directory");
    std::fs::create_dir_all(root.join("research/reports/mutation_testing"))
        .expect("receipt directory");
    let manifest_path = "conductor/mutation_campaigns/legacy.json";
    let manifest = b"{\"fixture\":true}\n";
    std::fs::write(root.join(manifest_path), manifest).expect("manifest");
    std::fs::write(
        root.join("conductor/mutation_campaigns/registry.json"),
        format!("{{\"campaigns\":[{{\"manifest\":\"{manifest_path}\"}}]}}"),
    )
    .expect("registry");
    let relative = "research/reports/mutation_testing/legacy.json";
    let bytes = b"{\"schema_version\":\"llm.mutation-testing.receipt.v2\"}\n".to_vec();
    std::fs::write(root.join(relative), &bytes).expect("receipt");
    for args in [
        vec!["init", "-q"],
        vec!["config", "user.email", "fixture@example.invalid"],
        vec!["config", "user.name", "fixture"],
        vec!["add", "."],
        vec!["commit", "-q", "-m", "fixture"],
    ] {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(&root)
            .status()
            .expect("git available");
        assert!(status.success(), "git fixture setup");
    }
    let rev = |argument: &str| {
        String::from_utf8(
            std::process::Command::new("git")
                .args(["rev-parse", argument])
                .current_dir(&root)
                .output()
                .expect("git revision")
                .stdout,
        )
        .expect("utf8")
        .trim()
        .to_owned()
    };
    let mut campaign = campaign();
    campaign.generated = false;
    campaign.manifest = manifest_path.to_owned();
    campaign.manifest_sha256 = sha256_file(&root.join(manifest_path)).expect("manifest hash");
    let receipt = Receipt {
        path: root.join(relative),
        relative: relative.to_owned(),
        name: "legacy.json".to_owned(),
        value: json!({}),
        bytes,
        parsed_bytes_available: true,
    };
    let anchor = AnchorConfig {
        repo: root.to_string_lossy().into_owned(),
        commit: rev("HEAD"),
        tree: rev("HEAD^{tree}"),
        receipt_prefix: "research/reports/mutation_testing/".to_owned(),
        registry_path: default_anchor_registry_path(),
    };
    (root, receipt, campaign, anchor)
}

#[test]
fn a_pass_receipt_cannot_label_baseline_survivors_as_complete() {
    let found = errors(json!({}));
    assert!(
        found
            .iter()
            .any(|error| error == "PASS receipt reports surviving mutants"),
        "{found:?}"
    );
}

#[test]
fn a_clean_run_is_accepted_even_though_the_score_is_not_one() {
    // This is the whole point of the second rule: the patch rule demanded score == 1.0,
    // which is why 482 of 483 scored campaigns published exactly 1.0.
    assert_eq!(errors(json!({"survivors": []})), Vec::<String>::new());
}

#[test]
fn a_survivor_outside_the_baseline_is_reported_by_id() {
    let found = errors(json!({
        "survivors": ["constant_replace-abc123456789-0", "operator_swap-0123456789ab-2"]
    }));
    assert!(
        found
            .iter()
            .any(|error| error.contains("operator_swap-0123456789ab-2")),
        "{found:?}"
    );
    assert!(
        found.iter().any(|error| error.starts_with("1 survivor(s)")),
        "{found:?}"
    );
}

#[test]
fn a_pass_receipt_rejects_non_string_survivor_rows() {
    let found = errors(json!({"survivors": [17]}));
    assert!(
        found
            .iter()
            .any(|error| error == "survivors must contain only mutation IDs"),
        "{found:?}"
    );
    assert!(
        found
            .iter()
            .any(|error| error == "PASS receipt reports surviving mutants"),
        "{found:?}"
    );
}

#[test]
fn a_receipt_rejects_nonmatching_core_adapter_and_scope_bindings() {
    for key in ["core_sha256", "adapter_sha256", "scope_guard_sha256"] {
        let found = errors(json!({key: "f".repeat(64)}));
        assert!(
            found.iter().any(|error| error.starts_with(key)),
            "{key}: {found:?}"
        );
    }
}

#[test]
fn a_generated_receipt_rejects_each_identity_binding_independently() {
    let cases = [
        ("schema_version", json!("old"), "receipt schema"),
        ("mutants_are_generated", json!(false), "engine-generated"),
        ("status", json!("ERROR"), "status="),
        ("status", json!("RATCHET_HELD"), "status="),
        ("campaign_id", json!("other"), "campaign_id mismatch"),
        ("manifest", json!("other.json"), "manifest path mismatch"),
        (
            "manifest_sha256",
            json!("f".repeat(64)),
            "manifest hash mismatch",
        ),
        ("source_sha256", json!({}), "source hash map mismatch"),
        ("test_sha256", json!({}), "test hash map mismatch"),
    ];
    for (key, value, expected) in cases {
        let found = errors(json!({key: value}));
        assert!(
            found.iter().any(|error| error.contains(expected)),
            "{key}: {found:?}"
        );
    }
}

#[test]
fn a_legacy_receipt_rejects_each_complete_campaign_binding() {
    let cases = [
        (
            "schema_version",
            json!("old"),
            "receipt schema is not current",
        ),
        ("status", json!("ERROR"), "status='ERROR'"),
        ("campaign_id", json!("other"), "campaign_id mismatch"),
        ("manifest", json!("other.json"), "manifest path mismatch"),
        (
            "manifest_sha256",
            json!("f".repeat(64)),
            "manifest hash mismatch",
        ),
        ("source_sha256", json!({}), "source hash map mismatch"),
        (
            "source_symbols",
            json!({"other.py": {}}),
            "source symbol map mismatch",
        ),
        (
            "test_scopes",
            json!({"other.py": {}}),
            "test scope map mismatch",
        ),
        (
            "complete_campaign",
            json!(false),
            "partial campaign receipt",
        ),
        (
            "selected_mutations",
            json!(["unexpected"]),
            "selected mutation ids mismatch",
        ),
        (
            "mutants",
            json!([{"id": "unexpected", "outcome": "KILLED"}]),
            "mutant result ids mismatch",
        ),
        ("mutation_score", json!(0.99), "mutation score is not 1.0"),
    ];
    for (key, value, expected) in cases {
        let found = manual_errors(json!({key: value}));
        assert!(
            found.iter().any(|error| error.contains(expected)),
            "{key}: {found:?}"
        );
    }
}

#[test]
fn value_and_runner_evidence_bind_every_required_field() {
    let contract = ValueContract {
        expected_nodeids: vec!["tests::subject".to_owned()],
        expected_repetitions: 2,
    };
    let valid_value = json!({
        "schema_version": "llm.mutation-testing.test-value.v1",
        "status": "PASS",
        "tests": [{"nodeid": "tests::subject"}],
        "baseline_repetitions": 2
    });
    assert!(value_receipt_errors(Some(&valid_value), &contract).is_empty());
    for (value, expected) in [
        (Value::Null, "test_value evidence is missing"),
        (json!({}), "test_value schema is not current"),
        (
            json!({"schema_version": "llm.mutation-testing.test-value.v1", "status": "FAIL"}),
            "test_value status='FAIL'",
        ),
        (
            json!({"schema_version": "llm.mutation-testing.test-value.v1", "status": "PASS", "tests": [], "baseline_repetitions": 2}),
            "test_value nodeids do not match ranked tests",
        ),
        (
            json!({"schema_version": "llm.mutation-testing.test-value.v1", "status": "PASS", "tests": [{"nodeid":"tests::subject"}], "baseline_repetitions": 1}),
            "test_value baseline repetitions mismatch",
        ),
    ] {
        let found = rejection_details(value_receipt_errors(Some(&value), &contract));
        assert!(
            found.iter().any(|error| error.contains(expected)),
            "{found:?}"
        );
    }
}
#[test]
fn runner_evidence_binds_every_required_field() {
    let root = repo_root();
    let anchor = AnchorConfig {
        repo: String::new(),
        commit: String::new(),
        tree: String::new(),
        receipt_prefix: String::new(),
        registry_path: default_anchor_registry_path(),
    };
    let components = json!({"core": "a".repeat(64)});
    let runner = RunnerState {
        components: Some(components.clone()),
        error: None,
        mutation_testing_sha256: Some("b".repeat(64)),
    };
    let context = ValidationContext {
        repo_root: &root,
        package_root: &root,
        anchor_repo: Path::new("/nonexistent"),
        runner: &runner,
        anchor: &anchor,
    };
    assert!(runner_errors(
        json!({"runner_components_sha256": components.clone(), "runner_sha256": "b".repeat(64)})
            .as_object()
            .expect("object"),
        &context
    )
    .is_empty());
    let stale_map = runner_errors(
        json!({"runner_components_sha256": {}, "runner_sha256": "b".repeat(64)})
            .as_object()
            .expect("object"),
        &context,
    );
    assert!(stale_map
        .iter()
        .any(|error| error.detail.contains("component hash map mismatch")));
    assert!(stale_map
        .iter()
        .any(|error| error.kind == RejectionKind::RunnerMapMismatch));
    let stale_hash = runner_errors(
        json!({"runner_components_sha256": components, "runner_sha256": "c".repeat(64)})
            .as_object()
            .expect("object"),
        &context,
    );
    assert!(stale_hash
        .iter()
        .any(|error| error.detail.contains("runner hash mismatch")));
    assert!(stale_hash
        .iter()
        .any(|error| error.kind == RejectionKind::RunnerMapMismatch));
    let mut legacy = campaign();
    legacy.generated = false;
    let current = Receipt {
        path: PathBuf::new(),
        relative: String::new(),
        name: String::new(),
        value: Value::Object(receipt(json!({"runner_components_sha256": {}}))),
        bytes: Vec::new(),
        parsed_bytes_available: false,
    };
    assert!(
        rejection_details(receipt_errors(&current, &legacy, &context))
            .iter()
            .any(|error| error.contains("component hash map mismatch"))
    );
}

#[test]
fn receipt_loading_keeps_valid_objects_and_reports_exact_malformed_inputs() {
    let root =
        std::env::temp_dir().join(format!("conductor-native-receipts-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let directory = root.join("research/receipts");
    std::fs::create_dir_all(&directory).expect("receipt directory");
    std::fs::write(directory.join("valid.json"), b"{\"status\":\"PASS\"}\n")
        .expect("valid receipt");
    std::fs::write(directory.join("array.json"), b"[]\n").expect("array receipt");
    std::fs::write(directory.join("broken.json"), b"{\n").expect("broken receipt");
    let (receipts, malformed) = load_receipts(
        &root,
        &["research/receipts".to_owned(), "absent".to_owned()],
    )
    .expect("readable directory");
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0].name, "valid.json");
    assert_eq!(malformed.len(), 2, "{malformed:?}");
    assert!(malformed
        .iter()
        .any(|row| row.contains("array.json") && row.contains("JSON object")));
    assert!(malformed.iter().any(|row| row.contains("broken.json")));
    assert!(exact_parse_error(&directory.join("array.json"), b"[]").contains("JSON object"));
    std::fs::remove_dir_all(root).expect("fixture cleanup");
}

#[test]
fn complete_campaign_scope_is_required_for_each_changed_test() {
    let mut contract = campaign();
    assert_eq!(
        campaign_scope_error(&contract, "conductor/test_subject.py"),
        Some("campaign lacks explicit test scope".to_owned())
    );
    contract.test_scopes.insert(
        "conductor/test_subject.py".to_owned(),
        json!({"mode": "partial"}),
    );
    assert_eq!(
        scope_error(&contract, "conductor/test_subject.py"),
        Some("test scope is 'partial', not 'complete'".to_owned())
    );
    contract.test_scopes.insert(
        "conductor/test_subject.py".to_owned(),
        json!({"mode": "complete"}),
    );
    assert_eq!(scope_error(&contract, "conductor/test_subject.py"), None);
}

#[test]
fn lineage_requires_a_current_schema_and_exact_component_map_entry() {
    let root = std::env::temp_dir().join(format!("native-lineage-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("conductor")).expect("lineage directory");
    let recorded = json!({"core": "a".repeat(64)});
    for value in [
        json!({}),
        json!({"schema_version": 0, "entries": []}),
        json!({"schema_version": 1}),
        json!({"schema_version": 1, "entries": [{}]}),
        json!({"schema_version": 1, "entries": [{"runner_components_sha256": {}}]}),
    ] {
        std::fs::write(
            root.join("conductor/mutation_runner_lineage.json"),
            serde_json::to_vec(&value).expect("lineage json"),
        )
        .expect("lineage write");
        assert!(!lineage_accepts(Some(&recorded), &root));
    }
    std::fs::write(
        root.join("conductor/mutation_runner_lineage.json"),
        serde_json::to_vec(
            &json!({"schema_version": 1, "entries": [{"runner_components_sha256": recorded}]}),
        )
        .expect("lineage json"),
    )
    .expect("lineage write");
    assert!(lineage_accepts(
        Some(&json!({"core": "a".repeat(64)})),
        &root
    ));
    std::fs::remove_dir_all(root).expect("fixture cleanup");
}

#[test]
fn legacy_receipt_anchor_requires_the_committed_receipt_and_all_anchor_bindings() {
    let (root, receipt, campaign, anchor) = legacy_anchor_fixture();
    assert!(legacy_receipt_anchor_errors(&receipt, &root, &root, &anchor, &campaign,).is_empty());
    let bad_tree = rejection_details(legacy_receipt_anchor_errors(
        &receipt,
        &root,
        &root,
        &rebound(&anchor, &anchor.commit, "0"),
        &campaign,
    ));
    assert!(bad_tree.iter().any(|error| error.contains("tree mismatch")));
    for (anchor_root, commit, tree, expected) in [
        (
            root.join("conductor"),
            anchor.commit.as_str(),
            anchor.tree.as_str(),
            "legacy receipt anchor repository is unavailable",
        ),
        (
            root.clone(),
            anchor.tree.as_str(),
            anchor.tree.as_str(),
            "legacy receipt anchor commit is unavailable",
        ),
    ] {
        assert_eq!(
            rejection_details(legacy_receipt_anchor_errors(
                &receipt,
                &root,
                &anchor_root,
                &rebound(&anchor, commit, tree),
                &campaign
            )),
            vec![expected.to_owned()]
        );
    }
    let changed_bytes = Receipt {
        path: receipt.path.clone(),
        relative: receipt.relative.clone(),
        name: receipt.name.clone(),
        value: receipt.value.clone(),
        bytes: b"different parser bytes".to_vec(),
        parsed_bytes_available: true,
    };
    let outside_directory = Receipt {
        path: root.join(&campaign.manifest),
        relative: campaign.manifest.clone(),
        name: "legacy.json".to_owned(),
        value: receipt.value.clone(),
        bytes: receipt.bytes.clone(),
        parsed_bytes_available: true,
    };
    assert_eq!(
        rejection_details(legacy_receipt_anchor_errors(
            &outside_directory,
            &root,
            &root,
            &anchor,
            &campaign
        )),
        vec!["legacy receipt path is outside the anchored receipt directory".to_owned()]
    );
    assert_eq!(
        rejection_details(legacy_receipt_anchor_errors(
            &changed_bytes,
            &root,
            &root,
            &anchor,
            &campaign
        )),
        vec!["legacy receipt parsed bytes differ from the anchor".to_owned()]
    );
    std::fs::remove_dir_all(root).expect("fixture cleanup");
}
#[test]
fn legacy_anchor_rejects_executable_receipts_and_missing_parser_bytes() {
    let (root, receipt, campaign, anchor) = legacy_anchor_fixture();
    for args in [
        vec!["update-index", "--chmod=+x", receipt.relative.as_str()],
        vec!["commit", "-q", "-m", "executable receipt fixture"],
    ] {
        assert!(std::process::Command::new("git")
            .args(args)
            .current_dir(&root)
            .status()
            .expect("fixture git")
            .success());
    }
    let revision = |name: &str| {
        String::from_utf8(
            std::process::Command::new("git")
                .args(["rev-parse", name])
                .current_dir(&root)
                .output()
                .expect("fixture revision")
                .stdout,
        )
        .expect("revision utf8")
        .trim()
        .to_owned()
    };
    assert_eq!(
        rejection_details(legacy_receipt_anchor_errors(
            &receipt,
            &root,
            &root,
            &rebound(&anchor, &revision("HEAD"), &revision("HEAD^{tree}")),
            &campaign
        )),
        vec!["legacy receipt is absent or unsafe at the anchor".to_owned()]
    );
    let unavailable = Receipt {
        parsed_bytes_available: false,
        ..receipt
    };
    assert!(rejection_details(legacy_receipt_anchor_errors(
        &unavailable,
        &root,
        &root,
        &anchor,
        &campaign,
    ))
    .iter()
    .any(|error| error.contains("path or parsed bytes")));
    std::fs::remove_dir_all(root).expect("fixture cleanup");
}

#[test]
fn a_generated_receipt_rejects_an_unknown_engine_mapping() {
    let mut campaign = campaign();
    campaign.mutation_engine = "unknown".to_owned();
    let runner = RunnerState {
        components: None,
        error: None,
        mutation_testing_sha256: None,
    };
    let anchor = AnchorConfig {
        repo: String::new(),
        commit: String::new(),
        tree: String::new(),
        receipt_prefix: String::new(),
        registry_path: default_anchor_registry_path(),
    };
    let root = repo_root();
    ensure_binding_files(&root);
    let context = ValidationContext {
        repo_root: &root,
        package_root: &root,
        anchor_repo: Path::new("/nonexistent"),
        runner: &runner,
        anchor: &anchor,
    };
    let found = rejection_details(generated_receipt_errors(
        &receipt(json!({})),
        &campaign,
        &context,
    ));
    assert!(
        found
            .iter()
            .any(|error| error.contains("unsupported generated mutation engine")),
        "{found:?}"
    );
}

#[test]
fn generated_adapter_binding_requires_the_exact_engine_adapter() {
    for (engine, adapter) in [
        ("fest", "conductor/mutation_engine_fest.py"),
        ("cargo-mutants", "conductor/mutation_engine_cargo.py"),
        ("mull", "conductor/mutation_engine_mull.py"),
    ] {
        let root = repo_root();
        ensure_binding_files(&root);
        std::fs::write(root.join(adapter), "adapter\n").expect("adapter");
        let mut campaign = campaign();
        campaign.mutation_engine = engine.to_owned();
        let runner = RunnerState {
            components: None,
            error: None,
            mutation_testing_sha256: None,
        };
        let anchor = AnchorConfig {
            repo: String::new(),
            commit: String::new(),
            tree: String::new(),
            receipt_prefix: String::new(),
            registry_path: default_anchor_registry_path(),
        };
        let context = ValidationContext {
            repo_root: &root,
            package_root: &root,
            anchor_repo: Path::new("/nonexistent"),
            runner: &runner,
            anchor: &anchor,
        };
        let good =
            receipt(json!({"survivors": [], "adapter_sha256": sha256_file(&root.join(adapter))}));
        assert!(
            generated_receipt_errors(&good, &campaign, &context).is_empty(),
            "{engine}"
        );
        let bad = receipt(json!({"survivors": [], "adapter_sha256": "f".repeat(64)}));
        assert!(
            rejection_details(generated_receipt_errors(&bad, &campaign, &context))
                .iter()
                .any(|error| error.contains("adapter_sha256 hash mismatch")),
            "{engine}"
        );
        let missing = receipt(json!({"survivors": [], "adapter_sha256": ""}));
        assert!(
            rejection_details(generated_receipt_errors(&missing, &campaign, &context))
                .iter()
                .any(|error| error.contains("adapter_sha256 is missing")),
            "{engine}"
        );
    }
}

#[test]
fn generated_bindings_resolve_against_package_root_not_repo_root() {
    // Under a src layout `repo_root` (the git root) and `package_root` (where a bare
    // `conductor/...` literal resolves from) differ -- `package_root` is `repo_root/src`.
    // The core/scope-guard/adapter bindings must join `package_root`, never `repo_root`,
    // or every receipt in a src-layout repository hashes the wrong (or absent) file.
    let repo_root = repo_root();
    let package_root = repo_root.join("src");
    ensure_binding_files(&package_root);
    let adapter = "conductor/mutation_engine_fest.py";
    std::fs::write(package_root.join(adapter), "adapter\n").expect("adapter");
    let mut campaign = campaign();
    campaign.mutation_engine = "fest".to_owned();
    let runner = RunnerState {
        components: None,
        error: None,
        mutation_testing_sha256: None,
    };
    let anchor = AnchorConfig {
        repo: String::new(),
        commit: String::new(),
        tree: String::new(),
        receipt_prefix: String::new(),
        registry_path: default_anchor_registry_path(),
    };
    let context = ValidationContext {
        repo_root: &repo_root,
        package_root: &package_root,
        anchor_repo: Path::new("/nonexistent"),
        runner: &runner,
        anchor: &anchor,
    };
    let good = receipt(json!({
        "survivors": [],
        "core_sha256": sha256_file(&package_root.join("conductor/mutation_engine_generated.py")),
        "scope_guard_sha256": sha256_file(&package_root.join("conductor/mutation_run_scope.py")),
        "adapter_sha256": sha256_file(&package_root.join(adapter)),
    }));
    let found = rejection_details(generated_receipt_errors(&good, &campaign, &context));
    assert!(
        !found
            .iter()
            .any(|error| error.contains("_sha256 hash mismatch")
                || error.contains("_sha256 is missing")),
        "{found:?}"
    );
    // A receipt bound to the (wrong) repo_root copies must still be rejected: the fix
    // must not have made the check universally lenient, only correctly rooted.
    let stale = receipt(json!({
        "survivors": [],
        "core_sha256": "f".repeat(64),
        "scope_guard_sha256": "f".repeat(64),
        "adapter_sha256": "f".repeat(64),
    }));
    let stale_found = rejection_details(generated_receipt_errors(&stale, &campaign, &context));
    assert!(
        stale_found
            .iter()
            .any(|error| error == "core_sha256 hash mismatch"),
        "{stale_found:?}"
    );
}

#[test]
fn a_receipt_rejects_current_source_or_test_drift() {
    let mut campaign = campaign();
    campaign.source_drifted = true;
    let runner = RunnerState {
        components: None,
        error: None,
        mutation_testing_sha256: None,
    };
    let anchor = AnchorConfig {
        repo: String::new(),
        commit: String::new(),
        tree: String::new(),
        receipt_prefix: String::new(),
        registry_path: default_anchor_registry_path(),
    };
    let root = repo_root();
    let context = ValidationContext {
        repo_root: &root,
        package_root: &root,
        anchor_repo: Path::new("/nonexistent"),
        runner: &runner,
        anchor: &anchor,
    };
    let found = rejection_details(generated_receipt_errors(
        &receipt(json!({})),
        &campaign,
        &context,
    ));
    assert!(
        found
            .iter()
            .any(|error| error == "current source or test hashes drifted"),
        "{found:?}"
    );
}

#[test]
fn rejections_carry_the_kind_of_the_rule_that_produced_them() {
    // A held ratchet is debt, not a defect: status lands as not_pass.
    assert!(
        rejections(json!({"status": "RATCHET_HELD", "survivors": []}))
            .iter()
            .any(
                |error| error.kind == RejectionKind::NotPass && error.detail.starts_with("status=")
            )
    );
    // The class PR #46 lived through: a mutants list the validator cannot
    // accept is a schema_error, never silently debt.
    assert!(rejections(json!({"mutants": json!({}), "survivors": []}))
        .iter()
        .any(|error| error.kind == RejectionKind::SchemaError
            && error.detail == "mutants must be a non-empty list"));
    // Identity bindings are schema errors...
    assert!(rejections(json!({"campaign_id": "other", "survivors": []}))
        .iter()
        .any(|error| error.kind == RejectionKind::SchemaError
            && error.detail == "campaign_id mismatch"));
    // ...while the runner-era bindings share the map's debt class.
    assert!(
        rejections(json!({"core_sha256": "f".repeat(64), "survivors": []}))
            .iter()
            .any(|error| error.kind == RejectionKind::RunnerMapMismatch
                && error.detail == "core_sha256 hash mismatch")
    );
    // A receipt for an earlier revision of the same campaign (the path
    // matched, the manifest has since been refreshed) is a stale PASS:
    // not_pass, the campaign re-runs -- never a defect-class rejection.
    assert!(
        rejections(json!({"manifest_sha256": "f".repeat(64), "survivors": []}))
            .iter()
            .any(|error| error.kind == RejectionKind::NotPass
                && error.detail == "manifest hash mismatch")
    );
    // A survivor outside the baseline contradicts the claimed PASS: the
    // validator refuses the claim rather than filing it as re-run debt.
    assert!(rejections(json!({
        "survivors": ["operator_swap-0123456789ab-2"],
        "mutants": [{"id": "operator_swap-0123456789ab-2", "outcome": "SURVIVED"}],
        "outcome_counts": {"KILLED":0,"NO_COVERAGE":0,"UNVIABLE":0,"ERROR":0,"SURVIVED":1,"TIMED_OUT":0}
    }))
    .iter()
    .any(|error| error.kind == RejectionKind::SchemaError
        && error.detail.starts_with("1 survivor(s) outside the recorded baseline")));
}

#[test]
fn a_receipt_that_does_not_declare_generated_mutants_is_refused() {
    // Otherwise a hand-authored patch receipt could be offered here and skip the ratchet.
    let found = errors(json!({"mutants_are_generated": null}));
    assert!(
        found.iter().any(|e| e.contains("engine-generated")),
        "{found:?}"
    );
}

#[test]
fn a_receipt_whose_tests_have_changed_is_refused() {
    let found = errors(json!({
        "test_sha256": {"conductor/test_subject.py": "f".repeat(64)}
    }));
    assert!(
        found.iter().any(|e| e == "test hash map mismatch"),
        "{found:?}"
    );
}

#[test]
fn a_receipt_without_an_adapter_digest_is_refused() {
    let found = errors(json!({"adapter_sha256": ""}));
    assert!(
        found.iter().any(|e| e == "adapter_sha256 is missing"),
        "{found:?}"
    );
}

#[test]
fn a_failing_status_cannot_be_offered_as_evidence() {
    let found = errors(json!({"status": "FAIL"}));
    assert!(found.iter().any(|e| e.starts_with("status=")), "{found:?}");
}
