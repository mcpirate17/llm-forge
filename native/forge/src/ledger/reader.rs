//! Streaming readers for the cost ledger's two input kinds (design section 2):
//! harness transcript JSONL and hook telemetry JSONL. Both stream with
//! `BufRead::lines` -- never `read_to_string` -- because transcripts reach
//! 100 MB (design "Measured this session" table).

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::Path;

use anyhow::{Context, Result};
use serde_json::Value;

use super::schema::{
    AgentDispatch, BlockTypeCounts, CompactionMarker, ContentBlock, HookStats, InputKind,
    ReadStats, SkippedLine, TelemetrySummary, TranscriptLine, TranscriptSummary, TurnSummary,
};
use super::session_ids::{find_agent_id, find_session_ids};

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
        agent_id: None,
        parent_session_id: None,
        is_subagent: false,
        agent_dispatches: Vec::new(),
        compaction_markers: Vec::new(),
        harness_session_ids: Vec::new(),
        commit_subject_digests: Vec::new(),
    };
    let mut harness_session_ids: BTreeSet<String> = BTreeSet::new();
    let mut commit_subject_digests: BTreeSet<String> = BTreeSet::new();
    // The first session id any line carries: for a subagent file this is the
    // PARENT session's uuid (every subagent line's `sessionId` names its
    // parent), which `parent_session_id` reports; for a top-level file it is
    // simply unused (its own turns already carry their session per line).
    let mut first_session_id: Option<String> = None;
    // tool_use_id -> index into `summary.agent_dispatches` for an `Agent`
    // dispatch still waiting for its `tool_result` to name the subagent.
    let mut pending_dispatches: BTreeMap<String, usize> = BTreeMap::new();

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
        // A subagent transcript's identity: every line carries the same
        // `agentId` (17 hex) alongside its parent's `sessionId`. The first
        // line that has one settles the file's identity; read off the raw
        // value before `from_value` consumes it, exactly like the fallback
        // above.
        if summary.agent_id.is_none() {
            if let Some(agent_id) = value.get("agentId").and_then(Value::as_str) {
                summary.agent_id = Some(agent_id.to_string());
            }
        }
        if first_session_id.is_none() {
            first_session_id = parsed_session_id_hint(&value).or(session_id_fallback.clone());
        }
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
        accumulate_transcript_line(
            &mut summary,
            parsed,
            &mut harness_session_ids,
            &mut commit_subject_digests,
            &mut pending_dispatches,
        );
    }

    summary.is_subagent = summary.agent_id.is_some();
    if summary.is_subagent {
        summary.parent_session_id = first_session_id;
    }
    summary.harness_session_ids = harness_session_ids.into_iter().collect();
    summary.commit_subject_digests = commit_subject_digests.into_iter().collect();
    Ok(summary)
}

/// A line's snake_case `session_id` duplicate (this repo's own tooling
/// stamps it onto main-session lines) -- the other half of the
/// first-session-id capture besides the harness's camelCase `sessionId`.
fn parsed_session_id_hint(value: &Value) -> Option<String> {
    value
        .get("session_id")
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn accumulate_transcript_line(
    summary: &mut TranscriptSummary,
    line: TranscriptLine,
    harness_session_ids: &mut BTreeSet<String>,
    commit_subject_digests: &mut BTreeSet<String>,
    pending_dispatches: &mut BTreeMap<String, usize>,
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
    track_agent_dispatch(
        summary,
        &message.content,
        &line.session_id,
        &line.timestamp,
        pending_dispatches,
    );
    track_commit_subjects(&message.content, commit_subject_digests);
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

/// Extracts `Agent` dispatches from one line's content blocks: an
/// `Agent`-named `tool_use` opens a pending dispatch (its structural input
/// fields only -- never `prompt`), and a `tool_result` whose id matches
/// closes it with the `agentId` its text reports. The result arrives on a
/// later line than the call, which is why the pending map lives across
/// lines in the caller.
fn track_agent_dispatch(
    summary: &mut TranscriptSummary,
    content: &Value,
    line_session_id: &Option<String>,
    line_timestamp: &Option<String>,
    pending_dispatches: &mut BTreeMap<String, usize>,
) {
    let Some(items) = content.as_array() else {
        return;
    };
    for item in items {
        match item.get("type").and_then(Value::as_str) {
            Some("tool_use") if item.get("name").and_then(Value::as_str) == Some("Agent") => {
                let Some(id) = item.get("id").and_then(Value::as_str) else {
                    continue;
                };
                let input = item.get("input");
                let field = |key: &str| -> Option<String> {
                    input
                        .and_then(|value| value.get(key))
                        .and_then(Value::as_str)
                        .map(str::to_string)
                };
                let index = summary.agent_dispatches.len();
                summary.agent_dispatches.push(AgentDispatch {
                    session_id: line_session_id.clone(),
                    timestamp: line_timestamp.clone(),
                    tool_use_id: id.to_string(),
                    subagent_type: field("subagent_type"),
                    description: field("description"),
                    model_requested: field("model"),
                    agent_id: None,
                });
                pending_dispatches.insert(id.to_string(), index);
            }
            Some("tool_result") => {
                let Some(id) = item.get("tool_use_id").and_then(Value::as_str) else {
                    continue;
                };
                let Some(&index) = pending_dispatches.get(id) else {
                    continue;
                };
                let text = tool_result_texts(item.get("content").unwrap_or(&Value::Null)).join("");
                if let Some(agent_id) = find_agent_id(&text) {
                    summary.agent_dispatches[index].agent_id = Some(agent_id);
                    pending_dispatches.remove(id);
                }
            }
            _ => {}
        }
    }
}

/// Extracts commit subjects from one line's `Bash` `tool_use` blocks: the
/// command text is scanned for the three shapes a typed subject takes
/// (`subjects_from_bash_command`), and every subject that survives
/// `subject.rs`'s floor becomes a digest in `commit_subject_digests` --
/// never the text. Mirrors `track_agent_dispatch`'s walk (the other
/// tool_use-input reader) and reads `input.command` the same structural way.
fn track_commit_subjects(content: &Value, digests: &mut BTreeSet<String>) {
    let Some(items) = content.as_array() else {
        return;
    };
    for item in items {
        if item.get("type").and_then(Value::as_str) != Some("tool_use") {
            continue;
        }
        if item.get("name").and_then(Value::as_str) != Some("Bash") {
            continue;
        }
        let Some(command) = item
            .get("input")
            .and_then(|input| input.get("command"))
            .and_then(Value::as_str)
        else {
            continue;
        };
        for subject in subjects_from_bash_command(command) {
            let digest = super::subject::subject_digest(&subject);
            if !digest.is_empty() {
                digests.insert(digest);
            }
        }
    }
}

/// The three shapes a typed commit subject takes in a Bash command:
/// (a) the first `-m` argument of a `git commit` (single- or
/// double-quoted; `$'...'` needs no handling of its own -- its payload
/// still ends at the closing quote), (b) the first non-empty line of a
/// heredoc body when the command runs `git commit` with `-F -`/`-F-` and a
/// `<<'EOF'`/`<<EOF`-style heredoc, (c) the `--title` argument of a
/// `gh pr create` (the squash-merged subject is that title plus ` (#N)`,
/// which `subject.rs`'s normalization strips on the landed side). At most
/// one subject per command: only the first `-m` counts (the second is the
/// body), and a `git commit --amend` with no message argument records
/// nothing. Returns raw subject text; hashing and the short-subject
/// refusal live in `subject.rs`, shared with the landed side.
fn subjects_from_bash_command(command: &str) -> Vec<String> {
    if token_index(command, "git commit").is_some() {
        if let Some(subject) = first_message_argument(command) {
            return vec![subject];
        }
        if reads_message_from_stdin(command) {
            if let Some(subject) = heredoc_first_line(command) {
                return vec![subject];
            }
        }
        return Vec::new();
    }
    if token_index(command, "gh pr create").is_some() {
        if let Some(title) = title_argument(command) {
            return vec![title];
        }
    }
    Vec::new()
}

/// Index of `token` in `haystack` when it stands as a shell word: preceded
/// by whitespace (or nothing) and followed by whitespace (or nothing, or
/// `=`/a quote so `--title=` and `-m"..."` count). `None` when the string
/// only occurs inside a longer word.
fn token_index(haystack: &str, token: &str) -> Option<usize> {
    let mut from = 0;
    while let Some(found) = haystack[from..].find(token) {
        let start = from + found;
        let end = start + token.len();
        let bounded_before = haystack[..start]
            .chars()
            .next_back()
            .is_none_or(char::is_whitespace);
        let bounded_after = haystack[end..]
            .chars()
            .next()
            .is_none_or(|c| c.is_whitespace() || c == '=' || c == '"' || c == '\'');
        if bounded_before && bounded_after {
            return Some(start);
        }
        from = start + 1;
    }
    None
}

/// The value of a quoted argument starting at `rest[0]` (the opening quote
/// itself): single quotes run to the next `'` with no escapes; double
/// quotes honor `\"` and `\\` and keep anything else verbatim. `None` when
/// `rest` does not start with a quote or the closing quote never came.
fn quoted_argument(rest: &str) -> Option<String> {
    let mut chars = rest.chars();
    match chars.next()? {
        '\'' => {
            let end = chars.by_ref().position(|c| c == '\'')?;
            Some(rest[1..end + 1].to_string())
        }
        '"' => {
            let mut out = String::new();
            while let Some(c) = chars.next() {
                match c {
                    '"' => return Some(out),
                    '\\' => match chars.next() {
                        Some(escaped @ ('"' | '\\')) => out.push(escaped),
                        Some(other) => {
                            out.push('\\');
                            out.push(other);
                        }
                        None => return None,
                    },
                    _ => out.push(c),
                }
            }
            None
        }
        _ => None,
    }
}

/// The first `-m` argument after the (first) `git commit` token: shell
/// whitespace may sit between the flag and its value, and the value may be
/// glued to the flag (`-m"..."`). `None` when the commit has no message
/// flag at all (e.g. `git commit --amend --no-edit`) or the quote never
/// closed.
fn first_message_argument(command: &str) -> Option<String> {
    let commit_at = token_index(command, "git commit")?;
    let flag_at = token_index(&command[commit_at..], "-m")? + commit_at;
    let rest = command[flag_at + 2..].trim_start();
    quoted_argument(rest)
}

/// True when the `git commit` reads its message from stdin: a `-F -` flag
/// pair or the glued `-F-` form.
fn reads_message_from_stdin(command: &str) -> bool {
    if token_index(command, "-F-").is_some() {
        return true;
    }
    match token_index(command, "-F") {
        Some(at) => command[at + 2..]
            .trim_start()
            .strip_prefix('-')
            .is_some_and(|after| after.starts_with(char::is_whitespace) || after.is_empty()),
        None => false,
    }
}

/// The first non-empty line of a `<<'EOF'`/`<<EOF`/`<<-` heredoc body: the
/// body starts on the line AFTER the one carrying the marker and ends at
/// the marker line. `None` when no heredoc marker (or no body) exists.
fn heredoc_first_line(command: &str) -> Option<String> {
    let at = command.find("<<")?;
    let after_markers = &command[at + 2..];
    let after_markers = after_markers.strip_prefix('-').unwrap_or(after_markers);
    let word: String = after_markers
        .trim_start_matches(['\'', '"'])
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    if word.is_empty() {
        return None;
    }
    let body_start = at + command[at..].find('\n')? + 1;
    for line in command[body_start..].lines() {
        if line.trim() == word {
            break;
        }
        let trimmed = line.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    None
}

/// The `--title` argument of a `gh pr create`: `--title "<title>"` (quoted)
/// or `--title=<title>`/a bare `--title <word>` (up to whitespace). `None`
/// when the flag or its value is absent.
fn title_argument(command: &str) -> Option<String> {
    let at = token_index(command, "--title")?;
    let rest = &command[at + "--title".len()..];
    if let Some(value) = rest.strip_prefix('=') {
        let word: String = value.chars().take_while(|c| !c.is_whitespace()).collect();
        return (!word.is_empty()).then_some(word);
    }
    let rest = rest.trim_start();
    quoted_argument(rest).or_else(|| {
        let word: String = rest.chars().take_while(|c| !c.is_whitespace()).collect();
        (!word.is_empty()).then_some(word)
    })
}

/// A `tool_result`'s text pieces (plain string, or the text sub-blocks of
/// an array), for the one scan that legitimately reads result text: the
/// `agentId` line the Agent tool itself writes. Bytes never leave this fn
/// except as the matched id.
fn tool_result_texts(content: &Value) -> Vec<&str> {
    match content {
        Value::String(text) => vec![text.as_str()],
        Value::Array(items) => items
            .iter()
            .filter(|item| item.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|item| item.get("text").and_then(Value::as_str))
            .collect(),
        _ => Vec::new(),
    }
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
            // The calibration step's unit fix: the block's structural JSON
            // (serialized `input`) plus the tool name, in chars -- the same
            // unit as every other block type in the proportional split. An
            // absent `input` contributes 0 rather than counting the four
            // chars of JSON `null`, which the block never actually sent.
            char_len: item
                .get("input")
                .map(|input| input.to_string().chars().count())
                .unwrap_or(0)
                + item
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .chars()
                    .count(),
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
        // `image.source` is either `{"type":"base64","data":...}` (chars of
        // the base64 payload -- what the API actually bills) or a URL source
        // (the payload is fetched server-side and never transits the request
        // the way bytes do, so it contributes 0, not the URL's length).
        "image" => ContentBlock::Image {
            char_len: match item.get("source") {
                Some(source) if source.get("type").and_then(Value::as_str) == Some("base64") => {
                    source
                        .get("data")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .chars()
                        .count()
                }
                _ => 0,
            },
        },
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

#[cfg(test)]
mod tests {
    use super::{subjects_from_bash_command, track_commit_subjects};
    use crate::ledger::subject::subject_digest;
    use serde_json::json;
    use std::collections::BTreeSet;

    fn subjects(command: &str) -> Vec<String> {
        subjects_from_bash_command(command)
    }

    #[test]
    fn first_m_of_a_git_commit_is_the_subject_quoted_either_way() {
        // (a): the subject is the FIRST -m; the second -m is the body.
        assert_eq!(
            subjects(r#"git commit -m 'feat: wire the subject join' -m "body line""#),
            vec!["feat: wire the subject join".to_string()]
        );
        assert_eq!(
            subjects(
                r#"git add -A && git commit -m "fix(ledger): double quotes in a compound command""#
            ),
            vec!["fix(ledger): double quotes in a compound command".to_string()]
        );
        // The value may be glued to the flag; escapes inside double quotes
        // stay literal subject text after unescaping.
        assert_eq!(
            subjects(r#"git commit -m"chore: glued form also carries a subject""#),
            vec!["chore: glued form also carries a subject".to_string()]
        );
        assert_eq!(
            subjects(r#"git commit -m "fix: say \"hi\" once""#),
            vec![r#"fix: say "hi" once"#.to_string()]
        );
    }

    #[test]
    fn a_heredoc_message_on_stdin_yields_its_first_non_empty_line() {
        // (b): git commit -F - with a <<'EOF' heredoc; the body's first
        // non-empty line is the subject.
        assert_eq!(
            subjects("git commit -F - <<'EOF'\nfeat: heredoc subject line\n\nbody\nEOF"),
            vec!["feat: heredoc subject line".to_string()]
        );
        assert_eq!(
            subjects("git commit -F- <<EOF\n\nchore: unquoted heredoc, leading blank\nEOF"),
            vec!["chore: unquoted heredoc, leading blank".to_string()]
        );
    }

    #[test]
    fn gh_pr_create_title_is_the_subject() {
        // (c): a squash-merged PR's landed subject is this title plus
        // " (#N)", which normalization strips on the landed side.
        assert_eq!(
            subjects(
                r#"gh pr create --title "feat(ledger): credit the session that typed it" --body-file /tmp/x"#
            ),
            vec!["feat(ledger): credit the session that typed it".to_string()]
        );
        // The `=` form carries the value up to whitespace -- a real shell
        // would split anything with a space into separate arguments, so a
        // spaced title always arrives quoted.
        assert_eq!(
            subjects("gh pr create --title=fix-typo-subject --fill"),
            vec!["fix-typo-subject".to_string()]
        );
    }

    #[test]
    fn amend_without_a_message_and_unrelated_commands_record_nothing() {
        assert!(subjects("git commit --amend --no-edit").is_empty());
        assert!(subjects("git commit --amend").is_empty());
        assert!(subjects("git commit -m").is_empty()); // flag with no value
        assert!(subjects("git log --oneline -5").is_empty());
        assert!(subjects("git commit -m 'unclosed quote").is_empty());
        // `-F -` without a heredoc body still yields nothing.
        assert!(subjects("git commit -F -").is_empty());
    }

    #[test]
    fn only_bash_tool_use_blocks_are_scanned() {
        let mut digests = BTreeSet::new();
        // An Edit-named tool_use whose input carries a command-shaped
        // string: not a Bash block, nothing recorded.
        track_commit_subjects(
            &json!([{ "type": "tool_use", "name": "Edit", "input": { "command": "git commit -m 'not a bash block'" } }]),
            &mut digests,
        );
        assert!(digests.is_empty());
        // The same shape named Bash records exactly the digest.
        track_commit_subjects(
            &json!([{ "type": "tool_use", "name": "Bash", "input": { "command": "git commit -m 'feat: one bash block'" } }]),
            &mut digests,
        );
        // "feat: one bash block" is 21 chars, above the floor.
        assert_eq!(
            digests.into_iter().collect::<Vec<_>>(),
            vec![subject_digest("feat: one bash block")]
        );
    }

    #[test]
    fn reader_side_and_landed_side_digests_agree() {
        // The landed subject carries the squash suffix the session never
        // typed; both sides must land on the same digest.
        let typed = subjects(r#"gh pr create --title "feat(x): reader and landed agree""#);
        let landed = "feat(x): reader and landed agree (#51)";
        assert_eq!(subject_digest(&typed[0]), subject_digest(landed));
        // And the git-commit form agrees with the landed subject verbatim.
        let committed = subjects("git commit -m 'fix: same subject on both sides'");
        assert_eq!(
            subject_digest(&committed[0]),
            subject_digest("fix: same subject on both sides")
        );
    }
}
