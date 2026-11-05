use super::*;

fn report(passed: bool) -> Value {
    json!({"status":"COMPLETE","tests":{"tests/contract.rs::check":{"outcome":if passed {"PASSED"} else {"FAILED"},"cases":1}},
        "missing_nodeids":[],"ambiguous_nodeids":[],"unmapped_cases":[],"unranked_failures":[]})
}
fn fixture() -> (Value, Value, Value, Value) {
    let metadata = json!({"declaration":{"schema_version":1,
        "target":{"manifest_path":"Cargo.toml","package":"demo","test":"contract"},"baseline_repetitions":2,
        "required_contracts":[{"id":"value","criticality":"high","active_paths":["src/lib.rs"]}],
        "ranked_tests":[{"rank":1,"nodeid":"tests/contract.rs::check","contract":"value","rationale":"value remains true"}]},
        "inventory":{"digest":"held-inventory"}});
    let observation = |path: &str, passed| json!({"log_path":path,"log_sha256":"a".repeat(64),"phases":[{},{}],"report":report(passed)});
    let mut mutant = observation("log/mutant.log", false);
    mutant["id"] = json!("generated-id");
    let observations = json!({"schema_version":SCHEMA,"inventory_digest":"held-inventory",
        "baseline_reports":[report(true),report(true)],"engine_baseline":observation("log/baseline.log",true),"mutants":[mutant]});
    (
        metadata,
        json!({"src/lib.rs":"source-sha"}),
        json!([{"id":"generated-id","path":"src/lib.rs","outcome":"KILLED"}]),
        observations,
    )
}

fn generated_fixture() -> (crate::mutation_manifest::CampaignContract, Value) {
    let (metadata, _, mutants, observations) = fixture();
    let sources = json!({"src/lib.rs":"b".repeat(64)});
    let tests = json!({"tests/contract.rs":"c".repeat(64)});
    let manifest = json!({"schema_version":1,"campaign_id":"cargo-value","title":"fixture","language":"rust",
        "mutation_engine":"cargo-mutants","source_sha256":sources,"test_sha256":tests,
        "test_argv":["cargo","test"],"generator":{"run_timeout_seconds":60},"survivor_baseline":[]});
    let hashes = BTreeMap::from([
        ("src/lib.rs".into(), "b".repeat(64)),
        ("tests/contract.rs".into(), "c".repeat(64)),
    ]);
    let loaded = crate::mutation_evidence::load_generated_campaign_captured(
        "campaign.json",
        &manifest.to_string(),
        &hashes,
    )
    .unwrap();
    let mut campaign: crate::mutation_manifest::CampaignContract =
        serde_json::from_value(loaded).unwrap();
    campaign.generated_value = Some(metadata.clone());
    campaign.value_analysis = Some(crate::mutation_manifest::ValueContract {
        expected_nodeids: vec!["tests/contract.rs::check".into()],
        expected_repetitions: 2,
    });
    let (value, attribution) = evaluate(&metadata, &sources, &mutants, &observations).unwrap();
    let payload = json!({"schema_version":"llm.mutation-testing.receipt.v3","status":"PASS","mutants_are_generated":true,
        "campaign_id":campaign.campaign_id,"manifest":campaign.manifest,"manifest_sha256":campaign.manifest_sha256,
        "source_sha256":sources,"test_sha256":tests,"survivors":[],"complete_campaign":true,
        "baseline":{"returncode":0,"timed_out":false},"mutants":mutants,"mutation_score":1.0,
        "outcome_counts":{"KILLED":1,"SURVIVED":0,"TIMED_OUT":0,"ERROR":0,"UNVIABLE":0,"NO_COVERAGE":0},
        "source_coverage":{"schema_version":1,"complete":true,"scored_by_source":{"src/lib.rs":1},"missing":[]},
        "runner_kind":crate::mutation_native_runner::RUNNER_KIND,
        "native_runner":{"schema_version":crate::mutation_native_runner::RUNNER_SCHEMA,"forge_version":"fixture",
        "executable_sha256":"d".repeat(64),"compiled_source_sha256":"e".repeat(64),"compiled_git_commit":"f".repeat(40),"source_matches_git":true},
        "test_value":value,"attribution":attribution,"cargo_value_observations":observations,
        "cargo_value_process_cleanup":{"complete":true,"commands_observed":3},"cargo_value_commands":[{"state":"observed","confinement":{"cleanup_complete":true,"outcome":"exited","exit_code":0,"signal":null,"resources":{"verified":true},"errors":[]}},{"state":"observed","confinement":{"cleanup_complete":true,"outcome":"exited","exit_code":0,"signal":null,"resources":{"verified":true},"errors":[]}},{"state":"observed","confinement":{"cleanup_complete":true,"outcome":"exited","exit_code":0,"signal":null,"resources":{"verified":true},"errors":[]}}],"snapshot":{"removed":true}});
    (campaign, payload)
}

#[test]
fn generated_receipt_rule_rejects_tampered_value_attribution_or_matrix() {
    let (campaign, payload) = generated_fixture();
    let runner = crate::mutation_receipt::RunnerState {
        components: None,
        error: None,
        mutation_testing_sha256: None,
    };
    let check = |value: &Value| {
        crate::mutation_receipt::captured_generated_errors(
            value.as_object().unwrap(),
            &campaign,
            &runner,
            |_| Some("e".repeat(64)),
            |_| false,
        )
    };
    assert!(check(&payload).is_empty(), "{:?}", check(&payload));
    let mut changed = payload.clone();
    changed["test_value"]["tests"][0]["classification"] = json!("INERT");
    assert!(!check(&changed).is_empty());
    let mut changed = payload.clone();
    changed["attribution"]["attributed_mutants"] = json!(0);
    assert!(!check(&changed).is_empty());
    let mut changed = payload.clone();
    changed["cargo_value_observations"]["mutants"][0]["report"] = report(true);
    assert!(!check(&changed).is_empty());
    for (field, replacement) in [
        (
            "cargo_value_process_cleanup",
            json!({"complete":false,"commands_observed":3}),
        ),
        (
            "cargo_value_process_cleanup",
            json!({"complete":true,"commands_observed":2}),
        ),
        ("snapshot", json!({"removed":false})),
    ] {
        let mut changed = payload.clone();
        changed[field] = replacement;
        assert!(!check(&changed).is_empty());
    }
    let mut changed = payload;
    changed["cargo_value_observations"]["baseline_reports"]
        .as_array_mut()
        .unwrap()
        .pop();
    assert!(!check(&changed).is_empty());
}

#[test]
fn derives_core_value_from_held_contract_and_observed_failures() {
    let (metadata, sources, mutants, observed) = fixture();
    let (value, attribution) = evaluate(&metadata, &sources, &mutants, &observed).unwrap();
    assert_eq!(value["status"], "PASS");
    assert_eq!(value["tests"][0]["classification"], "CORE");
    assert_eq!(value["tests"][0]["contract_id"], "value");
    assert_eq!(attribution["attributed_mutants"], 1);
    let mut other = metadata;
    other["declaration"]["required_contracts"][0]["id"] = json!("different");
    other["declaration"]["ranked_tests"][0]["contract"] = json!("different");
    assert_ne!(
        evaluate(&other, &sources, &mutants, &observed).unwrap().0,
        value
    );
}

#[test]
fn refuses_incomplete_baselines_counts_and_test_claims() {
    let (metadata, sources, mutants, observed) = fixture();
    let mut cases = Vec::new();
    let mut v = observed.clone();
    v["baseline_reports"].as_array_mut().unwrap().pop();
    cases.push(v);
    let mut v = observed.clone();
    v["baseline_reports"][0] = report(false);
    cases.push(v);
    let mut v = observed.clone();
    v["mutants"][0]["report"] = report(true);
    cases.push(v);
    let mut v = observed.clone();
    v["mutants"][0]["report"]["failed_nodeids"] = json!(["tests/contract.rs::check"]);
    cases.push(v);
    let mut v = observed.clone();
    v["mutants"][0]["report"]["tests"]["tests/contract.rs::check"]["cases"] = json!(2);
    cases.push(v);
    let mut v = observed.clone();
    v["mutants"][0]["report"]["unranked_failures"] = json!(["unknown"]);
    cases.push(v);
    for candidate in cases {
        assert!(
            evaluate(&metadata, &sources, &mutants, &candidate).is_err(),
            "{candidate}"
        );
    }
}

#[test]
fn refuses_extra_missing_duplicate_mutants_and_source_or_inventory_drift() {
    let (metadata, sources, mutants, observed) = fixture();
    let mut v = observed.clone();
    let duplicate = v["mutants"][0].clone();
    v["mutants"].as_array_mut().unwrap().push(duplicate);
    assert!(evaluate(&metadata, &sources, &mutants, &v).is_err());
    let mut v = observed.clone();
    v["mutants"][0]["id"] = json!("invented");
    assert!(evaluate(&metadata, &sources, &mutants, &v).is_err());
    let mut v = observed.clone();
    v["inventory_digest"] = json!("stale");
    assert!(evaluate(&metadata, &sources, &mutants, &v).is_err());
    let mut changed = mutants.clone();
    changed[0]["path"] = json!("tests/contract.rs");
    assert!(evaluate(&metadata, &sources, &changed, &observed).is_err());
    let mut changed = mutants;
    changed[0]["outcome"] = json!("SURVIVED");
    assert!(evaluate(&metadata, &sources, &changed, &observed).is_err());
}

#[test]
fn ordinary_native_rule_requires_independent_current_supporting_binding() {
    use crate::mutation_cargo_supporting_binding::SupportingInputs;
    let expected = SupportingInputs {
        schema_version: 1,
        algorithm: "sha256-framed-path-content-v1".into(),
        scope: "cargo-root-rust-discovery-v1".into(),
        digest: "a".repeat(64),
        files: 4,
    };
    let (mut campaign, mut payload) = generated_fixture();
    campaign.generated_value = None;
    payload["supporting_inputs"] = serde_json::to_value(&expected).unwrap();
    let runner = crate::mutation_receipt::RunnerState {
        components: None,
        error: None,
        mutation_testing_sha256: None,
    };
    let check = |value: &Value, supporting: Option<&SupportingInputs>| {
        crate::mutation_receipt::captured_generated_errors_with_installed(
            value.as_object().unwrap(),
            &campaign,
            &runner,
            |_| Some("e".repeat(64)),
            |_| false,
            None,
            Ok(supporting),
        )
    };
    assert!(!check(&payload, Some(&expected))
        .iter()
        .any(|e| e.detail.contains("supporting")));
    assert!(check(&payload, None)
        .iter()
        .any(|e| e.detail.contains("supporting")));
    let failure = crate::mutation_receipt::captured_generated_errors_with_installed(
        payload.as_object().unwrap(),
        &campaign,
        &runner,
        |_| Some("e".repeat(64)),
        |_| false,
        None,
        Err("fixture capture failed"),
    );
    assert!(failure.iter().any(|e| e.detail
        == "independently captured Cargo supporting inputs unavailable: fixture capture failed"));
    payload["supporting_inputs"]["files"] = json!(5);
    assert!(check(&payload, Some(&expected))
        .iter()
        .any(|e| e.detail.contains("supporting")));
    payload.as_object_mut().unwrap().remove("supporting_inputs");
    assert!(check(&payload, Some(&expected))
        .iter()
        .any(|e| e.detail.contains("supporting")));
}
