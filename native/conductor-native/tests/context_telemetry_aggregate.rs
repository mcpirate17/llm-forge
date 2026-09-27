//! Pure Rust telemetry contracts. These run with --no-default-features.

use conductor_native::context_telemetry_aggregate::{
    format_rich_summary, format_summary, parse_since, summarize, summarize_report,
};
use serde_json::json;
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

struct Case(PathBuf);

impl Case {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "telemetry-aggregate-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn log(&self, lines: &[serde_json::Value]) -> String {
        let path = self.0.join("events.jsonl");
        let contents = lines
            .iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(&path, format!("{contents}\ninvalid JSON\n")).unwrap();
        path.to_string_lossy().into_owned()
    }
}

impl Drop for Case {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn base_summary_keeps_first_seen_ties_and_exact_bound_excess() {
    let case = Case::new();
    let path = case.log(&[
        json!({"event":"PostToolUse","tool":"B","output_bytes":12,"output_tokens_estimate":3}),
        json!({"event":"HookContext","tool":"A","output_bytes":12,"output_tokens_estimate":3}),
    ]);
    let result = summarize(&[path], 10).unwrap();
    assert_eq!(result["events"], 2);
    assert_eq!(result["hook_context_bytes"], 12);
    assert_eq!(result["rows"][0]["tool"], "B");
    assert_eq!(result["rows"][0]["share"], 0.5);
    assert_eq!(result["rows"][0]["over_bound_bytes"], 2);
    let text = format_summary(&result).unwrap();
    assert!(text.starts_with("events=2 output_bytes=24 hook_context_bytes=12 bound=10\n"));
    assert!(text.lines().nth(2).unwrap().contains("50.0%"));
}

#[test]
fn rich_report_filters_all_sections_and_keeps_template_ties() {
    let case = Case::new();
    let path = case.log(&[
        json!({"timestamp":"2026-09-27T10:00:00+00:00","event":"HookContext",
            "tool":"old","hook_event":"SessionStart","output_bytes":100,
            "category":"instructions","content_hash":"old"}),
        json!({"timestamp":"2026-09-27T11:45:00+00:00","event":"HookContext",
            "tool":"first","hook_event":"SessionStart","output_bytes":20,
            "category":"instructions","content_hash":"same","session_id":"s1"}),
        json!({"timestamp":"2026-09-27T11:46:00+00:00","event":"HookContext",
            "tool":"second","hook_event":"SessionStart","output_bytes":20,
            "category":"instructions","content_hash":"same","session_id":"s1"}),
        json!({"timestamp":"2026-09-27T11:47:00+00:00","event":"HookTiming",
            "tool":"first","output_bytes":0,"elapsed_ms":10.0}),
        json!({"timestamp":"2026-09-27T11:48:00+00:00","event":"HookTiming",
            "tool":"first","output_bytes":0,"elapsed_ms":30.0}),
    ]);
    let result = summarize_report(
        &[path],
        15,
        Some("30m"),
        2,
        "2026-09-27T12:00:00+00:00",
        "/tmp/historical.jsonl",
    )
    .unwrap();
    assert_eq!(result["files"], json!(["/tmp/historical.jsonl"]));
    assert_eq!(result["events"], 4);
    assert_eq!(result["rows"][0]["tool"], "first");
    assert_eq!(result["sessions"]["s1"]["output_bytes"], 40);
    assert_eq!(result["sessions"]["unknown"]["events"], 2);
    assert_eq!(result["hook_ms"]["by_hook"]["first"]["p50_ms"], 20.0);
    assert_eq!(result["hook_ms"]["by_hook"]["first"]["p90_ms"], 28.0);
    assert_eq!(result["instructions"]["repeat_resends"], 1);
    assert_eq!(result["top_message_templates"][0]["hook"], "first");
    assert_eq!(result["top_message_templates"][1]["hook"], "second");
    let text = format_rich_summary(&result).unwrap();
    assert!(text.contains("since=30m\nsessions=2\n"));
    assert!(text.contains("hook_ms total=40.0"));
    assert!(text.ends_with("count=    1 bytes=        20"));
}

#[test]
fn missing_rich_paths_are_skipped_and_negative_top_matches_python_slice() {
    let case = Case::new();
    let path = case.log(&[
        json!({"event":"HookContext","tool":"a","hook_event":"PreToolUse","output_bytes":3}),
        json!({"event":"HookContext","tool":"b","hook_event":"PreToolUse","output_bytes":2}),
        json!({"event":"HookContext","tool":"c","hook_event":"PreToolUse","output_bytes":1}),
    ]);
    let missing = case.0.join("absent.jsonl").to_string_lossy().into_owned();
    assert!(summarize(std::slice::from_ref(&missing), 8).is_err());
    let report = summarize_report(
        &[missing, path],
        8,
        None,
        -1,
        "2026-09-27T12:00:00Z",
        "/tmp/label",
    )
    .unwrap();
    assert_eq!(report["events"], 3);
    assert_eq!(report["top_message_templates"].as_array().unwrap().len(), 2);
    assert_eq!(report["top_message_templates"][1]["hook"], "b");
}

#[test]
fn duration_validation_and_naive_timestamp_are_errors() {
    assert!(parse_since("nonsense", "2026-09-27T12:00:00Z")
        .unwrap_err()
        .to_string()
        .contains("--since must look like"));
    assert_eq!(
        parse_since(" 30m ", "2026-09-27T12:00:00Z").unwrap(),
        "2026-09-27T11:30:00+00:00"
    );
    let case = Case::new();
    let path = case.log(&[json!({"timestamp":"2026-09-27T11:55:00",
        "event":"HookContext","tool":"a","output_bytes":1})]);
    assert!(summarize_report(
        &[path],
        8,
        Some("30m"),
        10,
        "2026-09-27T12:00:00Z",
        "/tmp/label"
    )
    .unwrap_err()
    .to_string()
    .contains("offset-naive"));
}
