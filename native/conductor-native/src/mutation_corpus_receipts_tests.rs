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
