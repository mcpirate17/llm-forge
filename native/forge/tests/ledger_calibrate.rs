//! `forge ledger calibrate sample` integration tests (design step 3):
//! the sampler is deterministic under a seed, stratifies per session, caps
//! at `--per-session`, and never emits block text. The CLI tests exercise
//! the compiled binary (`CARGO_BIN_EXE_forge`); the frozen-fixture test
//! compiles the ledger module in via `#[path]`, the same pattern as
//! `tests/ledger_fixtures.rs` (this crate has no lib target).

#[path = "../src/json_canon.rs"]
#[allow(dead_code)]
mod json_canon;
#[path = "../src/ledger/mod.rs"]
#[allow(dead_code)]
// only calibrate's fixture parser is exercised here; the CLI tests below
// cover the sampler end to end through the binary itself.
mod ledger;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("forge-calibrate-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// Three transcripts (one of them mixing two sessions), enough turns per
/// session that a per-session cap actually bites: 12 per session, 4 total.
fn scenario() -> PathBuf {
    let dir = temp_dir("scenario");
    let usage = |input: u64, cache_read: u64, creation: u64| {
        format!(
            r#"{{"input_tokens":{input},"output_tokens":1,"cache_read_input_tokens":{cache_read},"cache_creation_input_tokens":{creation}}}"#
        )
    };
    let line = |uuid: &str, session: &str, usage: &str| {
        format!(
            r#"{{"uuid":"{uuid}","session_id":"{session}","timestamp":"2026-09-13T00:00:0{}Z","type":"assistant","message":{{"role":"assistant","model":"claude-sonnet-5","usage":{usage},"content":[{{"type":"text","text":"x"}}]}}}}"#,
            uuid.chars().last().unwrap(),
        )
    };
    let mut one = String::new();
    let mut two = String::new();
    let mut mixed = String::new();
    for i in 0..12 {
        one.push_str(&line(&format!("a{i}"), "sess-a", &usage(10 + i, 100, 7)));
        one.push('\n');
        two.push_str(&line(&format!("b{i}"), "sess-b", &usage(20, 0, 0)));
        two.push('\n');
        mixed.push_str(&line(&format!("c{i}"), "sess-c", &usage(30, 5, 0)));
        mixed.push('\n');
        mixed.push_str(&line(&format!("d{i}"), "sess-d", &usage(40, 0, 3)));
        mixed.push('\n');
    }
    fs::write(dir.join("one.jsonl"), one).unwrap();
    fs::write(dir.join("two.jsonl"), two).unwrap();
    fs::write(dir.join("mixed.jsonl"), mixed).unwrap();
    dir
}

fn run_sample(files: &[PathBuf], per_session: usize, seed: u64) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_forge"))
        .args(["ledger", "calibrate", "sample"])
        .arg("--per-session")
        .arg(per_session.to_string())
        .arg("--seed")
        .arg(seed.to_string())
        .args(files)
        .output()
        .expect("forge ledger calibrate sample runs");
    assert!(
        output.status.success(),
        "sample exited {:?}: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("stdout is UTF-8")
}

fn scenario_files(dir: &Path) -> Vec<PathBuf> {
    vec![
        dir.join("one.jsonl"),
        dir.join("two.jsonl"),
        dir.join("mixed.jsonl"),
    ]
}

#[test]
fn sample_is_deterministic_under_a_seed_and_covers_every_session() {
    let dir = scenario();
    let files = scenario_files(&dir);
    let first = run_sample(&files, 4, 42);
    let second = run_sample(&files, 4, 42);
    assert_eq!(first, second, "same seed must sample identically");
    assert_ne!(first, run_sample(&files, 4, 43), "a new seed reshuffles");

    let mut sessions = std::collections::BTreeMap::new();
    for line in first.lines() {
        let row: serde_json::Value = serde_json::from_str(line).unwrap();
        // The row carries shapes only, never block text or raw payloads.
        assert!(row.get("payloads").is_none());
        assert!(row.get("text").is_none());
        // Rows are keyed by declared session_id, so a mixed file's two
        // sessions stay distinguishable even though they share one file.
        *sessions
            .entry(row["session_id"].as_str().unwrap().to_string())
            .or_insert(0usize) += 1;
    }
    assert_eq!(
        sessions,
        std::collections::BTreeMap::from([
            ("sess-a".to_string(), 4),
            ("sess-b".to_string(), 4),
            ("sess-c".to_string(), 4),
            ("sess-d".to_string(), 4),
        ]),
        "every session is covered, capped at --per-session"
    );
    // A cap above the population samples everything: 4 sessions x 12 turns.
    let all = run_sample(&files, 50, 42);
    assert_eq!(all.lines().count(), 48);

    // billed_input arithmetic against the scenario's own usage numbers:
    // sess-a turn i bills (10+i) + 100 + 7 = 117+i, so every sampled sess-a
    // row bills >= 117 -- and exactly one sampled row bills each value, so
    // the picks are the scenario's real turns, not recomputed approximations.
    let mut billed: Vec<u64> = first
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|row| row["session_id"] == "sess-a")
        .map(|row| row["billed_input"].as_u64().unwrap())
        .collect();
    billed.sort_unstable();
    assert_eq!(billed.len(), 4);
    assert!(billed.iter().all(|b| (117..129).contains(b)));
    billed.dedup();
    assert_eq!(billed.len(), 4, "four distinct turns, no repeats");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn billed_input_is_the_sum_of_all_three_input_streams() {
    let dir = temp_dir("billed");
    // One turn with distinct values in all three streams: 11 + 700 + 13 = 724.
    fs::write(
        dir.join("t.jsonl"),
        r#"{"uuid":"z1","session_id":"sess-z","timestamp":"2026-09-13T00:00:00Z","type":"assistant","message":{"role":"assistant","model":"m","usage":{"input_tokens":11,"output_tokens":9,"cache_read_input_tokens":700,"cache_creation_input_tokens":13},"content":[{"type":"text","text":"x"}]}}"#
            .to_string()
            + "\n",
    )
    .unwrap();
    let files = [dir.join("t.jsonl")];
    let out = run_sample(&files, 10, 0);
    let row: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!(row["billed_input"].as_u64().unwrap(), 724);
    assert_eq!(row["turn_uuid"], "z1");
    assert_eq!(row["turn_index"], 0);
    let _ = fs::remove_dir_all(&dir);
}

/// The committed fixture is frozen: this test fails the moment its shape
/// changes, so an edit to it is a visible, deliberate act. It currently
/// holds the offline debt shape (no API key on the measuring machine), which
/// is exactly what it asserts -- paying that debt flips this test in the
/// same PR that measures the real bound.
#[test]
fn the_committed_calibration_fixture_is_frozen_and_honestly_null() {
    let text = fs::read_to_string("tests/fixtures/ledger/calibration.json")
        .expect("committed calibration fixture");
    assert!(
        ledger::calibrate::parse_calibration_fixture(&text)
            .expect("fixture must parse")
            .is_none(),
        "per_block_type is null until the count_tokens bound is measured"
    );
    let value: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert!(
        value["n_turns"].as_u64().unwrap() > 0,
        "a real sample, not a placeholder"
    );
    assert!(value["chars_per_token"]["median"].as_f64().unwrap() > 0.0);
    assert!(
        value["model"].as_str().is_some(),
        "the offline run still records which model the turns ran on"
    );
}
