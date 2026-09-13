//! `forge ledger rollup` fixture tests (design step 2,
//! `docs/design/cost_ledger.md` section 6 step 2): frozen-JSON parity for a
//! synthetic 3-session scenario (a compaction, a resend pattern, and a
//! subagent transcript) plus one telemetry file, and an idempotence check
//! that writing the same input twice does not duplicate or perturb rows.
//!
//! Same `#[path]`-inclusion pattern as `tests/ledger_fixtures.rs`: this
//! binary crate has no lib target, so `extern crate forge` is not an
//! option here either.

#[path = "../src/ledger/mod.rs"]
#[allow(dead_code)]
mod ledger;

use ledger::rollup::{run, today_utc_date, RollupArgs};
use std::fs;
use std::path::{Path, PathBuf};

fn scenario_dir() -> PathBuf {
    PathBuf::from("tests/fixtures/ledger/rollup_scenario")
}

fn expected_dry_run() -> String {
    fs::read_to_string("tests/fixtures/ledger/expected_rollup_scenario_dry_run.jsonl")
        .expect("reading expected_rollup_scenario_dry_run.jsonl")
}

/// Hand-computed expected rows (see the fixture files under
/// `rollup_scenario/` and their arithmetic in code review / the PR body):
/// `sess-agent-1` (1 turn, no compaction, no resend), `sess-compact-1` (2
/// turns, one `isCompactSummary` marker between them), `sess-resend-1` (2
/// turns, the second re-pays `cache_creation_input_tokens` without its own
/// byte total growing past the first's).
#[test]
fn dry_run_matches_frozen_scenario() {
    let exe = env!("CARGO_BIN_EXE_forge");
    let output = std::process::Command::new(exe)
        .args(["ledger", "rollup", "--dry-run"])
        .arg(scenario_dir())
        .output()
        .expect("forge ledger rollup --dry-run runs");
    assert!(
        output.status.success(),
        "forge ledger rollup --dry-run exited {:?}: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    let actual = String::from_utf8(output.stdout).expect("stdout is valid UTF-8");
    assert_eq!(actual.trim_end(), expected_dry_run().trim_end());
}

fn read_day_file(root: &Path, table: &str, day: &str) -> String {
    fs::read_to_string(root.join(table).join(format!("{day}.jsonl")))
        .unwrap_or_else(|err| panic!("reading {table}/{day}.jsonl: {err}"))
}

/// The real exit point (design step 2 item 2, "compaction and resend
/// detection" written to storage): run the rollup for real into a temp
/// ledger root and check every table's day file against hand-computed
/// frozen content, byte-for-byte.
#[test]
fn write_matches_hand_computed_tables() {
    let root =
        std::env::temp_dir().join(format!("forge-ledger-rollup-write-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);

    let args = RollupArgs {
        paths: vec![scenario_dir()],
        out: Some(root.clone()),
        dry_run: false,
    };
    let code = run(args).expect("rollup run succeeds");
    assert_eq!(code, 0);

    // Field order below is `serde_json::Value`'s (alphabetical -- the
    // writer round-trips every row through `serde_json::to_value` so it
    // can filter/rewrite by key, `writer.rs`), not `TurnAttributionRow`'s
    // declaration order; both carry the same hand-computed numbers.
    let turn_2026_01_01 = read_day_file(&root, "turn_attribution", "2026-01-01");
    assert_eq!(
        turn_2026_01_01.trim_end(),
        "{\"bytes_by_block_type\":{\"image\":0,\"other\":0,\"text\":10,\"thinking\":0,\"tool_result\":0,\"tool_use\":0},\"cache_creation_input_tokens\":0,\"cache_read_input_tokens\":50,\"estimate_method\":\"byte_proportional_uncalibrated_cpt4\",\"estimated_tokens_by_block_type\":{\"image\":0.0,\"other\":0.0,\"text\":150.0,\"tool_result\":0.0,\"tool_use\":0.0},\"input_tokens\":100,\"model\":\"claude-sonnet-5\",\"output_tokens\":20,\"session_id\":\"sess-compact-1\",\"timestamp\":\"2026-01-01T00:00:00Z\",\"turn_index\":0,\"turn_uuid\":\"t1\"}\n\
{\"bytes_by_block_type\":{\"image\":0,\"other\":0,\"text\":5,\"thinking\":0,\"tool_result\":0,\"tool_use\":0},\"cache_creation_input_tokens\":0,\"cache_read_input_tokens\":0,\"estimate_method\":\"byte_proportional_uncalibrated_cpt4\",\"estimated_tokens_by_block_type\":{\"image\":0.0,\"other\":0.0,\"text\":40.0,\"tool_result\":0.0,\"tool_use\":0.0},\"input_tokens\":40,\"model\":\"claude-sonnet-5\",\"output_tokens\":10,\"session_id\":\"sess-compact-1\",\"timestamp\":\"2026-01-01T00:10:00Z\",\"turn_index\":1,\"turn_uuid\":\"t2\"}"
    );

    let turn_2026_02_01 = read_day_file(&root, "turn_attribution", "2026-02-01");
    assert_eq!(
        turn_2026_02_01.trim_end(),
        "{\"bytes_by_block_type\":{\"image\":0,\"other\":0,\"text\":0,\"thinking\":0,\"tool_result\":10,\"tool_use\":0},\"cache_creation_input_tokens\":100,\"cache_read_input_tokens\":0,\"estimate_method\":\"byte_proportional_uncalibrated_cpt4\",\"estimated_tokens_by_block_type\":{\"image\":0.0,\"other\":0.0,\"text\":0.0,\"tool_result\":300.0,\"tool_use\":0.0},\"input_tokens\":200,\"model\":\"claude-sonnet-5\",\"output_tokens\":30,\"session_id\":\"sess-resend-1\",\"timestamp\":\"2026-02-01T00:00:00Z\",\"turn_index\":0,\"turn_uuid\":\"r1\"}\n\
{\"bytes_by_block_type\":{\"image\":0,\"other\":0,\"text\":0,\"thinking\":0,\"tool_result\":5,\"tool_use\":0},\"cache_creation_input_tokens\":80,\"cache_read_input_tokens\":0,\"estimate_method\":\"byte_proportional_uncalibrated_cpt4\",\"estimated_tokens_by_block_type\":{\"image\":0.0,\"other\":0.0,\"text\":0.0,\"tool_result\":130.0,\"tool_use\":0.0},\"input_tokens\":50,\"model\":\"claude-sonnet-5\",\"output_tokens\":5,\"session_id\":\"sess-resend-1\",\"timestamp\":\"2026-02-01T00:05:00Z\",\"turn_index\":1,\"turn_uuid\":\"r2\"}"
    );

    let turn_2026_03_01 = read_day_file(&root, "turn_attribution", "2026-03-01");
    assert_eq!(
        turn_2026_03_01.trim_end(),
        "{\"bytes_by_block_type\":{\"image\":0,\"other\":0,\"text\":11,\"thinking\":0,\"tool_result\":0,\"tool_use\":0},\"cache_creation_input_tokens\":0,\"cache_read_input_tokens\":0,\"estimate_method\":\"byte_proportional_uncalibrated_cpt4\",\"estimated_tokens_by_block_type\":{\"image\":0.0,\"other\":0.0,\"text\":10.0,\"tool_result\":0.0,\"tool_use\":0.0},\"input_tokens\":10,\"model\":\"claude-sonnet-5\",\"output_tokens\":2,\"session_id\":\"sess-agent-1\",\"timestamp\":\"2026-03-01T00:00:00Z\",\"turn_index\":0,\"turn_uuid\":\"a1\"}"
    );

    let session_2026_01_01 = read_day_file(&root, "session_rollup", "2026-01-01");
    assert_eq!(
        session_2026_01_01.trim_end(),
        "{\"first_ts\":\"2026-01-01T00:00:00Z\",\"last_ts\":\"2026-01-01T00:10:00Z\",\"n_compactions\":1,\"n_turns\":2,\"project\":\"rollup_scenario\",\"resend_bytes\":0,\"resend_events\":0,\"session_id\":\"sess-compact-1\",\"total_cache_creation\":0,\"total_cache_read\":50,\"total_input\":140,\"total_output\":30}"
    );

    let session_2026_02_01 = read_day_file(&root, "session_rollup", "2026-02-01");
    assert_eq!(
        session_2026_02_01.trim_end(),
        "{\"first_ts\":\"2026-02-01T00:00:00Z\",\"last_ts\":\"2026-02-01T00:05:00Z\",\"n_compactions\":0,\"n_turns\":2,\"project\":\"rollup_scenario\",\"resend_bytes\":320,\"resend_events\":1,\"session_id\":\"sess-resend-1\",\"total_cache_creation\":180,\"total_cache_read\":0,\"total_input\":250,\"total_output\":35}"
    );

    let hook_today = read_day_file(&root, "hook_rollup", &today_utc_date());
    assert_eq!(
        hook_today.trim_end(),
        "{\"event\":\"HookTiming\",\"hook_name\":\"delegate_check\",\"n_calls\":2,\"p50_ms\":5.0,\"p90_ms\":15.0,\"total_output_bytes\":100}"
    );

    let _ = fs::remove_dir_all(&root);
}

/// Idempotence (design step 2 item 2, "replaces that session's rows for
/// that day"): rerunning the rollup over the same input must not duplicate
/// rows or otherwise perturb the day files -- every table's bytes must be
/// identical to the first run's.
#[test]
fn rerunning_rollup_is_idempotent() {
    let root = std::env::temp_dir().join(format!(
        "forge-ledger-rollup-idempotent-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);

    let make_args = || RollupArgs {
        paths: vec![scenario_dir()],
        out: Some(root.clone()),
        dry_run: false,
    };

    run(make_args()).expect("first rollup run succeeds");
    let first: Vec<(String, String)> = [
        ("turn_attribution", "2026-01-01"),
        ("turn_attribution", "2026-02-01"),
        ("turn_attribution", "2026-03-01"),
        ("session_rollup", "2026-01-01"),
        ("session_rollup", "2026-02-01"),
    ]
    .into_iter()
    .map(|(table, day)| (format!("{table}/{day}"), read_day_file(&root, table, day)))
    .collect();
    let hook_day = today_utc_date();
    let first_hook = read_day_file(&root, "hook_rollup", &hook_day);

    run(make_args()).expect("second rollup run succeeds");
    for (label, content) in &first {
        let (table, day) = label.split_once('/').unwrap();
        assert_eq!(
            read_day_file(&root, table, day),
            *content,
            "{label} changed on rerun"
        );
    }
    assert_eq!(
        read_day_file(&root, "hook_rollup", &hook_day),
        first_hook,
        "hook_rollup changed on rerun"
    );

    let _ = fs::remove_dir_all(&root);
}
