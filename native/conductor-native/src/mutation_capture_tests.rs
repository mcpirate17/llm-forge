//! Existing automatic corpus is contract/decoding evidence, never fabricated PASS.
use super::*;
const MANIFEST: &str =
    "campaigns/fix-mutation-attribution-rootdir_mutation_attribution_fest_20260921.json";
const TEST: &str = "src/conductor/test_mutation_attribution.py";
const RECEIPT: &str = include_str!("../tests/fixtures/mutation_verifier/original-receipt.json");
fn files() -> BTreeMap<String, Vec<u8>> {
    BTreeMap::from([
        (
            MANIFEST.into(),
            include_bytes!("../tests/fixtures/mutation_verifier/manifest.json").to_vec(),
        ),
        (
            "src/conductor/mutation_attribution.py".into(),
            include_bytes!("../tests/fixtures/mutation_verifier/source.py.txt").to_vec(),
        ),
        (
            TEST.into(),
            include_bytes!("../tests/fixtures/mutation_verifier/test.py.txt").to_vec(),
        ),
    ])
}
#[test]
fn captured_automatic_contract_uses_actual_bytes_and_detects_drift() {
    let mut files = files();
    let contract = load_campaign_captured(MANIFEST, &files, &[TEST.into()]).unwrap();
    assert_eq!(contract["source_drifted"], false);
    let raw: Value = serde_json::from_str(RECEIPT).unwrap();
    assert_eq!(contract["manifest_sha256"], raw["manifest_sha256"]);
    files
        .get_mut(TEST)
        .unwrap()
        .extend_from_slice(b"\n# changed test bytes\n");
    assert_eq!(
        load_campaign_captured(MANIFEST, &files, &[TEST.into()]).unwrap()["source_drifted"],
        true
    );
    assert!(load_campaign_captured(MANIFEST, &BTreeMap::new(), &[TEST.into()]).is_err());
}
#[test]
fn held_receipt_selection_rejects_real_partial_corpus_without_runtime_claim() {
    let contract = load_campaign_captured(MANIFEST, &files(), &[TEST.into()]).unwrap();
    let request = json!({"candidate_paths":[TEST],"campaigns":[contract],"runtime":null,"broken_campaigns":[],"malformed_receipts":[]});
    let raw = [CapturedReceipt {
        path: "campaigns/receipts/original.json".into(),
        text: RECEIPT.into(),
    }];
    let selected = verify_captured(&request, &raw, |_, _, _| {
        panic!("automatic receipts must not invoke historical anchor")
    })
    .unwrap();
    assert!(selected.result["evidence"].as_array().unwrap().is_empty());
    assert_eq!(
        selected.result["missing_evidence"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(selected.attempts.len(), 1);
    assert!(!selected.attempts[0].valid);
    assert_eq!(
        selected.attempts[0].expanded["mutants"]
            .as_array()
            .unwrap()
            .len(),
        164
    );
}
#[test]
fn held_receipt_selection_validates_actual_outcomes_after_captured_runtime_binding() {
    let contract = load_campaign_captured(MANIFEST, &files(), &[TEST.into()]).unwrap();
    let raw: Value = serde_json::from_str(RECEIPT).unwrap();
    // Recorded hashes exercise pure matching only; this is not an operator bundle or accepted execution.
    let runtime = json!({"runner":{"components":raw["runner_components_sha256"],"error":null,"mutation_testing_sha256":raw["runner_sha256"]},
        "binding_sha256":{"conductor/mutation_engine_generated.py":raw["core_sha256"],"conductor/mutation_run_scope.py":raw["scope_guard_sha256"],
        "conductor/mutation_engine_fest.py":raw["adapter_sha256"]},"lineage":null});
    let request = json!({"candidate_paths":[TEST],"campaigns":[contract],"runtime":runtime,"broken_campaigns":[],"malformed_receipts":[]});
    let selected = verify_captured(
        &request,
        &[CapturedReceipt {
            path: "campaigns/receipts/original.json".into(),
            text: RECEIPT.into(),
        }],
        |_, _, _| Ok(()),
    )
    .unwrap();
    assert!(!selected.attempts[0].valid);
    assert!(selected.attempts[0]
        .errors
        .to_string()
        .contains("non-killed mutant"));
    assert!(!selected.attempts[0]
        .errors
        .to_string()
        .contains("hash mismatch"));
}
#[test]
fn captured_inventory_and_registry_fail_loudly() {
    let base = json!({"schema_version":1,"enforcement":"changed_tests","test_patterns":canonical_test_patterns(),
        "receipt_directories":["campaigns/receipts"],"campaigns":[]});
    let fragments = BTreeMap::from([(
        "campaigns/registry.d/a.json".into(),
        json!({"manifest":MANIFEST}).to_string(),
    )]);
    assert_eq!(
        load_registry_captured(&base.to_string(), &fragments).unwrap()["manifests"],
        json!([MANIFEST])
    );
    let bad = BTreeMap::from([("campaigns/registry.d/a.json".into(), "{}".into())]);
    assert!(load_registry_captured(&base.to_string(), &bad).is_err());
    let mut files = files();
    files.insert("../escape".into(), vec![]);
    assert!(load_campaign_captured(MANIFEST, &files, &[]).is_err());
    let contract = load_campaign_captured(MANIFEST, &self::files(), &[TEST.into()]).unwrap();
    let request = json!({"candidate_paths":[TEST,TEST],"campaigns":[contract],"runtime":null,"broken_campaigns":[],"malformed_receipts":[]});
    assert!(verify_captured(&request, &[], |_, _, _| Ok(())).is_err());
}

#[test]
fn captured_native_identity_uses_explicit_pin_without_python_runtime() {
    use crate::mutation_native_runner::{InstalledIdentity, RUNNER_KIND, RUNNER_SCHEMA};
    let contract = load_campaign_captured(MANIFEST, &files(), &[TEST.into()]).unwrap();
    let request = json!({"candidate_paths":[TEST],"campaigns":[contract],"runtime":null,"broken_campaigns":[],"malformed_receipts":[]});
    let original: Value = serde_json::from_str(RECEIPT).unwrap();
    let mut payload = crate::receipt_slim::expand_receipt(&original).unwrap();
    let source = payload["source_sha256"]
        .as_object()
        .unwrap()
        .keys()
        .next()
        .unwrap()
        .clone();
    payload["runner_kind"] = json!(RUNNER_KIND);
    payload["native_runner"] = json!({"schema_version":RUNNER_SCHEMA,"forge_version":"fixture",
        "executable_sha256":"a".repeat(64),"compiled_source_sha256":"b".repeat(64),
        "compiled_git_commit":"c".repeat(40),"source_matches_git":true});
    payload["mutants"] = json!([{"path":source,"outcome":"KILLED"}]);
    payload["source_coverage"] = crate::mutation_source_coverage::report(
        payload["source_sha256"].as_object().unwrap(),
        payload["mutants"].as_array().unwrap(),
    )
    .unwrap();
    let raw = [CapturedReceipt {
        path: "campaigns/receipts/native-test.json".into(),
        text: payload.to_string(),
    }];
    let pin = InstalledIdentity {
        executable_sha256: "a".repeat(64),
        compiled_source_sha256: "b".repeat(64),
        compiled_git_commit: "c".repeat(40),
    };
    let selected =
        verify_captured_native(&request, &raw, |_, _, _| Ok(()), None, Some(&pin)).unwrap();
    assert!(!selected.attempts[0]
        .errors
        .to_string()
        .contains("native runner source fingerprint"));
    let stale = verify_captured_native(
        &request,
        &raw,
        |_, _, _| Ok(()),
        Some(&"d".repeat(64)),
        Some(&pin),
    )
    .unwrap();
    assert!(stale.attempts[0]
        .errors
        .to_string()
        .contains("native runner source fingerprint"));
    let no_pin = verify_captured(&request, &raw, |_, _, _| Ok(())).unwrap();
    assert!(!no_pin.attempts[0].valid);
}
