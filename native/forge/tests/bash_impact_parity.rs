//! Native impact classification against historical frozen expectations.
//! Commands are classified against `tests/fixtures/bash_impact_tree/`; they are
//! never executed. `{ROOT}` is replaced with the actual fixture path and then
//! normalized out of the result for comparison. The JSON corpus and expected
//! output remain test data after retirement of the interpreted implementation.

#[path = "../src/bash_impact.rs"]
mod bash_impact;

use std::path::PathBuf;

use serde::Deserialize;

#[derive(Deserialize)]
struct ExpectedEntry {
    command_template: String,
    tier: String,
    impact_template: String,
}

fn load_expected() -> Vec<ExpectedEntry> {
    let raw = include_str!("fixtures/bash_impact_expected.json");
    serde_json::from_str(raw).expect("bash_impact_expected.json must be valid JSON")
}

fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/bash_impact_tree")
}

#[test]
fn corpus_has_at_least_thirty_cases() {
    let expected = load_expected();
    assert!(
        expected.len() >= 30,
        "expected >= 30 corpus entries, got {}",
        expected.len()
    );
}

#[test]
fn corpus_covers_allow_and_soft_warn_verdicts() {
    let expected = load_expected();
    assert!(expected.iter().any(|e| e.tier == "allow"));
    assert!(expected.iter().any(|e| e.tier == "soft_warn"));
}

#[test]
fn classify_matches_python_on_every_corpus_command() {
    let expected = load_expected();
    let root = fixture_root();
    let root_str = root.to_str().expect("fixture root is valid utf-8");
    assert!(
        root.is_dir(),
        "fixture tree missing at {root_str}; did tests/fixtures/bash_impact_tree get committed?"
    );

    let mut failures = Vec::new();
    for entry in &expected {
        let command = entry.command_template.replace("{ROOT}", root_str);
        let (tier, impact_text) = bash_impact::classify(&command);
        let tier_str = match tier {
            bash_impact::Tier::Allow => "allow",
            bash_impact::Tier::SoftWarn => "soft_warn",
        };
        let expected_impact = entry.impact_template.replace("{ROOT}", root_str);
        if tier_str != entry.tier || impact_text != expected_impact {
            failures.push(format!(
                "command={:?}\n  expected: tier={} impact={:?}\n  actual:   tier={} impact={:?}",
                entry.command_template, entry.tier, expected_impact, tier_str, impact_text
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} bash_impact mismatches:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
