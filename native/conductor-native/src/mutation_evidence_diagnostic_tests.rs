use super::*;

fn diagnostic() -> Value {
    json!({"runner":{"components":null,"error":"legacy identity unavailable","mutation_testing_sha256":null},"binding_sha256":{},"lineage":null})
}
fn request(runtime: Value) -> Value {
    json!({"candidate_paths":[],"campaigns":[],"runtime":runtime,"broken_campaigns":[],"malformed_receipts":[]})
}
#[test]
fn diagnostic_shape_is_admitted_and_inconsistent_states_refuse() {
    verify_captured(&request(diagnostic()), &[], |_, _, _| Ok(())).unwrap();
    for (field, value) in [
        ("error", json!(null)),
        ("error", json!("  ")),
        ("mutation_testing_sha256", json!("a".repeat(64))),
    ] {
        let mut runtime = diagnostic();
        runtime["runner"][field] = value;
        assert!(verify_captured(&request(runtime), &[], |_, _, _| Ok(())).is_err());
    }
    let mut runtime = diagnostic();
    runtime["binding_sha256"] = json!({"conductor/foo.py":"bad"});
    assert!(verify_captured(&request(runtime), &[], |_, _, _| Ok(())).is_err());
    let mut runtime = diagnostic();
    runtime["lineage"] = json!({});
    assert!(verify_captured(&request(runtime), &[], |_, _, _| Ok(())).is_err());
}
#[test]
fn unavailable_identity_is_debt_for_real_legacy_generated_receipt() {
    let manifest =
        "campaigns/fix-mutation-attribution-rootdir_mutation_attribution_fest_20260921.json";
    let test = "src/conductor/test_mutation_attribution.py";
    let files = BTreeMap::from([
        (
            manifest.into(),
            include_bytes!("../tests/fixtures/mutation_verifier/manifest.json").to_vec(),
        ),
        (
            "src/conductor/mutation_attribution.py".into(),
            include_bytes!("../tests/fixtures/mutation_verifier/source.py.txt").to_vec(),
        ),
        (
            test.into(),
            include_bytes!("../tests/fixtures/mutation_verifier/test.py.txt").to_vec(),
        ),
    ]);
    let contract = load_campaign_captured(manifest, &files, &[test.into()]).unwrap();
    let receipt = CapturedReceipt {
        path: "campaigns/receipts/original.json".into(),
        text: include_str!("../tests/fixtures/mutation_verifier/original-receipt.json").into(),
    };
    let request = json!({"candidate_paths":[test],"campaigns":[contract],"runtime":diagnostic(),"broken_campaigns":[],"malformed_receipts":[]});
    let verdict = verify_captured(&request, &[receipt], |_, _, _| Ok(())).unwrap();
    assert_eq!(verdict.result["status"], "FAIL");
    assert!(verdict.result["evidence"].as_array().unwrap().is_empty());
    assert!(verdict.attempts[0]
        .errors
        .to_string()
        .contains("legacy identity unavailable"));
}
#[test]
fn malformed_unrelated_receipt_is_counted() {
    let receipt = CapturedReceipt {
        path: "campaigns/receipts/bad.json".into(),
        text: "{".into(),
    };
    let result = verify_captured(&request(diagnostic()), &[receipt], |_, _, _| Ok(())).unwrap();
    assert_eq!(result.result["rejection_counts"]["decode_error"], 1);
}

#[test]
fn partial_slim_native_receipt_remains_debt_without_legacy_identity() {
    use crate::mutation_native_runner::{InstalledIdentity, RUNNER_KIND, RUNNER_SCHEMA};
    let manifest =
        "campaigns/fix-mutation-attribution-rootdir_mutation_attribution_fest_20260921.json";
    let test = "src/conductor/test_mutation_attribution.py";
    let files = BTreeMap::from([
        (
            manifest.into(),
            include_bytes!("../tests/fixtures/mutation_verifier/manifest.json").to_vec(),
        ),
        (
            "src/conductor/mutation_attribution.py".into(),
            include_bytes!("../tests/fixtures/mutation_verifier/source.py.txt").to_vec(),
        ),
        (
            test.into(),
            include_bytes!("../tests/fixtures/mutation_verifier/test.py.txt").to_vec(),
        ),
    ]);
    let contract = load_campaign_captured(manifest, &files, &[test.into()]).unwrap();
    let mut payload: Value = serde_json::from_str(include_str!(
        "../tests/fixtures/mutation_verifier/original-receipt.json"
    ))
    .unwrap();
    payload["status"] = json!("ERROR");
    payload["runner_kind"] = json!(RUNNER_KIND);
    payload["native_runner"] = json!({"schema_version":RUNNER_SCHEMA,"forge_version":"fixture", "executable_sha256":"a".repeat(64),"compiled_source_sha256":"b".repeat(64),"compiled_git_commit":"c".repeat(40),"source_matches_git":true});
    let receipt = CapturedReceipt {
        path: "campaigns/receipts/partial.json".into(),
        text: payload.to_string(),
    };
    let request = json!({"candidate_paths":[test],"campaigns":[contract],"runtime":diagnostic(),"broken_campaigns":[],"malformed_receipts":[]});
    let pin = InstalledIdentity {
        executable_sha256: "a".repeat(64),
        compiled_source_sha256: "b".repeat(64),
        compiled_git_commit: "c".repeat(40),
    };
    let verdict =
        verify_captured_native(&request, &[receipt], |_, _, _| Ok(()), None, Some(&pin)).unwrap();
    assert_eq!(verdict.result["status"], "FAIL");
    assert!(!verdict.attempts[0].valid);
    assert!(!verdict.attempts[0]
        .errors
        .to_string()
        .contains("legacy identity unavailable"));
}
