//! Schema for the cost ledger's first input kind: harness transcript JSONL.
//!
//! `docs/design/cost_ledger.md` section 2. The reader keeps shapes, never
//! content -- Section 3 explains why per-block attribution has to be an
//! estimate, and this module is the boundary past which block *text* does
//! not travel: only `char_len` and a handful of structural fields do.

use serde::{Deserialize, Serialize};

/// One line of a Claude Code transcript JSONL file, as written by the
/// harness. Only the fields the ledger needs are typed; anything else in
/// the line is ignored by `serde`'s default "unknown fields are dropped"
/// behavior for a non-`deny_unknown_fields` struct.
#[derive(Debug, Clone, Deserialize)]
pub struct TranscriptLine {
    pub uuid: String,
    /// Not read yet -- links a line into the harness's message tree, which
    /// only matters once a consumer needs to reconstruct branches (not this
    /// PR's summaries). Kept typed for schema fidelity with design section 2
    /// and so step 2 does not need to touch this struct to add it.
    #[serde(default)]
    #[allow(dead_code)]
    pub parent_uuid: Option<String>,
    /// The harness's own field is `sessionId` (camelCase); this repo's
    /// existing tooling additionally stamps a `session_id` duplicate onto
    /// main-session transcript lines (not this reader's doing), which is
    /// why PR #35's fixtures and this reader use snake_case as the primary
    /// name. A real *subagent* transcript (`agent-*.jsonl`) never gets that
    /// duplicate, only the harness's own `sessionId` -- `serde(alias)` is
    /// not used here because a line carrying *both* keys (every real
    /// main-session line) would then hit serde's "duplicate field" error;
    /// `reader.rs::read_transcript_file` instead reads `sessionId` off the
    /// raw `Value` itself and fills this field only when it parsed empty.
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub timestamp: Option<String>,
    #[serde(default)]
    pub message: Option<RawMessage>,
    /// True on the one line the harness writes immediately after a
    /// compaction (a `type: "user"` line carrying the compaction summary,
    /// no `usage`). Step 2's compaction detection (`rollup.rs`,
    /// `docs/design/cost_ledger.md` section 2) keys off this field; picked
    /// over the sibling `type: "system"`/`subtype: "compact_boundary"`
    /// marker so one compaction yields exactly one counted event instead of
    /// two redundant ones. Additive since PR #35: defaults `false` so every
    /// existing fixture and caller is unaffected.
    #[serde(default, rename = "isCompactSummary")]
    pub is_compact_summary: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawMessage {
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub usage: Option<Usage>,
    /// The harness writes this as either a plain string (short user turns)
    /// or an array of typed blocks (assistant turns, and most user turns
    /// carrying tool results). `raw_content_blocks` in `reader.rs` handles
    /// both shapes without panicking on either.
    #[serde(default)]
    pub content: serde_json::Value,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize)]
pub struct Usage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_read_input_tokens: u64,
    #[serde(default)]
    pub cache_creation_input_tokens: u64,
}

/// Tagged by `type` in the transcript; only the char length of text-bearing
/// blocks is read, never the text itself, past the reader boundary -- the
/// ledger stores shapes, not content. `is_system_reminder` on `Text` is true
/// when the block's text starts with `<system-reminder>`, decided inside the
/// reader (which sees the text once, then drops it) and carried out here as
/// a bool rather than the string that produced it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContentBlock {
    Text {
        char_len: usize,
        is_system_reminder: bool,
    },
    ToolUse {
        name: String,
    },
    ToolResult {
        char_len: usize,
        tool_use_id: String,
    },
    Thinking,
    Image,
    /// A `type` the reader does not recognize. The design (section 2) lists
    /// five variants observed at design time; this sixth one is the hard
    /// rule from the build brief -- an unknown block type counts here
    /// instead of failing the line.
    Other {
        char_len: usize,
    },
}

/// Per-block-type char totals, accumulated either over one turn or one
/// whole file depending on where it is stored. Field order matches the
/// design's `bytes_by_block_type` key order (section 2) plus `other` for
/// the hard-rule catch-all.
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct BlockTypeCounts {
    pub text: u64,
    pub tool_result: u64,
    pub tool_use: u64,
    pub thinking: u64,
    pub image: u64,
    pub other: u64,
}

impl BlockTypeCounts {
    pub fn add(&mut self, block: &ContentBlock) {
        match block {
            ContentBlock::Text { char_len, .. } => self.text += *char_len as u64,
            ContentBlock::ToolResult { char_len, .. } => self.tool_result += *char_len as u64,
            ContentBlock::ToolUse { .. } => self.tool_use += 1,
            ContentBlock::Thinking => self.thinking += 1,
            ContentBlock::Image => self.image += 1,
            ContentBlock::Other { char_len } => self.other += *char_len as u64,
        }
    }
}

/// One line the reader could not turn into a `TranscriptLine`: not valid
/// JSON (parse error, including a truncated final line from a file caught
/// mid-write -- both fail `serde_json::from_str` the same way and are
/// reported the same way), or valid JSON lacking `uuid`. 1-indexed to match
/// what a human opening the file in an editor would call it.
#[derive(Debug, Clone, Serialize)]
pub struct SkippedLine {
    pub line_number: usize,
    pub reason: String,
}

/// Everything the reader could not attribute to a parsed `TranscriptLine`.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ReadStats {
    pub skipped_lines: Vec<SkippedLine>,
}

/// One `turn_attribution`-shaped row (design section 2): one assistant
/// message that carries a `usage` block. Field order matches the design's
/// `turn_attribution` name order for the fields this PR can measure.
/// `tier` and `estimated_tokens_by_block_type` are named in the design but
/// are not emitted by step 1: tier inference is step 4's design choice
/// (section 6), and section 3.4 requires the ledger to refuse an
/// uncalibrated per-block token estimate rather than print one silently --
/// no calibration exists until step 3, so this struct has nothing to put
/// there and omits both fields rather than fabricate a value.
#[derive(Debug, Clone, Serialize)]
pub struct TurnSummary {
    pub session_id: Option<String>,
    pub turn_index: usize,
    pub turn_uuid: String,
    pub timestamp: Option<String>,
    pub model: Option<String>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub bytes_by_block_type: BlockTypeCounts,
}

/// Whole-file summary emitted by `forge ledger read`. `--json` prints one of
/// these per input file; `--summary` prints the same numbers as one text
/// line per file.
#[derive(Debug, Clone, Serialize)]
pub struct TranscriptSummary {
    pub path: String,
    pub bytes: u64,
    pub lines: u64,
    pub turns_with_usage: u64,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_cache_read_input_tokens: u64,
    pub total_cache_creation_input_tokens: u64,
    pub chars_by_block_type: BlockTypeCounts,
    pub read_stats: ReadStats,
    pub turns: Vec<TurnSummary>,
    /// One entry per `isCompactSummary` line seen (additive since PR #35;
    /// empty for a file with no compaction). Step 2's `rollup.rs` primary
    /// compaction-count signal.
    pub compaction_markers: Vec<CompactionMarker>,
}

/// One `isCompactSummary` line: the harness-written marker for one
/// compaction event (`docs/design/cost_ledger.md` section 2).
#[derive(Debug, Clone, Serialize)]
pub struct CompactionMarker {
    pub session_id: Option<String>,
    pub timestamp: Option<String>,
}

/// One row of `hook_rollup`-shaped telemetry (design section 2): per hook
/// name, call count and latency/byte stats from the telemetry JSONL stream
/// (`telemetry.rs::DelegationEvent` and `context_telemetry.py`'s
/// `HookTiming`/`HookContext` events).
#[derive(Debug, Clone, Serialize)]
pub struct HookStats {
    pub hook: String,
    /// The line's raw top-level `"event"` value: the telemetry record kind
    /// for the Python shapes (`HookTiming`/`HookContext`), or the Claude
    /// Code hook event name itself for forge's own `DelegationEvent` lines
    /// (which carry no separate `tool` field, so `hook` above is already
    /// that same value -- `event` duplicates it in that case, documented in
    /// `rollup.rs`, not fabricated). First value seen for this hook name.
    /// Additive since PR #35; `String::new()` for a hook name whose only
    /// lines carried neither key (should not occur for either known shape).
    pub event: String,
    pub n_calls: u64,
    pub p50_ms: Option<f64>,
    pub p90_ms: Option<f64>,
    pub total_output_bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct TelemetrySummary {
    pub path: String,
    pub bytes: u64,
    pub lines: u64,
    pub read_stats: ReadStats,
    pub hooks: Vec<HookStats>,
}

/// Which of the design's two input kinds a file is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputKind {
    Transcript,
    Telemetry,
}
