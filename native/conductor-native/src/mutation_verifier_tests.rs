use super::*;
use crate::mutation_receipt::generated_outcome_errors;
use std::{fs, path::PathBuf};
const RECEIPT: &str = include_str!("../tests/fixtures/mutation_verifier/original-receipt.json");
const MANIFEST: &str =
    "campaigns/fix-mutation-attribution-rootdir_mutation_attribution_fest_20260921.json";
struct Fixture {
    root: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "mutation-verifier-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let f = Self { root };
        f.write(
            MANIFEST,
            include_bytes!("../tests/fixtures/mutation_verifier/manifest.json"),
        );
        f.write(
            "src/conductor/mutation_attribution.py",
            include_bytes!("../tests/fixtures/mutation_verifier/source.py.txt"),
        );
        f.write(
            "src/conductor/test_mutation_attribution.py",
            include_bytes!("../tests/fixtures/mutation_verifier/test.py.txt"),
        );
        f
    }
    fn write(&self, path: &str, bytes: &[u8]) {
        let path = self.root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }
    fn request(&self) -> Value {
        let receipt: Value = serde_json::from_str(RECEIPT).unwrap();
        let campaign =
            load_campaign(&json!({"repo_root":self.root,"manifest_path":MANIFEST})).unwrap();
        assert_eq!(campaign["source_drifted"], false);
        assert_eq!(campaign["manifest_sha256"], receipt["manifest_sha256"]);
        json!({"campaign":campaign,"receipt":receipt,"runner":{
            "components":receipt["runner_components_sha256"],"error":null,
            "mutation_testing_sha256":receipt["runner_sha256"]},"binding_sha256":{
            "conductor/mutation_engine_generated.py":receipt["core_sha256"],
            "conductor/mutation_engine_fest.py":receipt["adapter_sha256"],
            "conductor/mutation_run_scope.py":receipt["scope_guard_sha256"]},"lineage":null})
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
fn has(value: &Value, needle: &str) -> bool {
    value["errors"]
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["detail"].as_str().unwrap().contains(needle))
}

#[test]
fn actual_automatic_corpus_decodes_but_unknown_outcomes_are_not_current_pass_evidence() {
    let fixture = Fixture::new();
    let request = fixture.request();
    let expanded = crate::receipt_slim::expand_receipt(&request["receipt"]).unwrap();
    assert_eq!(expanded["mutants"].as_array().unwrap().len(), 164);
    let result = validate_generated_captured(&request).unwrap();
    assert_eq!(result["valid"], false);
    assert!(has(&result, "non-killed mutant"));
    assert!(!has(&result, "hash mismatch"));
    assert_eq!(result["execution_verified"], false);
}

#[test]
fn pure_complete_outcome_predicate_accepts_only_all_killed_consistent_measurements() {
    // Outcome records only: not a fabricated campaign, receipt or PASS claim.
    let observations = json!({"complete_campaign":true,"baseline":{"returncode":0,"timed_out":false},
        "mutants":[{"id":"measured-row","outcome":"KILLED"}],"mutation_score":1.0,
        "outcome_counts":{"KILLED":1,"NO_COVERAGE":0,"UNVIABLE":0,"ERROR":0,"SURVIVED":0,"TIMED_OUT":0}});
    assert!(generated_outcome_errors(observations.as_object().unwrap()).is_empty());
    for outcome in ["TIMED_OUT", "NO_COVERAGE", "UNVIABLE", "ERROR", "SURVIVED"] {
        let mut changed = observations.clone();
        changed["mutants"][0]["outcome"] = json!(outcome);
        changed["outcome_counts"]["KILLED"] = json!(0);
        changed["outcome_counts"][outcome] = json!(1);
        assert!(
            !generated_outcome_errors(changed.as_object().unwrap()).is_empty(),
            "{outcome}"
        );
    }
    for field in ["baseline", "outcome_counts", "complete_campaign"] {
        let mut missing = observations.clone();
        missing.as_object_mut().unwrap().remove(field);
        assert!(
            !generated_outcome_errors(missing.as_object().unwrap()).is_empty(),
            "{field}"
        );
    }
    let mut partial = observations;
    partial["engine_summary"] = json!({"mutants_generated":2,"errors":0});
    assert!(!generated_outcome_errors(partial.as_object().unwrap()).is_empty());
}

#[test]
fn corpus_hash_corruption_missing_bindings_and_baseline_timeout_are_rejected() {
    let f = Fixture::new();
    let request = f.request();
    for (field, needle) in [
        ("manifest_sha256", "manifest hash mismatch"),
        ("core_sha256", "core_sha256 hash mismatch"),
    ] {
        let mut changed = request.clone();
        changed["receipt"][field] = json!("0".repeat(64));
        assert!(has(&validate_generated_captured(&changed).unwrap(), needle));
    }
    let mut missing = request.clone();
    missing["binding_sha256"].as_object_mut().unwrap().clear();
    assert!(has(
        &validate_generated_captured(&missing).unwrap(),
        "hash mismatch"
    ));
    let mut timed_out = request.clone();
    timed_out["receipt"]["baseline"]["timed_out"] = json!(true);
    assert!(has(
        &validate_generated_captured(&timed_out).unwrap(),
        "baseline must pass without timeout"
    ));
    let mut no_runner = request;
    no_runner["runner"]["components"] = Value::Null;
    assert!(validate_generated_captured(&no_runner)
        .unwrap_err()
        .contains("runner component map missing"));
}

#[test]
fn runner_lineage_is_explicit_captured_data_and_cannot_hide_hash_mismatch() {
    let f = Fixture::new();
    let mut request = f.request();
    let old = request["runner"]["components"].clone();
    request["runner"]["components"] = json!({"conductor/mutation_testing.py":"f".repeat(64)});
    assert!(has(
        &validate_generated_captured(&request).unwrap(),
        "component hash map mismatch"
    ));
    request["lineage"] = json!({"schema_version":1,"entries":[{"runner_components_sha256":old}]});
    assert!(!has(
        &validate_generated_captured(&request).unwrap(),
        "component hash map mismatch"
    ));
    request["lineage"] = json!({"schema_version":2,"entries":[]});
    assert!(has(
        &validate_generated_captured(&request).unwrap(),
        "component hash map mismatch"
    ));
}

#[test]
fn detail_decode_failure_missing_rows_and_limits_are_loud() {
    let f = Fixture::new();
    let mut request = f.request();
    request["receipt"]["detail"]["blob"] = json!("%%% invalid base64 %%%");
    assert!(validate_generated_captured(&request)
        .unwrap_err()
        .contains("base64"));
    let mut request = f.request();
    let mut expanded = crate::receipt_slim::expand_receipt(&request["receipt"]).unwrap();
    expanded.as_object_mut().unwrap().remove("mutants");
    request["receipt"] = expanded;
    assert!(has(
        &validate_generated_captured(&request).unwrap(),
        "mutants must be a non-empty list"
    ));
    assert!(parse_request(&" ".repeat(32 * 1024 * 1024 + 1)).is_err());
    let mut deep = Value::Null;
    for _ in 0..66 {
        deep = json!([deep]);
    }
    assert!(validate_generated_captured(&deep).is_err());
}

#[test]
fn native_receipt_loader_uses_existing_corpus_without_interpreter_or_hidden_missing_data() {
    let f = Fixture::new();
    f.write("receipts/actual.json", RECEIPT.as_bytes());
    f.write("receipts/broken.json", b"{broken");
    let (receipts, malformed) =
        load_receipts(&f.root, &["receipts".into(), "absent".into()]).unwrap();
    assert_eq!(receipts.len(), 1);
    assert_eq!(
        receipts[0].value["campaign_id"],
        f.request()["receipt"]["campaign_id"]
    );
    assert_eq!(malformed.len(), 1);
    assert!(malformed[0].contains("broken.json"));
}

#[test]
fn compressed_detail_expansion_has_a_hard_decoded_byte_limit() {
    use base64::Engine;
    let f = Fixture::new();
    let mut request = f.request();
    let raw = vec![b' '; 32 * 1024 * 1024 + 1];
    let compressed = zstd::stream::encode_all(raw.as_slice(), 1).unwrap();
    request["receipt"]["detail"] = json!({"encoding":"zstd+base64", "blob":base64::engine::general_purpose::STANDARD.encode(compressed)});
    assert!(validate_generated_captured(&request)
        .unwrap_err()
        .contains("32 MiB"));
}

#[test]
fn held_ratchet_and_explicit_partial_status_never_become_pass_evidence() {
    let f = Fixture::new();
    let mut request = f.request();
    request["receipt"]["status"] = json!("RATCHET_HELD");
    let result = validate_generated_captured(&request).unwrap();
    assert_eq!(result["valid"], false);
    assert_eq!(result["errors"][0]["kind"], "not_pass");
    let mut request = f.request();
    request["receipt"]["complete_campaign"] = json!(false);
    assert!(has(
        &validate_generated_captured(&request).unwrap(),
        "partial or unproven"
    ));
}

#[test]
fn current_automatic_corpus_supersession_remains_an_explicit_refusal() {
    let text=include_str!("../../../campaigns/receipts/fix-mutation-attribution-rootdir_mutation_attribution_fest_20260921_rootdirfix.json");
    let actual: Value = serde_json::from_str(text).unwrap();
    let error = crate::receipt_slim::expand_receipt(&actual).unwrap_err();
    assert!(error.contains("superseded"), "{error}");
}
