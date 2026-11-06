use super::*;
use serde_json::{json, Map};

pub(crate) fn fixture() -> (Selector, Vec<String>, Map<String, Value>) {
    let selector = Selector {
        schema_version: 1,
        target: Target::Test {
            name: "contract".into(),
        },
        filter: Some("critical".into()),
    };
    let args: Vec<String> = [
        "cargo",
        "test",
        "--manifest-path",
        "crate/Cargo.toml",
        "--package",
        "demo",
        "--test=contract",
        "--",
        "critical",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let baseline = json!([
        "cargo",
        "test",
        "--manifest-path",
        "crate/Cargo.toml",
        "--package",
        "demo",
        "--locked",
        "--offline",
        "--test=contract",
        "--",
        "critical"
    ]);
    let obs = json!({"frames":1,"passed":1,"ignored":0,"filtered":2,"inventory_sha256":"a".repeat(64),"scope":"observed selected libtest output, not producer attestation"});
    let payload = json!({"test_argv":args,"baseline_argv":baseline,
        "workspace_execution":{"package":"demo","profile":"cargo-workspace-v3","selector":selector,"test_scope":"selected-target-full-source-snapshot","targets":["test:contract"],"selected_target":{"kind":"test","name":"contract","source":"crate/tests/contract.rs"}},
        "workspace_baseline":{"direct":obs,"engine":obs,"engine_log_sha256":"b".repeat(64)},
        "process_commands":[{"argv":["cargo","metadata","--manifest-path","crate/Cargo.toml","--format-version=1","--locked","--offline"]},{"argv":baseline},{"argv":["/cargo-mutants","mutants","--manifest-path","crate/Cargo.toml","--in-place","--timeout","60","--output","/scratch/out","--package","demo","--file","src/a.rs","--no-config","--cargo-arg=--locked","--cargo-arg=--offline","--test-package","demo","--cargo-arg=--test=contract","--cargo-test-arg=--","--cargo-test-arg=critical"]}]}).as_object().unwrap().clone();
    (selector, args, payload)
}

#[test]
fn selected_receipt_is_bound_to_verified_contract_and_observed_cases() {
    let (selector, args, payload) = fixture();
    assert!(validate(
        Some("cargo-workspace-v3"),
        Some(&selector),
        &args,
        &json!({"src/a.rs":"e".repeat(64)}),
        &payload
    )
    .is_ok());
    for (path, replacement) in [
        (
            "selector",
            json!({"schema_version":1,"target":{"kind":"lib"},"filter":null}),
        ),
        (
            "target",
            json!({"kind":"test","name":"other","source":"crate/tests/contract.rs"}),
        ),
        (
            "source",
            json!({"kind":"test","name":"contract","source":"../escape.rs"}),
        ),
    ] {
        let mut changed = payload.clone();
        if path == "selector" {
            changed.get_mut("workspace_execution").unwrap()["selector"] = replacement;
        } else {
            changed.get_mut("workspace_execution").unwrap()["selected_target"] = replacement;
        }
        assert!(
            validate(
                Some("cargo-workspace-v3"),
                Some(&selector),
                &args,
                &json!({"src/a.rs":"e".repeat(64)}),
                &changed
            )
            .is_err(),
            "{path}"
        );
    }
}

#[test]
fn selected_receipt_refuses_incomplete_or_broadened_observations() {
    let (selector, args, payload) = fixture();
    for key in [
        "zero",
        "digest",
        "argv",
        "missing",
        "workspace",
        "separator",
        "overcount",
    ] {
        let mut changed = payload.clone();
        match key {
            "zero" => {
                changed.get_mut("workspace_baseline").unwrap()["direct"]["passed"] = json!(0);
                changed.get_mut("workspace_baseline").unwrap()["engine"]["passed"] = json!(0);
            }
            "digest" => {
                changed.get_mut("workspace_baseline").unwrap()["engine"]["inventory_sha256"] =
                    json!("c".repeat(64))
            }
            "argv" => changed.get_mut("process_commands").unwrap()[2]["argv"][17] = json!("other"),
            "workspace" | "separator" => {
                let args = changed.get_mut("process_commands").unwrap()[2]["argv"]
                    .as_array_mut()
                    .unwrap();
                args.insert(
                    14,
                    json!(if key == "workspace" {
                        "--test-workspace=true"
                    } else {
                        "--"
                    }),
                );
            }
            "overcount" => {
                for side in ["direct", "engine"] {
                    changed.get_mut("workspace_baseline").unwrap()[side]["ignored"] =
                        json!(100_000);
                }
            }
            _ => {
                changed.remove("workspace_baseline");
            }
        }
        assert!(
            validate(
                Some("cargo-workspace-v3"),
                Some(&selector),
                &args,
                &json!({"src/a.rs":"e".repeat(64)}),
                &changed
            )
            .is_err(),
            "{key}"
        );
    }
}

#[test]
fn selected_receipt_refuses_file_patterns_and_profile_downgrades() {
    let (selector, args, payload) = fixture();
    for file in [
        "src/*.rs",
        "src/{a}.rs",
        "src/\\a.rs",
        "../a.rs",
        "src/other.rs",
    ] {
        let mut changed = payload.clone();
        changed.get_mut("process_commands").unwrap()[2]["argv"][12] = json!(file);
        assert!(
            validate(
                Some("cargo-workspace-v3"),
                Some(&selector),
                &args,
                &json!({"src/a.rs":"e".repeat(64)}),
                &changed
            )
            .is_err(),
            "{file}"
        );
    }
    assert!(validate(None, None, &[], &Value::Null, &payload).is_err());
    assert!(validate(
        Some("cargo-workspace-v2"),
        None,
        &[],
        &Value::Null,
        &payload
    )
    .is_err());
    assert!(validate(
        Some("cargo-workspace-v3"),
        None,
        &args,
        &json!({"src/a.rs":"e".repeat(64)}),
        &payload
    )
    .is_err());
    assert!(validate(
        Some("cargo-workspace-v2"),
        None,
        &[],
        &Value::Null,
        &Map::new()
    )
    .is_ok());
}

#[test]
fn unfiltered_observation_refuses_filtered_counts() {
    let (mut selector, _, payload) = fixture();
    selector.filter = None;
    let row = &payload["workspace_baseline"]["direct"];
    assert!(!observation(row, &selector));
    let mut allowed = row.clone();
    allowed["filtered"] = json!(0);
    assert!(observation(&allowed, &selector));
}

#[test]
fn engine_file_arguments_bind_the_complete_verified_source_set() {
    let base: Vec<String> = [
        "cargo",
        "test",
        "--manifest-path",
        "crate/Cargo.toml",
        "--package",
        "demo",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let mut argv = vec![
        "/cargo-mutants",
        "mutants",
        "--manifest-path",
        "crate/Cargo.toml",
        "--in-place",
        "--timeout",
        "60",
        "--output",
        "/out",
        "--package",
        "demo",
        "--file",
        "member/a.rs",
        "--file",
        "member/b.rs",
        "--no-config",
        "--cargo-arg=--locked",
        "--cargo-arg=--offline",
    ];
    let sources = json!({"workspace/member/a.rs":"a", "workspace/member/b.rs":"b"});
    assert!(engine_prefix(&argv, &base, 0, &sources).is_ok());
    argv[14] = "other/b.rs";
    assert!(engine_prefix(&argv, &base, 0, &sources).is_err());
    argv[14] = "member/a.rs";
    assert!(engine_prefix(&argv, &base, 0, &sources).is_err());
    argv[14] = "member/*.rs";
    assert!(engine_prefix(&argv, &base, 0, &sources).is_err());
    argv[14] = "b.rs";
    assert!(engine_prefix(&argv, &base, 0, &sources).is_err());
    argv.drain(13..15);
    argv[12] = "b.rs";
    assert!(engine_prefix(&argv, &base, 0, &json!({"b.rs":"a"})).is_err());
}

fn generated_fixture() -> (
    crate::mutation_manifest::CampaignContract,
    crate::mutation_receipt::RunnerState,
    crate::mutation_cargo_supporting_binding::SupportingInputs,
    Map<String, Value>,
) {
    let (selector, argv, mut payload) = fixture();
    let campaign = serde_json::from_value(json!({"campaign_id":"c","title":"v3", "language":"rust","mutation_engine":"cargo-mutants","expected_mutations":0,
        "manifest":"campaigns/a.json","manifest_sha256":"f".repeat(64),"source_sha256":{"src/a.rs":"e".repeat(64)},"source_symbols":{},"test_scopes":{},
        "ranked_tests":[],"ranked_test_paths":[],"planned_mutations":[],"mutations":[],"test_argv":argv,"timeout_seconds":60,"blocked_process_substrings":[],"poll_seconds":0,
        "environment":{},"host_read_dependencies":[],"execution_profile":"cargo-workspace-v3","workspace_selector":selector,"value_analysis_payload":null,"value_analysis":null,
        "source_drifted":false,"generated":true,"survivor_baseline":[],"test_sha256":{"tests/a.rs":"b".repeat(64)}})).unwrap();
    let runner = crate::mutation_receipt::RunnerState {
        components: None,
        error: None,
        mutation_testing_sha256: None,
    };
    let supporting = crate::mutation_cargo_supporting_binding::SupportingInputs {
        schema_version: 2,
        algorithm: "sha256-framed-path-content-v1".into(),
        scope: "complete-snapshot-inputs-v2".into(),
        digest: "a".repeat(64),
        files: 4,
    };
    let extra = json!({"schema_version":"llm.mutation-testing.receipt.v3","mutants_are_generated":true,"status":"PASS","campaign_id":"c","manifest":"campaigns/a.json","manifest_sha256":"f".repeat(64),"source_sha256":{"src/a.rs":"e".repeat(64)},"test_sha256":{"tests/a.rs":"b".repeat(64)},"supporting_inputs":supporting,
        "runner_kind":"forge-native","native_runner":{"schema_version":"llm.mutation-testing.native-runner.v2","forge_version":"0.0.0","executable_sha256":"d".repeat(64),"compiled_source_sha256":"e".repeat(64),"compiled_git_commit":"f".repeat(40),"source_matches_git":true},
        "execution_grants_sha256":"a".repeat(64),"process_cleanup":{"schema_version":1,"complete":true,"commands_observed":3},"snapshot":{"removed":true},"baseline":{"returncode":0,"timed_out":false},"survivors":[],"mutants":[{"id":"m","path":"src/a.rs","outcome":"KILLED"}],"source_coverage":{"schema_version":1,"complete":true,"scored_by_source":{"src/a.rs":1},"missing":[]},"outcome_counts":{"KILLED":1,"NO_COVERAGE":0,"UNVIABLE":0,"ERROR":0,"SURVIVED":0,"TIMED_OUT":0},"complete_campaign":true,"mutation_score":1.0});
    for (k, v) in extra.as_object().unwrap() {
        payload.insert(k.clone(), v.clone());
    }
    for row in payload
        .get_mut("process_commands")
        .unwrap()
        .as_array_mut()
        .unwrap()
    {
        row["state"] = json!("observed");
        row["confinement"] = json!({"cleanup_complete":true,"outcome":"exited","exit_code":0,"signal":null,"resources":{"verified":true},"errors":[]});
    }
    (campaign, runner, supporting, payload)
}

#[test]
fn generated_receipt_seam_accepts_v3_and_refuses_changed_selected_observation() {
    let (campaign, mut runner, supporting, mut payload) = generated_fixture();
    runner.components = Some(json!({"legacy":"a"}));
    runner.mutation_testing_sha256 = Some("a".into());
    let check = |payload: &Map<String, Value>| {
        crate::mutation_receipt::captured_generated_errors_with_installed(
            payload,
            &campaign,
            &runner,
            |_| Some("e".repeat(64)),
            |_| false,
            None,
            Ok(Some(&supporting)),
        )
    };
    let good = check(&payload);
    assert!(good.is_empty(), "{good:?}");
    payload.get_mut("workspace_baseline").unwrap()["engine"]["inventory_sha256"] =
        json!("c".repeat(64));
    assert!(check(&payload)
        .iter()
        .any(|e| e.detail.contains("baseline")));
    payload.insert("runner_kind".into(), json!("python"));
    payload.insert("runner_components_sha256".into(), json!({"legacy":"a"}));
    payload.insert("runner_sha256".into(), json!("a"));
    assert!(check(&payload)
        .iter()
        .any(|e| e.detail.contains("requires a native runner")));
}

#[test]
fn generated_receipt_seam_requires_confined_fest_snapshot_and_native_runner() {
    let (mut campaign, runner, supporting, mut payload) = generated_fixture();
    campaign.language = "python".into();
    campaign.mutation_engine = "fest".into();
    campaign.execution_profile = Some("fest-confined-v1".into());
    campaign.workspace_selector = None;
    campaign.source_sha256 = json!({"src/a.py":"e".repeat(64)});
    campaign.test_sha256 = json!({"tests/test_a.py":"b".repeat(64)});
    payload.insert("source_sha256".into(), campaign.source_sha256.clone());
    payload.insert("test_sha256".into(), campaign.test_sha256.clone());
    payload.insert(
        "fest_cache".into(),
        json!({"schema_version":1,"path":"/private/cache",
        "initially_empty":true,"read_only":true,"removed":true}),
    );
    payload.get_mut("mutants").unwrap()[0]["path"] = json!("src/a.py");
    let evidence = crate::mutation_fest_receipt::attribution_tests::fixture();
    for key in ["attribution", "fest_attribution_observations", "test_value"] {
        payload.insert(key.into(), evidence[key].clone());
    }
    payload.get_mut("source_coverage").unwrap()["scored_by_source"] = json!({"src/a.py":1});
    let check =
        |payload: &Map<String, Value>,
         identity: Option<&crate::mutation_cargo_supporting_binding::SupportingInputs>| {
            crate::mutation_receipt::captured_generated_errors_with_installed(
                payload,
                &campaign,
                &runner,
                |_| Some("e".repeat(64)),
                |_| false,
                None,
                Ok(identity),
            )
        };
    assert!(check(&payload, Some(&supporting)).is_empty());
    let mut missing_attribution = payload.clone();
    missing_attribution.remove("fest_attribution_observations");
    assert!(check(&missing_attribution, Some(&supporting))
        .iter()
        .any(|e| e.detail.contains("attribution")));
    let mut cache_missing = payload.clone();
    cache_missing.remove("fest_cache");
    assert!(check(&cache_missing, Some(&supporting))
        .iter()
        .any(|e| e.detail.contains("cache")));
    let mut retained = payload.clone();
    retained["fest_cache"]["removed"] = json!(false);
    assert!(check(&retained, Some(&supporting))
        .iter()
        .any(|e| e.detail.contains("cleanup")));
    assert!(check(&payload, None)
        .iter()
        .any(|e| e.detail.contains("unavailable")));
    let mut drift = payload.clone();
    drift["supporting_inputs"]["digest"] = json!("c".repeat(64));
    assert!(check(&drift, Some(&supporting))
        .iter()
        .any(|e| e.detail.contains("changed")));
    let mut python = payload;
    python.insert("runner_kind".into(), json!("python"));
    assert!(check(&python, Some(&supporting))
        .iter()
        .any(|e| e.detail.contains("requires a native runner")));
}
