use super::*;
use std::fs;

fn fixture() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("Cargo.toml"),
        "[package]\nname='fixture'\nversion='0.1.0'\n",
    )
    .unwrap();
    fs::write(root.path().join("Cargo.lock"), "version=4\n").unwrap();
    fs::create_dir(root.path().join("src")).unwrap();
    fs::write(root.path().join("src/lib.rs"), "pub mod a;\n").unwrap();
    fs::write(root.path().join("src/a.rs"), "pub fn a() {}\n").unwrap();
    root
}
#[test]
fn held_supporting_inventory_rejects_bytes_and_file_additions() {
    for change in ["source", "manifest", "lock", "added"] {
        let root = fixture();
        let held = Held::capture(root.path(), 32, &BTreeMap::new()).unwrap();
        let path = match change {
            "source" => "src/lib.rs",
            "manifest" => "Cargo.toml",
            "lock" => "Cargo.lock",
            _ => "src/added.rs",
        };
        fs::write(root.path().join(path), "changed").unwrap();
        assert!(held.verify().is_err(), "{change}");
    }
}
#[test]
fn independent_snapshot_must_match_and_all_targets_be_included() {
    let host = fixture();
    let snapshot = fixture();
    let pin = crate::mutation_campaign::sha256_path(&host.path().join("src/a.rs")).unwrap();
    let required = BTreeMap::from([("src/a.rs".into(), pin)]);
    let held = Held::capture(host.path(), 32, &required).unwrap();
    let peer = held.capture_peer(snapshot.path()).unwrap();
    assert_eq!(held.inventory(), peer.inventory());
    fs::write(snapshot.path().join("src/lib.rs"), "different").unwrap();
    assert!(peer.verify().is_err());
    let excluded = BTreeMap::from([("target/hidden.rs".into(), "a".repeat(64))]);
    assert!(Held::capture(host.path(), 32, &excluded).is_err());
}

fn eligible_fixture() -> (serde_json::Value, serde_json::Value) {
    let contract = serde_json::json!({"campaign_id":"c","generated":true,"manifest":"campaign.json", "manifest_sha256":"f".repeat(64),
        "mutation_engine":"cargo-mutants","source_sha256":{"src/lib.rs":"a".repeat(64)},
        "test_sha256":{"tests/contract.rs":"b".repeat(64)}});
    let payload = serde_json::json!({"runner_kind":"forge-native","campaign_id":"c",
        "status":"PASS","schema_version":"llm.mutation-testing.receipt.v3","mutants_are_generated":true,
        "manifest":"campaign.json","manifest_sha256":"f".repeat(64),
        "source_sha256":{"src/lib.rs":"a".repeat(64)},"test_sha256":{"tests/contract.rs":"b".repeat(64)},
        "execution_grants_sha256":"a".repeat(64),
        "process_cleanup":{"schema_version":1,"complete":true,"commands_observed":1},
        "process_commands":[{"state":"observed","confinement":{"cleanup_complete":true,"outcome":"exited","errors":[],"resources":{"verified":true}}}],
        "snapshot":{"removed":true},
        "native_runner":{"schema_version":conductor_native::mutation_native_runner::RUNNER_SCHEMA,"forge_version":"fixture",
            "executable_sha256":"d".repeat(64),"compiled_source_sha256":"e".repeat(64),"compiled_git_commit":"f".repeat(40),"source_matches_git":true},
        "mutants":[{"path":"src/lib.rs","outcome":"KILLED"}],
        "source_coverage":{"schema_version":1,"complete":true,"scored_by_source":{"src/lib.rs":1},"missing":[]},
        "supporting_inputs":{"schema_version":1,"algorithm":"sha256-framed-path-content-v1","scope":"cargo-root-rust-discovery-v1","digest":"a".repeat(64),"files":4}});
    (contract, payload)
}

#[test]
fn fest_snapshot_capture_is_selected_by_verified_contract_not_receipt_profile() {
    use conductor_native::mutation_evidence::CapturedReceipt;
    let (mut contract, mut payload) = eligible_fixture();
    contract["language"] = serde_json::json!("python");
    contract["mutation_engine"] = serde_json::json!("fest");
    payload["execution_profile"] = serde_json::json!("fest-confined-v1");
    payload["supporting_inputs"]["schema_version"] = serde_json::json!(2);
    payload["supporting_inputs"]["scope"] = serde_json::json!("complete-snapshot-inputs-v2");
    let receipt = CapturedReceipt {
        path: "receipt.json".into(),
        text: payload.to_string(),
    };
    assert!(workspace_required_for(
        std::slice::from_ref(&contract),
        std::slice::from_ref(&receipt),
        || panic!("unmarked Python must not request native snapshot capture"),
        None,
    )
    .unwrap()
    .is_empty());
    contract["execution_profile"] = serde_json::json!("fest-confined-v1");
    let requested = workspace_required_for(
        std::slice::from_ref(&contract),
        std::slice::from_ref(&receipt),
        || Ok(Some("e".repeat(64))),
        None,
    )
    .unwrap();
    let pins = requested["c"].as_ref().unwrap();
    assert_eq!(pins.manifest, "campaign.json");
    assert_eq!(pins.pins.len(), 2);
    assert!(
        required_for(
            std::slice::from_ref(&contract),
            std::slice::from_ref(&receipt),
            || Ok(Some("e".repeat(64))),
            None,
        )
        .unwrap()
        .is_none(),
        "Fest must never use the root Cargo hash domain"
    );
    contract["execution_profile"] = serde_json::json!("fest-confined-v9");
    assert!(
        workspace_required_for(&[contract], &[receipt], || panic!("unknown profile"), None,)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn inventory_is_only_requested_for_admissible_native_ordinary_receipts() {
    use conductor_native::mutation_evidence::CapturedReceipt;
    let (contract, mut payload) = eligible_fixture();
    let row = |value: &serde_json::Value| CapturedReceipt {
        path: "evidence.json".into(),
        text: value.to_string(),
    };
    let check = |value: &serde_json::Value| {
        required_for(
            std::slice::from_ref(&contract),
            &[row(value)],
            || Ok(Some("e".repeat(64))),
            None,
        )
        .unwrap()
    };
    assert_eq!(check(&payload).unwrap().len(), 2);
    for (field, value) in [
        ("status", serde_json::json!("ERROR")),
        ("runner_kind", serde_json::json!("python")),
        ("manifest_sha256", serde_json::json!("wrong")),
    ] {
        let mut bad = payload.clone();
        bad[field] = value;
        assert!(check(&bad).is_none(), "{field}");
    }
    let mut bad = payload.clone();
    bad["process_cleanup"]["complete"] = serde_json::json!(false);
    assert!(check(&bad).is_none());
    let mut bad = payload.clone();
    bad["native_runner"]["compiled_source_sha256"] = serde_json::json!("0".repeat(64));
    assert!(check(&bad).is_none());
    let mut failed = payload.clone();
    failed["status"] = serde_json::json!("ERROR");
    let python = serde_json::json!({"runner_kind":"python","campaign_id":"c"});
    assert!(required_for(
        std::slice::from_ref(&contract),
        &[row(&failed), row(&python)],
        || panic!("non-passing native and Python rows must not trigger native capture"),
        None
    )
    .unwrap()
    .is_none());
    let stale = serde_json::json!({"source_drifted":true});
    assert!(stale_native_contract(&stale, &payload, true));
    assert!(!stale_native_contract(&stale, &python, true));
    payload.as_object_mut().unwrap().remove("supporting_inputs");
    assert!(check(&payload).is_none());
}

#[test]
fn eligible_receipt_checks_schema_and_generated_flag_before_fingerprint() {
    use conductor_native::mutation_evidence::CapturedReceipt;
    let (contract, payload) = eligible_fixture();
    let row = |value: &serde_json::Value| CapturedReceipt {
        path: "evidence.json".into(),
        text: value.to_string(),
    };
    let mut calls = 0;
    let accepted = eligible_receipt(
        &row(&payload),
        std::slice::from_ref(&contract),
        &mut || {
            calls += 1;
            Ok(Some("e".repeat(64)))
        },
        None,
    )
    .unwrap();
    assert_eq!(accepted.as_deref(), Some("c"));
    assert_eq!(calls, 1);
    for (field, value) in [
        ("schema_version", Some(serde_json::json!("wrong"))),
        ("schema_version", None),
        ("mutants_are_generated", Some(serde_json::json!(false))),
        ("mutants_are_generated", None),
    ] {
        let mut bad = payload.clone();
        if let Some(value) = value {
            bad[field] = value;
        } else {
            bad.as_object_mut().unwrap().remove(field);
        }
        assert!(
            eligible_receipt(
                &row(&bad),
                std::slice::from_ref(&contract),
                &mut || panic!("invalid {field} must not request fingerprint"),
                None
            )
            .unwrap()
            .is_none(),
            "{field}"
        );
    }
}

#[test]
fn eligible_receipt_checks_each_contract_guard_before_fingerprint() {
    use conductor_native::mutation_evidence::CapturedReceipt;
    let (contract, payload) = eligible_fixture();
    let row = CapturedReceipt {
        path: "evidence.json".into(),
        text: payload.to_string(),
    };
    for (field, value) in [
        ("source_drifted", serde_json::json!(true)),
        ("generated", serde_json::json!(false)),
        ("mutation_engine", serde_json::json!("fest")),
        ("generated_value", serde_json::json!({})),
    ] {
        let mut bad = contract.clone();
        bad[field] = value;
        assert!(
            eligible_receipt(
                &row,
                &[bad],
                &mut || panic!("invalid {field} must not request fingerprint"),
                None
            )
            .unwrap()
            .is_none(),
            "{field}"
        );
    }
}

#[test]
fn removing_a_required_host_or_snapshot_file_invalidates_the_held_inventory() {
    for removed in ["Cargo.toml", "Cargo.lock", "src/a.rs"] {
        let host = fixture();
        let snapshot = fixture();
        let pin = crate::mutation_campaign::sha256_path(&host.path().join("src/a.rs")).unwrap();
        let required = BTreeMap::from([("src/a.rs".into(), pin)]);
        let host_held = Held::capture(host.path(), 32, &required).unwrap();
        let snapshot_held = host_held.capture_peer(snapshot.path()).unwrap();
        fs::remove_file(snapshot.path().join(removed)).unwrap();
        assert!(snapshot_held.verify().is_err(), "snapshot {removed}");
        assert!(
            host_held.verify().is_ok(),
            "host should remain intact for {removed}"
        );
        fs::remove_file(host.path().join(removed)).unwrap();
        assert!(host_held.verify().is_err(), "host {removed}");
    }
}

#[test]
fn workspace_capture_errors_are_per_campaign_and_do_not_discard_other_profiles() {
    use conductor_native::mutation_cargo_supporting_binding::SupportingInputs;
    let required = BTreeMap::from([
        ("bad".to_owned(), Err("conflicting source pins".to_owned())),
        (
            "good".to_owned(),
            Ok(WorkspaceRequired {
                pins: BTreeMap::new(),
                manifest: "campaigns/good.json".into(),
            }),
        ),
    ]);
    let results = capture_workspace_profiles(required, |_| {
        Ok(SupportingInputs {
            schema_version: 2,
            algorithm: "sha256-framed-path-content-v1".into(),
            scope: "complete-snapshot-inputs-v2".into(),
            digest: "a".repeat(64),
            files: 3,
        })
    });
    assert_eq!(
        results["bad"].as_ref().unwrap_err(),
        "conflicting source pins"
    );
    assert_eq!(
        results["good"].as_ref().unwrap().scope,
        "complete-snapshot-inputs-v2"
    );
}

#[test]
fn empty_or_ineligible_contracts_skip_both_fingerprint_scans() {
    use conductor_native::mutation_evidence::CapturedReceipt;
    let (contract, payload) = eligible_fixture();
    let receipt = CapturedReceipt {
        path: "receipt.json".into(),
        text: payload.to_string(),
    };
    let receipts = [
        receipt,
        CapturedReceipt {
            path: "malformed.json".into(),
            text: "{".into(),
        },
    ];
    let check = |contracts: &[serde_json::Value]| {
        assert!(required_for(
            contracts,
            &receipts,
            || panic!("ineligible inventory requested fingerprint"),
            None
        )
        .unwrap()
        .is_none());
        assert!(workspace_required_for(
            contracts,
            &receipts,
            || panic!("ineligible inventory requested fingerprint"),
            None
        )
        .unwrap()
        .is_empty());
    };
    check(&[]);
    check(&[
        serde_json::Value::Null,
        serde_json::json!({"generated":"invalid"}),
    ]);
    for (field, value) in [
        ("source_drifted", serde_json::json!(true)),
        ("generated", serde_json::json!(false)),
        ("mutation_engine", serde_json::json!("fest")),
        ("generated_value", serde_json::json!({})),
    ] {
        let mut ineligible = contract.clone();
        ineligible[field] = value;
        assert!(!eligible_contract(&ineligible), "{field}");
        check(&[ineligible]);
    }
}

#[test]
fn eligible_contract_preserves_fingerprint_failures_in_both_scans() {
    use conductor_native::mutation_evidence::CapturedReceipt;
    let (contract, payload) = eligible_fixture();
    let receipt = CapturedReceipt {
        path: "receipt.json".into(),
        text: payload.to_string(),
    };
    assert!(required_for(
        std::slice::from_ref(&contract),
        std::slice::from_ref(&receipt),
        || anyhow::bail!("fingerprint unavailable"),
        None
    )
    .unwrap_err()
    .to_string()
    .contains("fingerprint unavailable"));
    assert!(workspace_required_for(
        &[contract],
        &[receipt],
        || anyhow::bail!("fingerprint unavailable"),
        None
    )
    .err()
    .expect("workspace fingerprint error")
    .to_string()
    .contains("fingerprint unavailable"));
}

#[test]
fn receipt_detail_can_override_summary_before_contract_selection() {
    use conductor_native::mutation_evidence::CapturedReceipt;
    let (contract, mut payload) = eligible_fixture();
    payload["runner_kind"] = serde_json::json!("python");
    payload["campaign_id"] = serde_json::json!("different");
    payload["status"] = serde_json::json!("ERROR");
    payload["detail"] = serde_json::json!({"encoding":"json","runner_kind":"forge-native","campaign_id":"c","status":"PASS"});
    let receipt = CapturedReceipt {
        path: "receipt.json".into(),
        text: payload.to_string(),
    };
    let result = required_for(&[contract], &[receipt], || Ok(Some("e".repeat(64))), None).unwrap();
    assert_eq!(result.unwrap().len(), 2);
}
