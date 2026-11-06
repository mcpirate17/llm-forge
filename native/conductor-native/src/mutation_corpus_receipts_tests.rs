use super::*;
fn campaign<'a>(symbols: &'a BTreeSet<String>) -> Campaign<'a> {
    Campaign {
        campaign_id: "c",
        generated: false,
        ordinary_cargo: false,
        ordinary_contract: None,
        declares_value_analysis: false,
        source_symbols: symbols,
    }
}
#[test]
fn status_runner_and_tree_pins_are_checked_without_equating_ratchet_with_pass() {
    let symbols = BTreeSet::from(["src/symbol.py".to_owned()]);
    let c = campaign(&symbols);
    let runner = json!({"runner":"a"});
    let receipt = json!({"status":"RATCHET_HELD", "runner_components_sha256":{"runner":"a"},
        "source_sha256":{"src/file.py":"one","src/symbol.py":"old"}});
    assert_eq!(
        rejection(
            &c,
            &receipt,
            &runner,
            |_| Ok(Some("one".into())),
            |_| Ok(false)
        )
        .unwrap(),
        None
    );
    assert!(rejection(
        &c,
        &receipt,
        &runner,
        |_| Ok(Some("different".into())),
        |_| Ok(false)
    )
    .unwrap()
    .is_some());
    let mut bad = receipt.clone();
    bad["status"] = json!("TIMEOUT");
    assert_eq!(
        rejection(&c, &bad, &runner, |_| Ok(None), |_| Ok(false))
            .unwrap()
            .as_deref(),
        Some("status=TIMEOUT")
    );
    bad = receipt.clone();
    bad["runner_components_sha256"] = json!({"runner":"old"});
    assert!(
        rejection(&c, &bad, &runner, |_| Ok(Some("one".into())), |_| Ok(false))
            .unwrap()
            .is_some()
    );
    assert!(
        rejection(&c, &bad, &runner, |_| Ok(Some("one".into())), |_| Ok(true))
            .unwrap()
            .is_none()
    );
}
#[test]
fn newest_acceptable_uses_timestamp_and_preserves_first_equal_timestamp() {
    let receipts = vec![
        json!({"generated_at":"2026-01-01","id":"old"}),
        json!({"generated_at":"2026-02-01","id":"first"}),
        json!({"generated_at":"2026-02-01","id":"second"}),
    ];
    let chosen = newest_acceptable(&receipts, |_| Ok(None)).unwrap().unwrap();
    assert_eq!(chosen["id"], "first");
    assert!(newest_acceptable(&[json!({"generated_at":42})], |_| Ok(None)).is_err());
}
#[test]
fn missing_invalid_and_inert_value_evidence_are_distinct() {
    let symbols = BTreeSet::new();
    let c = campaign(&symbols);
    let (missing, inert) = value_findings(&c, None).unwrap();
    assert_eq!(missing[0]["reason"], "NO_VALUE_ANALYSIS");
    assert!(inert.is_empty());
    let bad = json!({"test_value":{"tests":[{"nodeid":"t","classification":"DELETE_CANDIDATE"},{"nodeid":3}]}});
    let (unmeasured, inert) = value_findings(&c, Some(&bad)).unwrap();
    assert_eq!(unmeasured[0]["reason"], "INVALID_VALUE_ANALYSIS");
    assert_eq!(inert[0]["nodeid"], "t");
    let generated = Campaign {
        generated: true,
        ..c
    };
    assert!(value_findings(&generated, None).unwrap().0.is_empty());
}

mod native_receipts {
    use super::*;
    use crate::mutation_native_runner::{RUNNER_KIND, RUNNER_SCHEMA};

    fn native_receipt() -> Value {
        json!({
            "status": "RATCHET_HELD",
            "execution_grants_sha256": "a".repeat(64),
            "process_cleanup": {"schema_version":1,"complete":true,"commands_observed":1},
            "process_commands": [{"state":"observed","confinement":{"cleanup_complete":true,"outcome":"exited","exit_code":0,"signal":null,"resources":{"verified":true},"errors":[]}}],
            "snapshot": {"removed":true},
            "runner_kind": RUNNER_KIND,
            "source_sha256": {"src/a.rs": "e".repeat(64)},
            "mutants": [{"path":"src/a.rs","outcome":"KILLED"}],
            "source_coverage": {"schema_version":1,"complete":true,"scored_by_source":{"src/a.rs":1},"missing":[]},
            "native_runner": {
                "schema_version": RUNNER_SCHEMA,
                "forge_version": "0.0.0",
                "executable_sha256": "d".repeat(64),
                "compiled_source_sha256": "e".repeat(64),
                "compiled_git_commit": "f".repeat(40),
                "source_matches_git": true,
            },
        })
    }

    #[test]
    fn ordinary_native_receipt_requires_independent_current_supporting_identity() {
        use crate::mutation_cargo_supporting_binding::SupportingInputs;
        let symbols = BTreeSet::new();
        let contract = json!({"campaign_id":"c","manifest":"campaign.json","manifest_sha256":"f".repeat(64),
            "source_sha256":{"src/a.rs":"e".repeat(64)},"test_sha256":{"tests/contract.rs":"b".repeat(64)}});
        let campaign = Campaign {
            campaign_id: "c",
            generated: true,
            ordinary_cargo: true,
            ordinary_contract: Some(&contract),
            declares_value_analysis: false,
            source_symbols: &symbols,
        };
        let expected = SupportingInputs {
            schema_version: 1,
            algorithm: "sha256-framed-path-content-v1".into(),
            scope: "cargo-root-rust-discovery-v1".into(),
            digest: "a".repeat(64),
            files: 3,
        };
        let mut receipt = native_receipt();
        receipt["campaign_id"] = contract["campaign_id"].clone();
        receipt["manifest"] = contract["manifest"].clone();
        receipt["manifest_sha256"] = contract["manifest_sha256"].clone();
        receipt["test_sha256"] = contract["test_sha256"].clone();
        receipt["supporting_inputs"] = serde_json::to_value(&expected).unwrap();
        let check = |binding: &Campaign<'_>, value: &Value, identity: Option<&SupportingInputs>| {
            rejection_with_supporting(
                binding,
                value,
                &json!({}),
                |_| Ok(Some("e".repeat(64))),
                |_| Ok(false),
                None,
                identity,
            )
            .unwrap()
        };
        assert!(check(&campaign, &receipt, Some(&expected)).is_none());
        let missing_contract = Campaign {
            ordinary_contract: None,
            ..campaign
        };
        assert!(check(&missing_contract, &receipt, Some(&expected))
            .unwrap()
            .contains("campaign binding unavailable"));

        let mut older = receipt.clone();
        older["generated_at"] = json!("2026-01-01");
        for field in [
            "campaign_id",
            "manifest",
            "manifest_sha256",
            "source_sha256",
            "test_sha256",
        ] {
            let mut forged = receipt.clone();
            forged["generated_at"] = json!("2027-01-01");
            forged[field] = match field {
                "source_sha256" => json!({"src/a.rs":"0".repeat(64)}),
                "test_sha256" => json!({"tests/contract.rs":"0".repeat(64)}),
                _ => json!("wrong"),
            };
            assert!(
                check(&campaign, &forged, Some(&expected))
                    .unwrap()
                    .contains(field),
                "{field}"
            );
            let rows = vec![older.clone(), forged];
            let chosen = newest_acceptable(&rows, |row| Ok(check(&campaign, row, Some(&expected))))
                .unwrap()
                .unwrap();
            assert_eq!(chosen["generated_at"], "2026-01-01", "{field}");
        }
        let mut forged_detail = receipt.clone();
        forged_detail["detail"] =
            json!({"encoding":"json", "test_sha256":{"tests/contract.rs":"0".repeat(64)}});
        assert!(check(&campaign, &forged_detail, Some(&expected))
            .unwrap()
            .contains("test_sha256"));
        let rows = vec![older, forged_detail];
        let chosen = newest_acceptable(&rows, |row| Ok(check(&campaign, row, Some(&expected))))
            .unwrap()
            .unwrap();
        assert_eq!(chosen["generated_at"], "2026-01-01");
        let mut python_row = receipt.clone();
        python_row["runner_kind"] = json!("python");
        python_row["detail"] = json!({"encoding":"json", "runner_kind":"forge-native"});
        assert!(check(&campaign, &python_row, Some(&expected))
            .unwrap()
            .contains("changes runner identity"));
        assert!(check(&campaign, &receipt, None).is_some());
        let mut stale = receipt.clone();
        stale["supporting_inputs"]["digest"] = json!("b".repeat(64));
        assert!(check(&campaign, &stale, Some(&expected)).is_some());
        let mut legacy = receipt;
        legacy.as_object_mut().unwrap().remove("supporting_inputs");
        assert!(check(&campaign, &legacy, Some(&expected)).is_some());
    }

    #[test]
    fn workspace_profile_requires_its_own_independent_identity() {
        use crate::mutation_cargo_supporting_binding::SupportingInputs;
        let symbols = BTreeSet::new();
        let contract = json!({"campaign_id":"c","manifest":"campaigns/a.json","manifest_sha256":"f".repeat(64),
            "source_sha256":{"src/a.rs":"e".repeat(64)},"test_sha256":{"tests/a.rs":"b".repeat(64)},
            "execution_profile":"cargo-workspace-v2"});
        let campaign = Campaign {
            campaign_id: "c",
            generated: true,
            ordinary_cargo: true,
            ordinary_contract: Some(&contract),
            declares_value_analysis: false,
            source_symbols: &symbols,
        };
        let expected = SupportingInputs {
            schema_version: 2,
            algorithm: "sha256-framed-path-content-v1".into(),
            scope: "complete-snapshot-inputs-v2".into(),
            digest: "a".repeat(64),
            files: 4,
        };
        let mut receipt = native_receipt();
        for field in ["campaign_id", "manifest", "manifest_sha256", "test_sha256"] {
            receipt[field] = contract[field].clone();
        }
        receipt["supporting_inputs"] = serde_json::to_value(&expected).unwrap();
        let check = |value: &Value, binding: Option<&SupportingInputs>| {
            rejection_with_supporting(
                &campaign,
                value,
                &json!({}),
                |_| Ok(Some("e".repeat(64))),
                |_| Ok(false),
                None,
                binding,
            )
            .unwrap()
        };
        assert!(check(&receipt, Some(&expected)).is_none());
        assert!(check(&receipt, None).is_some());
        let mut forged = receipt.clone();
        forged["supporting_inputs"]["digest"] = json!("c".repeat(64));
        assert!(check(&forged, Some(&expected)).is_some());
        let root = SupportingInputs {
            schema_version: 1,
            scope: "cargo-root-rust-discovery-v1".into(),
            ..expected.clone()
        };
        let mut forged_scope = receipt;
        forged_scope["supporting_inputs"] = serde_json::to_value(&root).unwrap();
        assert!(check(&forged_scope, Some(&root)).is_some());
    }

    fn judged(receipt: &Value, hash: &str, lineage: bool) -> Option<String> {
        let symbols = std::collections::BTreeSet::new();
        let campaign = Campaign {
            campaign_id: "c",
            generated: true,
            ordinary_cargo: false,
            ordinary_contract: None,
            declares_value_analysis: false,
            source_symbols: &symbols,
        };
        rejection(
            &campaign,
            receipt,
            &json!({}),
            |_| Ok(Some(hash.to_owned())),
            |_| Ok(lineage),
        )
        .expect("rejection")
    }

    #[test]
    fn native_acceptance_requires_consistent_owned_cleanup() {
        let mut receipt = native_receipt();
        receipt
            .as_object_mut()
            .unwrap()
            .remove("execution_grants_sha256");
        assert!(judged(&receipt, &"e".repeat(64), true)
            .unwrap()
            .contains("historical"));
        let mut receipt = native_receipt();
        receipt["process_cleanup"]["commands_observed"] = json!(2);
        assert!(judged(&receipt, &"e".repeat(64), true)
            .unwrap()
            .contains("inconsistent"));
        let mut receipt = native_receipt();
        receipt["snapshot"]["removed"] = json!(false);
        assert!(judged(&receipt, &"e".repeat(64), true)
            .unwrap()
            .contains("inconsistent"));
    }

    #[test]
    fn installed_pin_is_only_used_when_source_fingerprint_is_unavailable() {
        let receipt = native_receipt();
        let symbols = BTreeSet::new();
        let c = campaign(&symbols);
        let pin = native_runner::InstalledIdentity {
            executable_sha256: "d".repeat(64),
            compiled_source_sha256: "e".repeat(64),
            compiled_git_commit: "f".repeat(40),
        };
        let check = |fingerprint: Option<String>| {
            rejection_with_installed(
                &c,
                &receipt,
                &json!({}),
                |path| {
                    Ok(if path == native_runner::FINGERPRINT_KEY {
                        fingerprint.clone()
                    } else {
                        Some("e".repeat(64))
                    })
                },
                |_| Ok(false),
                Some(&pin),
            )
            .unwrap()
        };
        assert!(check(None).is_none());
        assert!(check(Some("a".repeat(64))).is_some());
        assert!(
            rejection(&c, &receipt, &json!({}), |_| Ok(None), |_| Ok(false))
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn native_receipts_bind_to_this_tree_or_lineage() {
        let receipt = native_receipt();
        assert_eq!(judged(&receipt, &"e".repeat(64), false), None);
        assert_eq!(
            judged(&receipt, "other", false).as_deref(),
            Some("native runner source fingerprint is unavailable or differs from this tree")
        );
    }

    #[test]
    fn a_dirty_native_build_is_not_corpus_evidence() {
        let mut receipt = native_receipt();
        receipt["native_runner"]["source_matches_git"] = json!(false);
        assert_eq!(
            judged(&receipt, "c", true).as_deref(),
            Some("native_runner was built from a dirty source tree")
        );
    }

    #[test]
    fn a_non_passing_native_status_stops_before_identity() {
        let mut receipt = native_receipt();
        receipt["status"] = json!("CANCELLED");
        assert_eq!(
            judged(&receipt, "c", true).as_deref(),
            Some("status=CANCELLED")
        );
    }
}
