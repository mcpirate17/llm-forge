//! Frozen command-guard and write-target compatibility corpus.
//! Expected values were captured before the interpreted hooks were retired.
//! The native functions are tested directly, using the retained JSON fixtures
//! and `/repo` as the lexical root. No interpreter or legacy implementation runs.

#[path = "../src/bash_guard.rs"]
mod bash_guard;
#[path = "../src/write_targets.rs"]
mod write_targets;

use std::path::Path;

use serde::Deserialize;

#[derive(Deserialize)]
struct ExpectedEntry {
    command: String,
    blocked: bool,
    reason: Option<String>,
    write_targets: Vec<String>,
}

fn load_expected() -> Vec<ExpectedEntry> {
    let raw = include_str!("fixtures/guard_parity_expected.json");
    serde_json::from_str(raw).expect("guard_parity_expected.json must be valid JSON")
}

#[test]
fn corpus_has_at_least_forty_new_commands_beyond_the_python_test_suite() {
    let expected = load_expected();
    // 36 commands come from `test_bash_guard.py`'s 9 existing tests (with
    // duplicates collapsed); the rest are new, hand-written for this PR.
    assert!(
        expected.len() >= 76,
        "expected >= 36 existing + 40 new = 76 corpus entries, got {}",
        expected.len()
    );
}

#[test]
fn bash_guard_matches_python_on_every_corpus_command() {
    let expected = load_expected();
    let mut failures = Vec::new();
    for entry in &expected {
        let reason = bash_guard::check(&entry.command);
        let blocked = reason.is_some();
        if blocked != entry.blocked || reason != entry.reason {
            failures.push(format!(
                "command={:?}\n  expected: blocked={} reason={:?}\n  actual:   blocked={} reason={:?}",
                entry.command, entry.blocked, entry.reason, blocked, reason
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} guard mismatches:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn write_targets_match_python_on_every_corpus_command() {
    let expected = load_expected();
    let repo_root = Path::new("/repo");
    let mut failures = Vec::new();
    for entry in &expected {
        let targets = write_targets::repo_write_targets(&entry.command, repo_root);
        if targets != entry.write_targets {
            failures.push(format!(
                "command={:?}\n  expected: {:?}\n  actual:   {:?}",
                entry.command, entry.write_targets, targets
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} write-target mismatches:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
