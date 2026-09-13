//! `forge ledger read` fixture tests (design step 1,
//! `docs/design/cost_ledger.md` section 6): frozen-JSON parity for four
//! synthetic transcript shapes and one telemetry shape, plus a direct check
//! of the "no block text past the reader boundary" hard rule.
//!
//! This binary crate has no lib target (see `tests/guard_parity.rs`), so the
//! ledger module is compiled in via `#[path]` rather than an external
//! `extern crate forge` import.

#[path = "../src/ledger/mod.rs"]
#[allow(dead_code)]
// only reader/schema are exercised here; run()/CLI printing are covered by the `forge` binary itself.
mod ledger;

use std::fs;
use std::path::Path;

fn fixture(name: &str) -> String {
    format!("tests/fixtures/ledger/{name}")
}

fn assert_matches_frozen(actual_json: String, expected_file: &str) {
    let expected = fs::read_to_string(fixture(expected_file))
        .unwrap_or_else(|err| panic!("reading {expected_file}: {err}"));
    assert_eq!(actual_json, expected.trim_end(), "{expected_file}");
}

#[test]
fn wellformed_transcript_matches_frozen_summary() {
    let summary =
        ledger::reader::read_transcript_file(Path::new(&fixture("transcript_wellformed.jsonl")))
            .expect("well-formed fixture parses");
    assert_eq!(summary.turns_with_usage, 6);
    assert_eq!(summary.read_stats.skipped_lines.len(), 0);
    assert_matches_frozen(
        serde_json::to_string(&summary).unwrap(),
        "expected_transcript_wellformed.json",
    );
}

#[test]
fn truncated_last_line_is_skipped_not_panicked() {
    let summary =
        ledger::reader::read_transcript_file(Path::new(&fixture("transcript_truncated.jsonl")))
            .expect("truncated fixture still returns a summary");
    assert_eq!(summary.read_stats.skipped_lines.len(), 1);
    assert_eq!(summary.read_stats.skipped_lines[0].line_number, 3);
    assert_matches_frozen(
        serde_json::to_string(&summary).unwrap(),
        "expected_transcript_truncated.json",
    );
}

#[test]
fn unknown_block_type_counts_as_other() {
    let summary =
        ledger::reader::read_transcript_file(Path::new(&fixture("transcript_unknown_block.jsonl")))
            .expect("unknown-block fixture parses");
    assert_eq!(summary.chars_by_block_type.other, 58);
    assert_eq!(summary.read_stats.skipped_lines.len(), 0);
    assert_matches_frozen(
        serde_json::to_string(&summary).unwrap(),
        "expected_transcript_unknown_block.json",
    );
}

#[test]
fn non_json_line_is_skipped_not_panicked() {
    let summary =
        ledger::reader::read_transcript_file(Path::new(&fixture("transcript_non_json_line.jsonl")))
            .expect("non-JSON-line fixture still returns a summary");
    assert_eq!(summary.read_stats.skipped_lines.len(), 1);
    assert_eq!(summary.read_stats.skipped_lines[0].line_number, 2);
    assert_matches_frozen(
        serde_json::to_string(&summary).unwrap(),
        "expected_transcript_non_json_line.json",
    );
}

/// Regression for the `ToolResult.char_len` fix: real transcripts store
/// `tool_result.content` overwhelmingly as a plain string (not the array-of-
/// text-blocks shape), and an array-form `content` can carry a non-text
/// sub-block (e.g. `image`) that must contribute 0 chars, not be
/// stringified and counted as if it were text.
#[test]
fn tool_result_string_and_mixed_array_content_are_measured_correctly() {
    let summary = ledger::reader::read_transcript_file(Path::new(&fixture(
        "transcript_tool_result_mixed.jsonl",
    )))
    .expect("mixed tool_result fixture parses");
    // "running the string-form tool result, forty-two chars" (52 chars)
    // + "array-form text sub-block" (25 chars) from the array-form block's
    // text sub-block; the array-form block's image sub-block contributes 0.
    assert_eq!(summary.chars_by_block_type.tool_result, 77);
    assert_matches_frozen(
        serde_json::to_string(&summary).unwrap(),
        "expected_transcript_tool_result_mixed.json",
    );
}

/// A subagent transcript (`agent-*.jsonl`, PR #45): every line carries the
/// same 17-hex `agentId` alongside its PARENT's uuid as `sessionId`. The
/// reader settles the file's identity from the first `agentId` seen and
/// reports the parent's uuid in `parent_session_id` -- while each turn's own
/// `session_id` field stays the raw per-line value (the `agent-<id>` keying
/// rule is rollup's, not the reader's; a reader that rewrote it would make
/// `forge ledger read` lie about what the file actually says).
#[test]
fn subagent_transcript_reports_identity_fields() {
    let summary =
        ledger::reader::read_transcript_file(Path::new(&fixture("transcript_subagent.jsonl")))
            .expect("subagent fixture parses");
    assert_eq!(summary.agent_id.as_deref(), Some("7fa9c1b0123456789"));
    assert_eq!(
        summary.parent_session_id.as_deref(),
        Some("5a1f0c33-90d1-4a2f-b1e1-7c2d3e4f5a6b")
    );
    assert!(summary.is_subagent);
    // Raw per-line session ids, un-rewritten: the parent's uuid.
    assert_eq!(
        summary.turns[0].session_id.as_deref(),
        Some("5a1f0c33-90d1-4a2f-b1e1-7c2d3e4f5a6b")
    );
    assert_matches_frozen(
        serde_json::to_string(&summary).unwrap(),
        "expected_transcript_subagent.json",
    );
}

#[test]
fn telemetry_file_matches_frozen_summary() {
    let summary =
        ledger::reader::read_telemetry_file(Path::new(&fixture("telemetry_sample.jsonl")))
            .expect("telemetry fixture parses");
    assert_eq!(summary.hooks.len(), 3);
    assert_matches_frozen(
        serde_json::to_string(&summary).unwrap(),
        "expected_telemetry_sample.json",
    );
}

/// Hard rule: "the reader keeps `char_len` for text and tool_result blocks
/// and never retains block text past the reader boundary." Writes a
/// transcript line whose text carries a long, singular marker and asserts
/// the marker is nowhere in the parsed `TranscriptSummary` -- not in its
/// serialized JSON, not in its `Debug` output -- only the char count is.
#[test]
fn reader_never_retains_block_text() {
    let marker = "MARKER-DO-NOT-RETAIN-3f9b2c7e1a";
    let line = format!(
        r#"{{"uuid":"m1","session_id":"sess-m","timestamp":"2026-09-13T05:00:00.000+00:00","message":{{"role":"assistant","model":"claude-sonnet-5","usage":{{"input_tokens":1,"output_tokens":1,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}},"content":[{{"type":"text","text":"{marker}"}},{{"type":"tool_result","tool_use_id":"tu-m","content":"{marker}"}}]}}}}"#
    );
    let dir = std::env::temp_dir().join(format!("forge-ledger-no-retain-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("line.jsonl");
    fs::write(&path, format!("{line}\n")).unwrap();

    let summary = ledger::reader::read_transcript_file(&path).expect("single line parses");
    let serialized = serde_json::to_string(&summary).unwrap();
    let debugged = format!("{summary:?}");
    assert!(
        !serialized.contains(marker),
        "marker text leaked into serialized TranscriptSummary"
    );
    assert!(
        !debugged.contains(marker),
        "marker text leaked into Debug-formatted TranscriptSummary"
    );
    // The char count is what should survive instead of the text itself.
    assert_eq!(
        summary.turns[0].bytes_by_block_type.text,
        marker.len() as u64
    );
    assert_eq!(
        summary.turns[0].bytes_by_block_type.tool_result,
        marker.len() as u64
    );

    let _ = fs::remove_dir_all(&dir);
}
