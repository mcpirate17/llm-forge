use super::native_receipts::native_receipt;
use super::*;
use crate::mutation_cargo_supporting_binding::SupportingInputs;

fn fixture() -> (Value, Value, SupportingInputs) {
    let (observation, sources, tests) = crate::mutation_mull_receipt::tests::fixture();
    let contract = json!({"campaign_id":"c","execution_profile":"mull-confined-v1",
        "manifest":"campaign.json","manifest_sha256":"f".repeat(64),
        "source_sha256":sources,"test_sha256":tests,"survivor_baseline":[]});
    let expected = SupportingInputs {
        schema_version: 2,
        algorithm: "sha256-framed-path-content-v1".into(),
        scope: "complete-snapshot-inputs-v2".into(),
        digest: "a".repeat(64),
        files: 3,
    };
    let mut receipt = native_receipt();
    for (key, value) in observation.as_object().unwrap() {
        receipt[key] = value.clone();
    }
    for key in ["campaign_id", "manifest", "manifest_sha256"] {
        receipt[key] = contract[key].clone();
    }
    let fields = json!({"status":"PASS","complete_campaign":true,"survivors":[],"mutation_score":1.0,
        "outcome_counts":{"KILLED":1,"SURVIVED":0,"TIMED_OUT":0,"NO_COVERAGE":0,"UNVIABLE":0,"ERROR":0},
        "accounting":{"planned":1,"completed":1,"failed":0,"censored":0,"unstarted":0},
        "baseline":{"returncode":0,"timed_out":false},"engine_result":{"returncode":0,"timed_out":false},
        "source_coverage":{"schema_version":1,"complete":true,"scored_by_source":{"src/a.cpp":1},"missing":[]}});
    for (key, value) in fields.as_object().unwrap() {
        receipt[key] = value.clone();
    }
    receipt["supporting_inputs"] = serde_json::to_value(&expected).unwrap();
    (receipt, contract, expected)
}

fn check(
    receipt: &Value,
    contract: Option<&Value>,
    expected: Option<&SupportingInputs>,
) -> Option<String> {
    let symbols = BTreeSet::new();
    let campaign = Campaign {
        campaign_id: "c",
        generated: true,
        ordinary_cargo: false,
        execution_profile: Some("mull-confined-v1"),
        ordinary_contract: contract,
        declares_value_analysis: false,
        source_symbols: &symbols,
    };
    rejection_with_supporting(
        &campaign,
        receipt,
        &json!({}),
        |path| {
            Ok(if path == crate::mutation_native_runner::FINGERPRINT_KEY {
                Some("e".repeat(64))
            } else {
                receipt["source_sha256"][path].as_str().map(str::to_owned)
            })
        },
        |_| Ok(false),
        None,
        expected,
    )
    .unwrap()
}

#[test]
fn bound_mull_corpus_requires_independent_inputs_and_retained_replay() {
    let (receipt, contract, expected) = fixture();
    assert!(check(&receipt, Some(&contract), Some(&expected)).is_none());
    assert!(check(&receipt, None, Some(&expected))
        .unwrap()
        .contains("contract missing"));
    assert!(check(&receipt, Some(&contract), None)
        .unwrap()
        .contains("unavailable"));
    for field in ["supporting_inputs", "mull_observation", "mull_definition"] {
        let mut bad = receipt.clone();
        bad.as_object_mut().unwrap().remove(field);
        assert!(
            check(&bad, Some(&contract), Some(&expected)).is_some(),
            "{field}"
        );
    }
    let mut bad = receipt.clone();
    bad["runner_kind"] = json!("python");
    assert!(check(&bad, Some(&contract), Some(&expected))
        .unwrap()
        .contains("native runner"));
}

#[test]
fn consistent_survivor_report_cannot_claim_pass_or_an_unaccepted_ratchet() {
    let (mut receipt, mut contract, expected) = fixture();
    let observation = &mut receipt["mull_observation"];
    let report = &mut observation["report"];
    report["files"]["/private/worktree/src/a.cpp"]["mutants"][0]["status"] = json!("Survived");
    observation["report_sha256"] =
        json!(crate::mutation_mull_receipt::report_sha256(report).unwrap());
    receipt["mutants"][0]["outcome"] = json!("SURVIVED");
    receipt["outcome_counts"]["KILLED"] = json!(0);
    receipt["outcome_counts"]["SURVIVED"] = json!(1);
    receipt["mutation_score"] = json!(0.0);
    receipt["survivors"] = json!([receipt["mutants"][0]["id"]]);
    assert!(check(&receipt, Some(&contract), Some(&expected))
        .unwrap()
        .contains("status or score"));
    receipt["status"] = json!("RATCHET_HELD");
    assert!(check(&receipt, Some(&contract), Some(&expected))
        .unwrap()
        .contains("outside the verified baseline"));
    contract["survivor_baseline"] = receipt["survivors"].clone();
    assert!(check(&receipt, Some(&contract), Some(&expected)).is_none());
}
