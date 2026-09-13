//! Streaming readers for the cost ledger's two input kinds (design section 2):
//! harness transcript JSONL and hook telemetry JSONL. Both stream with
//! `BufRead::lines` -- never `read_to_string` -- because transcripts reach
//! 100 MB (design "Measured this session" table).

use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::Path;

use anyhow::{Context, Result};
use serde_json::Value;

use super::schema::{
    BlockTypeCounts, CompactionMarker, ContentBlock, HookStats, InputKind, ReadStats, SkippedLine,
    TelemetrySummary, TranscriptLine, TranscriptSummary, TurnSummary,
};
use super::session_ids::find_session_ids;

const SYSTEM_REMINDER_PREFIX: &str = "<system-reminder>";

/// Look at the first several parseable lines of `path` and decide which
/// input kind it is: a transcript line always carries `uuid`; every
/// telemetry shape this reader knows (`DelegationEvent`, `HookTiming`,
/// `HookContext`) carries a top-level `event` string and no `uuid`. Fails
/// loud (asks for `--kind`) rather than guessing past the first handful of
/// lines.
pub fn detect_kind(path: &Path) -> Result<InputKind> {
    let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let reader = BufReader::new(file);
    for line in reader.lines().take(20) {
        let Ok(line) = line else { continue };
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let Some(obj) = value.as_object() else {
            continue;
        };
        if obj.contains_key("uuid") {
            return Ok(InputKind::Transcript);
        }
        if obj.contains_key("event") {
            return Ok(InputKind::Telemetry);
        }
    }
    anyhow::bail!(
        "{}: could not detect input kind from the first 20 lines; pass --kind",
        path.display()
    )
}

/// Parses one transcript JSONL file into a `TranscriptSummary`, never
/// panicking on a malformed or truncated line -- it counts the line in
/// `skipped_lines` instead.
pub fn read_transcript_file(path: &Path) -> Result<TranscriptSummary> {
    let bytes = fs::metadata(path)
        .with_context(|| format!("stat {}", path.display()))?
        .len();
    let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let reader = BufReader::new(file);

    let mut summary = TranscriptSummary {
        path: path.display().to_string(),
        bytes,
        lines: 0,
        turns_with_usage: 0,
        total_input_tokens: 0,
        total_output_tokens: 0,
        total_cache_read_input_tokens: 0,
        total_cache_creation_input_tokens: 0,
        chars_by_block_type: BlockTypeCounts::default(),
        read_stats: ReadStats::default(),
        turns: Vec::new(),
        compaction_markers: Vec::new(),
        harness_session_ids: Vec::new(),
    };
    let mut harness_session_ids: BTreeSet<String> = BTreeSet::new();

    for (idx, line) in reader.lines().enumerate() {
        let line_number = idx + 1;
        summary.lines += 1;
        let Ok(text) = line else {
            // A read error this late (not the initial `File::open`) is the
            // truncated-final-line case on some filesystems: report it the
            // same way as any other unparseable line, never panic.
            summary.read_stats.skipped_lines.push(SkippedLine {
                line_number,
                reason: "read_error".to_string(),
            });
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(&text) else {
            summary.read_stats.skipped_lines.push(SkippedLine {
                line_number,
                reason: "invalid_json".to_string(),
            });
            continue;
        };
        if value.get("uuid").is_none() {
            summary.read_stats.skipped_lines.push(SkippedLine {
                line_number,
                reason: "missing_uuid".to_string(),
            });
            continue;
        }
        // Read the harness's own camelCase key before `value` is consumed
        // below: real transcripts (`docs/design/cost_ledger.md` section 2)
        // carry `sessionId`, and a subagent transcript (`agent-*.jsonl`)
        // never gets the snake_case `session_id` duplicate this repo's
        // tooling stamps onto main-session lines -- see `TranscriptLine`'s
        // doc comment for why this is a fallback fill-in rather than a
        // `serde(alias)`.
        let session_id_fallback = value
            .get("sessionId")
            .and_then(Value::as_str)
            .map(str::to_string);
        let Ok(mut parsed) = serde_json::from_value::<TranscriptLine>(value) else {
            summary.read_stats.skipped_lines.push(SkippedLine {
                line_number,
                reason: "schema_mismatch".to_string(),
            });
            continue;
        };
        if parsed.session_id.is_none() {
            parsed.session_id = session_id_fallback;
        }
        accumulate_transcript_line(&mut summary, parsed, &mut harness_session_ids);
    }

    summary.harness_session_ids = harness_session_ids.into_iter().collect();
    Ok(summary)
}

fn accumulate_transcript_line(
    summary: &mut TranscriptSummary,
    line: TranscriptLine,
    harness_session_ids: &mut BTreeSet<String>,
) {
    if line.is_compact_summary {
        summary.compaction_markers.push(CompactionMarker {
            session_id: line.session_id.clone(),
            timestamp: line.timestamp.clone(),
        });
    }
    let Some(message) = line.message else {
        return;
    };
    let blocks = parse_content_blocks(&message.content, harness_session_ids);
    for block in &blocks {
        summary.chars_by_block_type.add(block);
    }
    // `turn_attribution` (design section 2) is one row per *assistant*
    // usage-bearing message; a `usage` block only ever appears on those,
    // but a malformed line could carry one elsewhere, so check both.
    let is_assistant_turn = message.role.as_deref() == Some("assistant");
    let Some(usage) = message.usage.filter(|_| is_assistant_turn) else {
        return;
    };
    let mut turn_bytes = BlockTypeCounts::default();
    for block in &blocks {
        turn_bytes.add(block);
    }
    summary.turns_with_usage += 1;
    summary.total_input_tokens += usage.input_tokens;
    summary.total_output_tokens += usage.output_tokens;
    summary.total_cache_read_input_tokens += usage.cache_read_input_tokens;
    summary.total_cache_creation_input_tokens += usage.cache_creation_input_tokens;
    summary.turns.push(TurnSummary {
        session_id: line.session_id,
        turn_index: summary.turns_with_usage as usize - 1,
        turn_uuid: line.uuid,
        timestamp: line.timestamp,
        model: message.model,
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        cache_read_input_tokens: usage.cache_read_input_tokens,
        cache_creation_input_tokens: usage.cache_creation_input_tokens,
        bytes_by_block_type: turn_bytes,
    });
}

/// `message.content` is either a plain string (short user turns) or an
/// array of typed blocks (assistant turns, and most tool-result-bearing
/// user turns). Handles both without panicking; anything else (missing,
/// null, an unexpected shape) yields no blocks.
fn parse_content_blocks(
    content: &Value,
    harness_session_ids: &mut BTreeSet<String>,
) -> Vec<ContentBlock> {
    match content {
        Value::String(text) => vec![text_block(text, harness_session_ids)],
        Value::Array(items) => items
            .iter()
            .map(|item| parse_one_block(item, harness_session_ids))
            .collect(),
        _ => Vec::new(),
    }
}

fn text_block(text: &str, harness_session_ids: &mut BTreeSet<String>) -> ContentBlock {
    find_session_ids(text, harness_session_ids);
    ContentBlock::Text {
        char_len: text.chars().count(),
        is_system_reminder: text.starts_with(SYSTEM_REMINDER_PREFIX),
    }
}

fn parse_one_block(item: &Value, harness_session_ids: &mut BTreeSet<String>) -> ContentBlock {
    let block_type = item.get("type").and_then(Value::as_str).unwrap_or("");
    match block_type {
        "text" => {
            let text = item.get("text").and_then(Value::as_str).unwrap_or("");
            text_block(text, harness_session_ids)
        }
        "tool_use" => ContentBlock::ToolUse {
            name: item
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
        },
        "tool_result" => ContentBlock::ToolResult {
            char_len: tool_result_char_len(
                item.get("content").unwrap_or(&Value::Null),
                harness_session_ids,
            ),
            tool_use_id: item
                .get("tool_use_id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
        },
        "thinking" => ContentBlock::Thinking,
        "image" => ContentBlock::Image,
        _ => ContentBlock::Other {
            char_len: item.to_string().chars().count(),
        },
    }
}

/// A `tool_result` block's `content` is itself either a plain string (the
/// overwhelmingly common shape in real transcripts) or an array of
/// `{"type":"text","text":...}` sub-blocks; anything else contributes no
/// chars rather than being stringified, so an untyped or non-text sub-block
/// (e.g. a nested `image`) is not silently counted as if it were text.
/// Measured as chars, never retained.
fn tool_result_char_len(content: &Value, harness_session_ids: &mut BTreeSet<String>) -> usize {
    match content {
        Value::String(text) => {
            find_session_ids(text, harness_session_ids);
            text.chars().count()
        }
        Value::Array(items) => items
            .iter()
            .filter(|item| item.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|item| item.get("text").and_then(Value::as_str))
            .map(|text| {
                find_session_ids(text, harness_session_ids);
                text.chars().count()
            })
            .sum(),
        _ => 0,
    }
}

/// Parses one telemetry JSONL file (`forge`'s own `DelegationEvent` lines,
/// or `context_telemetry.py`'s `HookTiming`/`HookContext` lines) into per-
/// hook call counts, latency percentiles and output bytes.
///
/// Hook name: the `tool` field when present (both Python event shapes name
/// the specific hook there); otherwise the `event` field (forge's own
/// `DelegationEvent`, which names the Claude Code hook *event*, e.g.
/// `PreToolUse`, since it has no per-hook breakdown of its own).
pub fn read_telemetry_file(path: &Path) -> Result<TelemetrySummary> {
    let bytes = fs::metadata(path)
        .with_context(|| format!("stat {}", path.display()))?
        .len();
    let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let reader = BufReader::new(file);

    let mut lines_total: u64 = 0;
    let mut read_stats = ReadStats::default();
    // (n_calls, latencies, total_output_bytes, first `event` value seen).
    let mut by_hook: std::collections::BTreeMap<String, (u64, Vec<f64>, u64, String)> =
        std::collections::BTreeMap::new();

    for (idx, line) in reader.lines().enumerate() {
        let line_number = idx + 1;
        lines_total += 1;
        let Ok(text) = line else {
            read_stats.skipped_lines.push(SkippedLine {
                line_number,
                reason: "read_error".to_string(),
            });
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(&text) else {
            read_stats.skipped_lines.push(SkippedLine {
                line_number,
                reason: "invalid_json".to_string(),
            });
            continue;
        };
        let hook = value
            .get("tool")
            .and_then(Value::as_str)
            .or_else(|| value.get("event").and_then(Value::as_str));
        let Some(hook) = hook else {
            read_stats.skipped_lines.push(SkippedLine {
                line_number,
                reason: "missing_hook_name".to_string(),
            });
            continue;
        };
        let entry = by_hook.entry(hook.to_string()).or_default();
        entry.0 += 1;
        if let Some(ms) = value.get("elapsed_ms").and_then(Value::as_f64) {
            entry.1.push(ms);
        }
        entry.2 += value
            .get("output_bytes")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        if entry.3.is_empty() {
            if let Some(event) = value.get("event").and_then(Value::as_str) {
                entry.3 = event.to_string();
            }
        }
    }

    let hooks = by_hook
        .into_iter()
        .map(
            |(hook, (n_calls, mut latencies, total_output_bytes, event))| {
                latencies.sort_by(|a, b| a.partial_cmp(b).expect("elapsed_ms is never NaN"));
                HookStats {
                    hook,
                    event,
                    n_calls,
                    p50_ms: percentile(&latencies, 0.50),
                    p90_ms: percentile(&latencies, 0.90),
                    total_output_bytes,
                }
            },
        )
        .collect();

    Ok(TelemetrySummary {
        path: path.display().to_string(),
        bytes,
        lines: lines_total,
        read_stats,
        hooks,
    })
}

/// Nearest-rank percentile over an already-sorted slice. `None` when there
/// is no latency sample at all (a hook with only `HookContext`-shaped lines,
/// which carry no `elapsed_ms`).
fn percentile(sorted: &[f64], p: f64) -> Option<f64> {
    if sorted.is_empty() {
        return None;
    }
    let rank = (p * sorted.len() as f64).ceil() as usize;
    let idx = rank.saturating_sub(1).min(sorted.len() - 1);
    Some(sorted[idx])
}
