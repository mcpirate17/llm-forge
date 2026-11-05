use super::*;
fn campaign<'a>(symbols: &'a BTreeSet<String>) -> Campaign<'a> {
    Campaign {
        campaign_id: "c",
        generated: false,
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

    fn judged(receipt: &Value, hash: &str, lineage: bool) -> Option<String> {
        let symbols = std::collections::BTreeSet::new();
        let campaign = Campaign {
            campaign_id: "c",
            generated: true,
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
