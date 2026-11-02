//! Independent executed CPython3.12/Radon6.0.1 oracle; no interpreter at test time.
use super::{analyze_with_options, Options, PROTOCOL};
use serde_json::{json, Value};
use std::collections::BTreeSet;

#[test]
fn frozen_reference_edges_match_full_blocks_module_score_and_both_assertion_options() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../tests/fixtures/radon_complexity/edge-reference-6.0.1.json"
    ))
    .unwrap();
    assert_eq!(fixture["provenance"]["protocol"], PROTOCOL);
    let cases = fixture["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 50);
    let mut seen = BTreeSet::new();
    for case in cases {
        let name = case["case"].as_str().unwrap();
        let no_assert = case["no_assert"].as_bool().unwrap();
        assert!(
            seen.insert((name, no_assert)),
            "duplicate {name}/{no_assert}"
        );
        let result = analyze_with_options(
            case["source"].as_str().unwrap(),
            case["path"].as_str().unwrap(),
            Options { no_assert },
        );
        let expected = &case["expected"];
        if expected.get("reference_error").is_some() {
            assert!(
                result.is_err(),
                "{name}, no_assert={no_assert}: expected refusal"
            );
            continue;
        }
        let actual =
            result.unwrap_or_else(|error| panic!("{name}, no_assert={no_assert}: {error}"));
        assert_eq!(
            json!(actual.module_complexity),
            expected["module_complexity"],
            "{name}, no_assert={no_assert}"
        );
        assert_eq!(
            serde_json::to_value(actual.blocks).unwrap(),
            expected["blocks"],
            "{name}, no_assert={no_assert}"
        );
    }
    assert_eq!(
        seen.into_iter()
            .map(|(name, _)| name)
            .collect::<BTreeSet<_>>()
            .len(),
        25
    );
}
