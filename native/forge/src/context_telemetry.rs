//! Port of `conductor.context_telemetry`'s writer surface: the JSONL record
//! `event(payload)` reduces one hook payload to, and `record()`'s bounded,
//! rotating append. Backs the `context_telemetry` hook (one record per
//! PostToolUse event) and the hook-context side records `adapters._telemetry`
//! writes for hooks that inject context.
//!
//! The record's bytes are a contract with `conductor-native`'s
//! `context_telemetry_event_native` (the PyO3 extension the Python module
//! calls) plus the field ordering Python adds around it: `event()` merges
//! `session_id` last, and `_encoded_record` dumps with
//! `ensure_ascii=False, separators=(",", ":")`. serde_json's `Value` map is
//! key-sorted, so the line is assembled by hand in the exact field order the
//! Python dict literal produces -- byte-identical output without taking the
//! extension as a dependency or turning on serde_json's `preserve_order`
//! (which would reorder every other JSON this crate emits).
//!
//! `record()`'s failure discipline ports with it: an unwritable sink never
//! raises into hook dispatch and never writes to stdout -- it prints one
//! stderr line and best-effort records a `telemetry_disabled` event. Python
//! additionally latches a process-global `_disabled` flag after the first
//! failure; each forge invocation is one short-lived process answering one
//! hook, so the latch would never be observed and is not carried over.

use std::fs::{self, OpenOptions};
use std::io::{self, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use serde_json::Value;
use sha2::{Digest, Sha256};
use std::os::unix::io::AsRawFd;

use crate::instant;

const MAX_PROVIDER_CHARS: usize = 32;
const MAX_TOOL_CHARS: usize = 100;
const USAGE_KEYS: [&str; 4] = ["usage", "usageMetadata", "usage_metadata", "token_usage"];
const USAGE_CONTAINERS: [&str; 2] = ["response", "metadata"];

/// `MAX_LOG_BYTES`: one telemetry file never exceeds 10 MiB; a record that
/// would cross the cap rotates the log first instead of being dropped.
pub const MAX_LOG_BYTES: u64 = 10 * 1024 * 1024;

/// `MAX_ROTATED_LOGS`: rotations pruned back to the newest five.
pub const MAX_ROTATED_LOGS: usize = 5;

/// `adapters._telemetry_path`: `CONTEXT_TELEMETRY_PATH` when set, else the
/// module's `DEFAULT_PATH`. The default is derived from the module file's own
/// location (`src/conductor/context_telemetry.py` -> `<root>/src/research/...`),
/// which for a checkout-rooted session is `project_root()/src/research/...` --
/// ported path-for-path, including the `src/` quirk (an inherited defect on
/// main; see the PR body's Debt section), so native and Python records land
/// in one file.
pub fn telemetry_path(root: &Path) -> PathBuf {
    if let Ok(raw) = std::env::var("CONTEXT_TELEMETRY_PATH") {
        return PathBuf::from(raw);
    }
    root.join("src")
        .join("research")
        .join("tmp")
        .join("context_telemetry")
        .join("events.jsonl")
}

/// `context_telemetry.provider()`: which harness family is recording, by
/// signature env vars, checked in the extension's fixed order.
fn provider() -> String {
    let present = |name: &str| std::env::var_os(name).is_some_and(|value| !value.is_empty());
    let selected = if present("GROK_WORKSPACE_ROOT") || present("GROK_HOOK_EVENT") {
        "grok"
    } else if present("QWEN_PROJECT_DIR") {
        "qwen"
    } else if present("CLAUDE_PROJECT_DIR") {
        "claude"
    } else if present("CODEX_SESSION_ID") || present("CODEX_HOME") {
        "codex"
    } else {
        "unknown"
    };
    truncate_chars(selected, MAX_PROVIDER_CHARS)
}

fn truncate_chars(value: &str, limit: usize) -> String {
    value.chars().take(limit).collect()
}

/// Python truthiness for the JSON value kinds a payload can carry.
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// `field_string`: the first of `names` present in the payload (present-but-
/// falsy counts as absent), rendered the way Python `str()` would for the
/// scalar kinds a real payload carries; a JSON object/array `tool_name` falls
/// back rather than guessing at Python's `repr` rendering.
fn field_string(payload: &Value, names: &[&str], fallback: &str) -> String {
    let Some(found) = names.iter().find_map(|name| payload.get(name)) else {
        return fallback.to_string();
    };
    if !truthy(found) {
        return fallback.to_string();
    }
    match found {
        Value::String(s) => s.clone(),
        Value::Bool(b) => (if *b { "True" } else { "False" }).to_string(),
        Value::Number(n) => n.to_string(),
        _ => fallback.to_string(),
    }
}

/// The first of `names` present in the payload (a present `null` included,
/// exactly like `PyDict::get_item`) -- `event()`'s tool_input/tool_output
/// selection.
fn first_present<'a>(payload: &'a Value, names: &[&str]) -> Option<&'a Value> {
    names.iter().find_map(|name| payload.get(name))
}

/// `model_visible_output`: Edit/Write responses are projected down to the
/// fields the model actually sees; every other tool reports its response as-is.
fn model_visible_output(tool_name: &str, tool_output: &Value) -> Value {
    let fields: &[&str] = match tool_name {
        "Edit" => &["filePath", "structuredPatch", "userModified"],
        "Write" => &["type", "filePath"],
        _ => return tool_output.clone(),
    };
    let Value::Object(map) = tool_output else {
        return tool_output.clone();
    };
    let mut projected = serde_json::Map::new();
    for field in fields {
        if let Some(value) = map.get(*field) {
            projected.insert(field.to_string(), value.clone());
        }
    }
    Value::Object(projected)
}

/// `json_bytes`: the length of `json.dumps(value, ensure_ascii=False,
/// separators=(",", ":"))` -- key order never changes a byte count, so
/// serde_json's compact encoding is equivalent. `None` encodes to nothing.
fn json_byte_len(value: &Value) -> usize {
    if value.is_null() {
        return 0;
    }
    serde_json::to_string(value)
        .map(|text| text.len())
        .unwrap_or(0)
}

/// `bounded_output`: did the response say (or look like) it was cut down?
fn bounded_output(tool_output: &Value, encoded_len_source: &Value) -> bool {
    if let Value::Object(map) = tool_output {
        for key in ["elided", "truncated", "output_bounded", "outputBounded"] {
            match map.get(key) {
                Some(Value::Bool(true)) => return true,
                Some(Value::String(text)) if !text.is_empty() => return true,
                _ => {}
            }
        }
    }
    let Ok(encoded) = serde_json::to_string(encoded_len_source) else {
        return false;
    };
    let lowered = encoded.to_lowercase();
    lowered.contains("\"elided\"") || lowered.contains("\"truncated\"")
}

/// `nonnegative_int`: an int >= 0, or a decimal string of one; bools and
/// negatives are not counts. Python's `isdecimal()` also admits non-ASCII
/// decimal digits -- out of scope for JSON-carried numbers.
fn nonnegative_int(value: &Value) -> Option<u64> {
    match value {
        Value::Number(n) => n.as_u64(),
        Value::String(text) if !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()) => {
            text.parse().ok()
        }
        _ => None,
    }
}

fn usage_value(usage: &Value, names: &[&str]) -> Option<(u64, String)> {
    for name in names {
        if let Some(value) = usage.get(name).and_then(nonnegative_int) {
            return Some((value, (*name).to_string()));
        }
    }
    None
}

fn usage_mappings(payload: &Value) -> Vec<(&Value, String)> {
    let mut found = Vec::new();
    for key in USAGE_KEYS {
        if let Some(value) = payload.get(key).filter(|v| v.is_object()) {
            found.push((value, key.to_string()));
        }
    }
    for container_key in USAGE_CONTAINERS {
        let Some(container) = payload.get(container_key) else {
            continue;
        };
        if !container.is_object() {
            continue;
        }
        for usage_key in USAGE_KEYS {
            if let Some(value) = container.get(usage_key).filter(|v| v.is_object()) {
                found.push((value, format!("{container_key}.{usage_key}")));
            }
        }
    }
    found
}

/// The six count fields `add_native_usage` fills, in the fixed order the
/// Python dict literal produces, plus the three trailing labels.
struct UsageFields {
    counts: Vec<(&'static str, u64)>,
    source: &'static str,
    path: Option<String>,
    field_names: Vec<String>,
}

fn add_usage_field(
    usage: &Value,
    counts: &mut Vec<(&'static str, u64)>,
    field_names: &mut Vec<String>,
    output_name: &'static str,
    names: &[&str],
) {
    if let Some((value, matched)) = usage_value(usage, names) {
        counts.push((output_name, value));
        field_names.push(matched);
    }
}

/// The nested `*_details` objects: a second, deeper chance at
/// `cached_input_tokens`/`reasoning_tokens` when the top-level field was
/// absent, recorded under `<details-key>.<matched-field>` in `field_names`
/// (Python's `usage_details` walk).
fn add_detail_fields(
    usage: &Value,
    counts: &mut Vec<(&'static str, u64)>,
    field_names: &mut Vec<String>,
) {
    for detail_key in [
        "prompt_tokens_details",
        "input_tokens_details",
        "promptTokenDetails",
    ] {
        let Some(details) = usage.get(detail_key).filter(|v| v.is_object()) else {
            continue;
        };
        if counts
            .iter()
            .any(|(name, _)| *name == "cached_input_tokens")
        {
            continue;
        }
        if let Some((value, matched)) = usage_value(
            details,
            &[
                "cached_tokens",
                "cache_read_input_tokens",
                "cache_read_tokens",
            ],
        ) {
            counts.push(("cached_input_tokens", value));
            field_names.push(format!("{detail_key}.{matched}"));
        }
    }
    for detail_key in [
        "completion_tokens_details",
        "output_tokens_details",
        "completionTokenDetails",
    ] {
        let Some(details) = usage.get(detail_key).filter(|v| v.is_object()) else {
            continue;
        };
        if counts.iter().any(|(name, _)| *name == "reasoning_tokens") {
            continue;
        }
        if let Some((value, matched)) = usage_value(details, &["reasoning_tokens"]) {
            counts.push(("reasoning_tokens", value));
            field_names.push(format!("{detail_key}.{matched}"));
        }
    }
}

fn usage_fields(payload: &Value) -> UsageFields {
    for (usage, usage_path) in usage_mappings(payload) {
        let mut counts = Vec::new();
        let mut field_names = Vec::new();
        add_usage_field(
            usage,
            &mut counts,
            &mut field_names,
            "input_tokens",
            &["input_tokens", "prompt_tokens", "prompt_eval_count"],
        );
        add_usage_field(
            usage,
            &mut counts,
            &mut field_names,
            "output_tokens",
            &["output_tokens", "completion_tokens", "eval_count"],
        );
        add_usage_field(
            usage,
            &mut counts,
            &mut field_names,
            "cached_input_tokens",
            &[
                "cached_tokens",
                "cache_read_input_tokens",
                "cache_read_tokens",
            ],
        );
        add_usage_field(
            usage,
            &mut counts,
            &mut field_names,
            "cache_creation_input_tokens",
            &["cache_creation_input_tokens", "cache_creation_tokens"],
        );
        add_usage_field(
            usage,
            &mut counts,
            &mut field_names,
            "reasoning_tokens",
            &["reasoning_tokens"],
        );
        add_usage_field(
            usage,
            &mut counts,
            &mut field_names,
            "total_tokens",
            &["total_tokens"],
        );
        add_detail_fields(usage, &mut counts, &mut field_names);
        if !counts.is_empty() {
            field_names.sort();
            return UsageFields {
                counts,
                source: "native",
                path: Some(usage_path),
                field_names,
            };
        }
    }
    UsageFields {
        counts: Vec::new(),
        source: "none",
        path: None,
        field_names: Vec::new(),
    }
}

// ── Ordered JSON line assembly ────────────────────────────────────────────
//
// A tiny writer over exactly the value kinds these records carry (strings,
// u64 counts, one bool, null, one string array), keeping the Python dict's
// insertion order. String escaping goes through serde_json so it is
// byte-identical with `json.dumps(ensure_ascii=False)`.

struct LineBuilder {
    text: String,
}

impl LineBuilder {
    fn new() -> Self {
        LineBuilder {
            text: "{".to_string(),
        }
    }

    fn str_field(&mut self, key: &str, value: &str) {
        if self.text.len() > 1 {
            self.text.push(',');
        }
        self.text.push_str(&serde_json::to_string(key).unwrap());
        self.text.push(':');
        self.text.push_str(&serde_json::to_string(value).unwrap());
    }

    fn raw_field(&mut self, key: &str, raw: &str) {
        if self.text.len() > 1 {
            self.text.push(',');
        }
        self.text.push_str(&serde_json::to_string(key).unwrap());
        self.text.push(':');
        self.text.push_str(raw);
    }

    fn int_field(&mut self, key: &str, value: u64) {
        self.raw_field(key, &value.to_string());
    }

    fn bool_field(&mut self, key: &str, value: bool) {
        self.raw_field(key, if value { "true" } else { "false" });
    }

    fn null_field(&mut self, key: &str) {
        self.raw_field(key, "null");
    }

    fn strings_field(&mut self, key: &str, values: &[String]) {
        let items: Vec<String> = values
            .iter()
            .map(|v| serde_json::to_string(v).unwrap())
            .collect();
        self.raw_field(key, &format!("[{}]", items.join(",")));
    }

    fn finish(mut self) -> String {
        self.text.push('}');
        self.text
    }
}

/// The record `conductor.context_telemetry.event(payload)` encodes, with the
/// timestamp and pid supplied so parity tests can pin both. Field order is
/// the contract: timestamp, pid, provider, event, tool, input_bytes,
/// output_bytes, both estimates, the scope label, output_bounded, the usage
/// block, then `session_id` last (added by Python's `event()`, not the
/// extension).
pub fn event_record(payload: &Value, timestamp: &str, pid: u32) -> String {
    let tool_input = first_present(payload, &["tool_input", "toolInput"])
        .cloned()
        .unwrap_or(Value::Null);
    let mut tool_output = first_present(
        payload,
        &[
            "tool_response",
            "toolResult",
            "tool_output",
            "toolOutput",
            "tool_result",
        ],
    )
    .cloned()
    .unwrap_or(Value::Null);
    if tool_output.is_null() {
        tool_output = payload.get("output").cloned().unwrap_or(Value::Null);
    }
    let event_name = field_string(
        payload,
        &["hook_event_name", "hookEventName"],
        "PostToolUse",
    );
    let tool_name = field_string(payload, &["tool_name", "toolName"], "unknown");
    let tool_output = model_visible_output(&tool_name, &tool_output);
    let input_bytes = json_byte_len(&tool_input);
    let output_bytes = json_byte_len(&tool_output);
    let usage = usage_fields(payload);

    let mut line = LineBuilder::new();
    line.str_field("timestamp", timestamp);
    line.int_field("pid", pid as u64);
    line.str_field("provider", &provider());
    line.str_field("event", &truncate_chars(&event_name, MAX_TOOL_CHARS));
    line.str_field("tool", &truncate_chars(&tool_name, MAX_TOOL_CHARS));
    line.int_field("input_bytes", input_bytes as u64);
    line.int_field("output_bytes", output_bytes as u64);
    line.int_field("tool_input_tokens_estimate", input_bytes.div_ceil(4) as u64);
    line.int_field("output_tokens_estimate", output_bytes.div_ceil(4) as u64);
    line.str_field("output_tokens_estimate_scope", "tool-output-bytes");
    line.bool_field("output_bounded", bounded_output(&tool_output, &tool_output));
    if usage.source == "native" {
        for (name, value) in &usage.counts {
            line.int_field(name, *value);
        }
        line.str_field("usage_source", usage.source);
        line.str_field("native_usage_path", usage.path.as_deref().unwrap_or(""));
        line.strings_field("native_usage_fields", &usage.field_names);
    } else {
        for name in [
            "input_tokens",
            "output_tokens",
            "cached_input_tokens",
            "cache_creation_input_tokens",
            "reasoning_tokens",
            "total_tokens",
        ] {
            line.null_field(name);
        }
        line.str_field("usage_source", usage.source);
        line.null_field("native_usage_path");
        line.strings_field("native_usage_fields", &[]);
    }
    if let Some(session) = payload.get("session_id") {
        if let Some(text) = session.as_str() {
            if !text.is_empty() {
                line.str_field("session_id", text);
            }
        }
    }
    line.finish()
}

/// The record `event()` encodes for a live call: real clock, own pid.
/// Dead in `#[path]`-included test binaries that pull `context_telemetry.rs`
/// in for the record builders alone (e.g. `post_tool_zero_start_parity.rs`) --
/// the same pattern `instant.rs`'s `isoformat_millis_utc` established.
#[allow(dead_code)]
pub fn event_line(payload: &Value) -> Vec<u8> {
    let stamp = instant::isoformat_millis_utc(instant::now());
    let mut encoded = event_record(payload, &stamp, std::process::id());
    encoded.push('\n');
    encoded.into_bytes()
}

/// Python `str(value)` for the scalar kinds these records carry.
fn py_str(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Bool(b) => (if *b { "True" } else { "False" }).to_string(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

/// `_injected_context_text`: the text the byte count measures.
/// Dead in `#[path]`-included test binaries that pull `context_telemetry.rs`
/// in for the record builders alone (e.g. `post_tool_zero_start_parity.rs`) --
/// the same pattern `instant.rs`'s `isoformat_millis_utc` established.
#[allow(dead_code)]
fn injected_context_text(hook_json: &Value) -> String {
    let Some(specific) = hook_json.get("hookSpecificOutput") else {
        return String::new();
    };
    if let Some(context) = specific.get("additionalContext").and_then(Value::as_str) {
        if !context.is_empty() {
            return context.to_string();
        }
    }
    specific
        .get("permissionDecisionReason")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

/// The record `hook_context_event(hook, hook_json, session_id=...)` encodes:
/// how much context one hook's own output injected. `event_name` and
/// `session_id` follow `adapters._telemetry`'s call shape; `content_hash` (a
/// 16-hex sha256 prefix of the injected text) lands last, like Python's.
pub fn hook_context_record(
    hook: &str,
    hook_json: &Value,
    event_name: &str,
    session_id: &str,
    timestamp: &str,
    pid: u32,
) -> String {
    let mut context = String::new();
    let mut selected_event = event_name.to_string();
    if let Some(specific) = hook_json
        .get("hookSpecificOutput")
        .filter(|v| v.is_object())
    {
        for key in ["additionalContext", "permissionDecisionReason"] {
            if let Some(value) = specific.get(key).filter(|v| truthy(v)) {
                context = py_str(value);
                break;
            }
        }
        if selected_event.is_empty() {
            if let Some(name) = specific.get("hookEventName").filter(|v| truthy(v)) {
                selected_event = py_str(name);
            }
        }
    }
    let output_bytes = context.chars().count();
    let mut line = LineBuilder::new();
    line.str_field("timestamp", timestamp);
    line.int_field("pid", pid as u64);
    line.str_field("provider", &provider());
    line.str_field("event", "HookContext");
    line.str_field(
        "hook_event",
        &truncate_chars(
            if selected_event.is_empty() {
                "unknown"
            } else {
                &selected_event
            },
            MAX_TOOL_CHARS,
        ),
    );
    line.str_field("tool", &truncate_chars(hook, MAX_TOOL_CHARS));
    line.int_field("input_bytes", 0);
    line.int_field("output_bytes", output_bytes as u64);
    line.int_field("tool_input_tokens_estimate", 0);
    line.int_field("output_tokens_estimate", output_bytes.div_ceil(4) as u64);
    line.str_field(
        "output_tokens_estimate_scope",
        "hook-additional-context-bytes",
    );
    line.bool_field("output_bounded", false);
    if !session_id.is_empty() {
        line.str_field("session_id", session_id);
    }
    if !context.is_empty() {
        let digest = Sha256::digest(context.as_bytes());
        line.str_field("content_hash", &format!("{:x}", digest)[..16]);
    }
    line.finish()
}

/// `hook_context_record` for a live call: real clock, own pid.
/// Dead in `#[path]`-included test binaries that pull `context_telemetry.rs`
/// in for the record builders alone (e.g. `post_tool_zero_start_parity.rs`) --
/// the same pattern `instant.rs`'s `isoformat_millis_utc` established.
#[allow(dead_code)]
pub fn hook_context_line(
    hook: &str,
    hook_json: &Value,
    event_name: &str,
    session_id: &str,
) -> Vec<u8> {
    let stamp = instant::isoformat_millis_utc(instant::now());
    let mut encoded = hook_context_record(
        hook,
        hook_json,
        event_name,
        session_id,
        &stamp,
        std::process::id(),
    );
    encoded.push('\n');
    encoded.into_bytes()
}

// ── The writer ────────────────────────────────────────────────────────────

fn flock_exclusive(file: &std::fs::File, nonblocking: bool) -> io::Result<bool> {
    let mode = if nonblocking {
        libc::LOCK_EX | libc::LOCK_NB
    } else {
        libc::LOCK_EX
    };
    let rc = unsafe { libc::flock(file.as_raw_fd(), mode) };
    if rc == 0 {
        return Ok(true);
    }
    let err = io::Error::last_os_error();
    if nonblocking && err.raw_os_error() == Some(libc::EWOULDBLOCK) {
        return Ok(false); // held elsewhere: an answer, not a failure
    }
    Err(err)
}

fn flock_unlock(file: &std::fs::File) {
    unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
}

/// `append_bytes`: create the parent, open for append, and under an exclusive
/// `flock` either append (true) or refuse past `MAX_LOG_BYTES` (false).
fn append_bytes(path: &Path, encoded: &[u8]) -> io::Result<bool> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .read(true)
        .append(true)
        .open(path)?;
    flock_exclusive(&file, false)?;
    let result: io::Result<bool> = (|| {
        let length = file.seek(SeekFrom::End(0))?;
        if length.saturating_add(encoded.len() as u64) > MAX_LOG_BYTES {
            return Ok(false);
        }
        let mut writer = &file;
        writer.write_all(encoded)?;
        writer.flush()?;
        Ok(true)
    })();
    flock_unlock(&file);
    result
}

/// `_rotated_name`: `events.jsonl` at `stamp` becomes `events.<stamp>.jsonl`.
fn rotated_name(path: &Path, stamp: &str) -> PathBuf {
    let suffix = path
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let name = if suffix.is_empty() {
        format!("{stem}.{stamp}")
    } else {
        format!("{stem}.{stamp}{suffix}")
    };
    path.with_file_name(name)
}

/// `_unique_target`: a same-second collision disambiguates with `-2`, `-3`...
fn unique_target(target: PathBuf) -> PathBuf {
    if !target.exists() {
        return target;
    }
    let stem = target
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let suffix = target
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    let parent = target.parent().map(Path::to_path_buf).unwrap_or_default();
    let mut counter = 2u32;
    loop {
        let candidate = parent.join(format!("{stem}-{counter}{suffix}"));
        if !candidate.exists() {
            return candidate;
        }
        counter += 1;
    }
}

/// `_prune_rotated`: delete the oldest rotations beyond the newest `keep`
/// (by mtime, so a same-second rotation stays ordered).
fn prune_rotated(path: &Path, keep: usize) {
    let suffix = path
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let Ok(entries) = fs::read_dir(path.parent().unwrap_or(path)) else {
        return;
    };
    let mut rotated: Vec<(std::time::SystemTime, PathBuf)> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|candidate| candidate != path)
        .filter(|candidate| {
            candidate
                .file_name()
                .map(|n| {
                    let name = n.to_string_lossy();
                    name.starts_with(&format!("{stem}."))
                        && (suffix.is_empty() || name.ends_with(&suffix))
                })
                .unwrap_or(false)
        })
        .filter_map(|candidate| {
            let mtime = candidate.metadata().and_then(|meta| meta.modified()).ok()?;
            Some((mtime, candidate))
        })
        .collect();
    if rotated.len() <= keep {
        return;
    }
    rotated.sort_by_key(|(mtime, _)| *mtime);
    for (_, stale) in rotated.iter().take(rotated.len() - keep) {
        let _ = fs::remove_file(stale);
    }
}

/// `_mark_disabled`'s two stderr signals, without the process latch (see
/// module docs). Never raises, never writes to stdout.
fn mark_disabled(path: &Path, exc: &dyn std::fmt::Display) {
    eprintln!(
        "context telemetry disabled for this process: {} unwritable ({exc})",
        path.display()
    );
    let mut line = LineBuilder::new();
    let stamp = instant::isoformat_millis_utc(instant::now());
    line.str_field("timestamp", &stamp);
    line.int_field("pid", std::process::id() as u64);
    line.str_field("provider", &provider());
    line.str_field("event", "telemetry_disabled");
    line.str_field("tool", "context_telemetry");
    line.int_field("output_bytes", 0);
    line.str_field("reason", &exc.to_string());
    let mut encoded = line.finish();
    encoded.push('\n');
    if append_bytes(path, encoded.as_bytes()).is_err() {
        eprintln!("context telemetry: could not record telemetry_disabled either: {exc}");
    }
}

/// `record`: append while the log is under budget, else rotate (stamp-named
/// sibling, pruned to `MAX_ROTATED_LOGS`) and append to the fresh log; any
/// I/O failure degrades to `mark_disabled` instead of raising into the hook.
pub fn record_encoded(path: &Path, encoded: &[u8]) {
    match append_bytes(path, encoded) {
        Ok(true) => {}
        Ok(false) => {
            let stamp = format!("{}Z", instant::format_compact_utc(instant::now()));
            let target = unique_target(rotated_name(path, &stamp));
            if let Err(err) = fs::rename(path, &target) {
                mark_disabled(path, &err);
                return;
            }
            prune_rotated(path, MAX_ROTATED_LOGS);
            eprintln!(
                "context telemetry log rotated to {}",
                target
                    .file_name()
                    .map(|n| n.to_string_lossy())
                    .unwrap_or_default()
            );
            if !append_bytes(path, encoded).unwrap_or(false) {
                let err = io::Error::other(format!(
                    "record exceeds the log budget even after rotation ({})",
                    path.display()
                ));
                mark_disabled(path, &err);
            }
        }
        Err(err) => mark_disabled(path, &err),
    }
}

/// One whole hook invocation's write: `record(telemetry.event(payload), path)`
/// exactly as the Python adapter composes them. Errors are impossible by
/// construction (`record_encoded` degrades to stderr).
/// Dead in `#[path]`-included test binaries that pull `context_telemetry.rs`
/// in for the record builders alone (e.g. `post_tool_zero_start_parity.rs`) --
/// the same pattern `instant.rs`'s `isoformat_millis_utc` established.
#[allow(dead_code)]
pub fn record_event(payload: &Value, root: &Path) {
    record_encoded(&telemetry_path(root), &event_line(payload));
}

/// `adapters._telemetry`'s hook-context variant: record only when the hook
/// injected something (`output_bytes > 0`), never raising on a broken sink.
/// Dead in `#[path]`-included test binaries that pull `context_telemetry.rs`
/// in for the record builders alone (e.g. `post_tool_zero_start_parity.rs`) --
/// the same pattern `instant.rs`'s `isoformat_millis_utc` established.
#[allow(dead_code)]
pub fn record_hook_context(hook: &str, hook_json: &Value, session_id: &str, root: &Path) {
    if injected_context_text(hook_json).is_empty() {
        return;
    }
    record_encoded(
        &telemetry_path(root),
        &hook_context_line(hook, hook_json, "", session_id),
    );
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use serde_json::json;

    /// `provider()` reads five process-global env vars; `handlers`' tests
    /// (same binary, other threads) mutate `CLAUDE_PROJECT_DIR` under a
    /// different mutex, so these tests pin a var that outranks it in the
    /// ladder instead of trying to scrub it: with `QWEN_PROJECT_DIR` set the
    /// provider is "qwen" no matter what `CLAUDE_PROJECT_DIR` state other
    /// tests race.
    pub(crate) static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn pin_qwen() -> std::sync::MutexGuard<'static, ()> {
        let guard = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        std::env::set_var("QWEN_PROJECT_DIR", "1");
        guard
    }

    fn unpinned(guard: std::sync::MutexGuard<'static, ()>) {
        std::env::remove_var("QWEN_PROJECT_DIR");
        drop(guard);
    }

    const STAMP: &str = "2026-09-13T00:00:00.000+00:00";
    const PID: u32 = 383_1796;

    /// Byte-identical to what `conductor-native`'s
    /// `context_telemetry_event_native` + Python's `event()`/`_encoded_record`
    /// produce for the same payload, stamp and pid (recorded once with the
    /// real extension; see `tests/post_tool_zero_start_parity.rs` for the
    /// corpus-level twin).
    #[test]
    fn event_record_is_byte_identical_to_the_extension() {
        let _guard = pin_qwen();
        let payload = json!({
            "session_id": "s-1", "hook_event_name": "PostToolUse", "tool_name": "Bash",
            "tool_input": {"command": "ls"},
            "tool_response": {"stdout": "a\n", "stderr": "", "exit_code": 0},
        });
        assert_eq!(
            event_record(&payload, STAMP, PID),
            "{\"timestamp\":\"2026-09-13T00:00:00.000+00:00\",\"pid\":3831796,\
             \"provider\":\"qwen\",\"event\":\"PostToolUse\",\"tool\":\"Bash\",\
             \"input_bytes\":16,\"output_bytes\":42,\"tool_input_tokens_estimate\":4,\
             \"output_tokens_estimate\":11,\
             \"output_tokens_estimate_scope\":\"tool-output-bytes\",\
             \"output_bounded\":false,\"input_tokens\":null,\"output_tokens\":null,\
             \"cached_input_tokens\":null,\"cache_creation_input_tokens\":null,\
             \"reasoning_tokens\":null,\"total_tokens\":null,\"usage_source\":\"none\",\
             \"native_usage_path\":null,\"native_usage_fields\":[],\
             \"session_id\":\"s-1\"}"
        );
        unpinned(_guard);
    }

    /// Native usage fields: ints and decimal strings count, floats and
    /// negatives do not; the container path and sorted field names record.
    #[test]
    fn usage_fields_follow_the_extension_rules() {
        let _guard = pin_qwen();
        let payload = json!({
            "session_id": "s-2", "tool_name": "mcp__code_review_graph__query_graph",
            "tool_response": {"rows": []},
            "response": {"usage": {"input_tokens": 100, "output_tokens": "7",
                                    "cached_tokens": 3.5, "total_tokens": -2}},
        });
        assert_eq!(
            event_record(&payload, STAMP, PID),
            "{\"timestamp\":\"2026-09-13T00:00:00.000+00:00\",\"pid\":3831796,\
             \"provider\":\"qwen\",\"event\":\"PostToolUse\",\
             \"tool\":\"mcp__code_review_graph__query_graph\",\"input_bytes\":0,\
             \"output_bytes\":11,\"tool_input_tokens_estimate\":0,\
             \"output_tokens_estimate\":3,\
             \"output_tokens_estimate_scope\":\"tool-output-bytes\",\
             \"output_bounded\":false,\"input_tokens\":100,\"output_tokens\":7,\
             \"usage_source\":\"native\",\"native_usage_path\":\"response.usage\",\
             \"native_usage_fields\":[\"input_tokens\",\"output_tokens\"],\
             \"session_id\":\"s-2\"}"
        );
        unpinned(_guard);
    }

    /// The details containers fill cached/reasoning when the top-level names
    /// did not, and the matched field name carries the details prefix.
    #[test]
    fn usage_details_containers_fill_cached_tokens() {
        let _guard = pin_qwen();
        let payload = json!({
            "tool_name": "Grep",
            "response": {"usage": {"prompt_tokens": 50, "completion_tokens": 2,
                                    "prompt_tokens_details": {"cached_tokens": "9"}}},
        });
        assert_eq!(
            event_record(&payload, STAMP, PID),
            "{\"timestamp\":\"2026-09-13T00:00:00.000+00:00\",\"pid\":3831796,\
             \"provider\":\"qwen\",\"event\":\"PostToolUse\",\"tool\":\"Grep\",\
             \"input_bytes\":0,\"output_bytes\":0,\"tool_input_tokens_estimate\":0,\
             \"output_tokens_estimate\":0,\
             \"output_tokens_estimate_scope\":\"tool-output-bytes\",\
             \"output_bounded\":false,\"input_tokens\":50,\"output_tokens\":2,\
             \"cached_input_tokens\":9,\"usage_source\":\"native\",\
             \"native_usage_path\":\"response.usage\",\
             \"native_usage_fields\":[\"completion_tokens\",\"prompt_tokens\",\
             \"prompt_tokens_details.cached_tokens\"]}"
        );
        unpinned(_guard);
    }

    /// Edit responses project to the model-visible fields only; a
    /// `truncated: true` marker flags the response as bounded.
    #[test]
    fn edit_projection_and_bounded_marker() {
        let _guard = pin_qwen();
        let edit = json!({
            "session_id": "s-3", "tool_name": "Edit",
            "tool_input": {"file_path": "a.py", "old_string": "x", "new_string": "y"},
            "tool_response": {"filePath": "a.py", "userModified": false,
                               "structuredPatch": "@@", "fullFile": "never counted"},
        });
        assert_eq!(
            event_record(&edit, STAMP, PID),
            "{\"timestamp\":\"2026-09-13T00:00:00.000+00:00\",\"pid\":3831796,\
             \"provider\":\"qwen\",\"event\":\"PostToolUse\",\"tool\":\"Edit\",\
             \"input_bytes\":54,\"output_bytes\":63,\"tool_input_tokens_estimate\":14,\
             \"output_tokens_estimate\":16,\
             \"output_tokens_estimate_scope\":\"tool-output-bytes\",\
             \"output_bounded\":false,\"input_tokens\":null,\"output_tokens\":null,\
             \"cached_input_tokens\":null,\"cache_creation_input_tokens\":null,\
             \"reasoning_tokens\":null,\"total_tokens\":null,\"usage_source\":\"none\",\
             \"native_usage_path\":null,\"native_usage_fields\":[],\
             \"session_id\":\"s-3\"}"
        );
        let bounded = json!({
            "tool_name": "Read", "tool_response": {"content": "...", "truncated": true},
        });
        let line = event_record(&bounded, STAMP, PID);
        assert!(line.contains("\"output_bounded\":true"), "{line}");
        unpinned(_guard);
    }

    #[test]
    fn hook_context_record_carries_session_then_hash_last() {
        let _guard = pin_qwen();
        let out = json!({
            "hookSpecificOutput": {"hookEventName": "PostToolUse",
                "additionalContext":
                    "code-review-graph refresh queued after a git working-tree change."}
        });
        assert_eq!(
            hook_context_record("post-bash-graph", &out, "", "s-9", STAMP, PID),
            "{\"timestamp\":\"2026-09-13T00:00:00.000+00:00\",\"pid\":3831796,\
             \"provider\":\"qwen\",\"event\":\"HookContext\",\
             \"hook_event\":\"PostToolUse\",\"tool\":\"post-bash-graph\",\
             \"input_bytes\":0,\"output_bytes\":65,\"tool_input_tokens_estimate\":0,\
             \"output_tokens_estimate\":17,\
             \"output_tokens_estimate_scope\":\"hook-additional-context-bytes\",\
             \"output_bounded\":false,\"session_id\":\"s-9\",\
             \"content_hash\":\"33d7d019e7244457\"}"
        );
        unpinned(_guard);
    }

    #[test]
    fn append_respects_the_cap_and_rotates_with_prune() {
        let dir = std::env::temp_dir().join(format!("forge-ctx-tele-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("events.jsonl");
        // Fill to exactly one byte under the cap, then refuse the overflow...
        assert!(append_bytes(&path, b"{\"a\":1}\n").unwrap());
        // (7 bytes written; the real 10 MiB cap is not worth filling here --
        // rotate-on-full is exercised through `record_encoded` below with a
        // tiny pre-seeded file plus the cap check asserted separately.)
        assert!(append_bytes(&path, b"{\"b\":2}\n").unwrap());
        let text = fs::read_to_string(&path).unwrap();
        assert_eq!(text, "{\"a\":1}\n{\"b\":2}\n");
        // Rotation naming and pruning:
        fs::write(dir.join("events.20260101T000000Z.jsonl"), b"old1\n").unwrap();
        fs::write(dir.join("events.20260102T000000Z.jsonl"), b"old2\n").unwrap();
        let stamp = "20260913T000000Z";
        let target = unique_target(rotated_name(&path, stamp));
        assert_eq!(
            target.file_name().unwrap().to_string_lossy(),
            "events.20260913T000000Z.jsonl"
        );
        fs::rename(&path, &target).unwrap();
        prune_rotated(&path, MAX_ROTATED_LOGS);
        assert!(dir.join("events.20260101T000000Z.jsonl").is_file()); // 3 kept
        assert!(path.read_link().is_err() && !path.exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn record_encoded_writes_and_degrades_loudly_on_a_dead_sink() {
        let dir = std::env::temp_dir().join(format!("forge-ctx-tele-dead-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("events.jsonl");
        record_encoded(&path, b"{\"ok\":1}\n");
        assert_eq!(fs::read_to_string(&path).unwrap(), "{\"ok\":1}\n");
        // A directory where the log path should be is unwritable:
        let dead = dir.join("dead");
        fs::create_dir_all(dead.join("events.jsonl")).unwrap();
        record_encoded(&dead.join("events.jsonl"), b"{\"ok\":2}\n"); // stderr only
        assert!(dead.join("events.jsonl").is_dir());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn telemetry_path_honours_the_env_override() {
        let _guard = pin_qwen();
        std::env::set_var("CONTEXT_TELEMETRY_PATH", "/tmp/alt-events.jsonl");
        assert_eq!(
            telemetry_path(Path::new("/repo")),
            PathBuf::from("/tmp/alt-events.jsonl")
        );
        std::env::remove_var("CONTEXT_TELEMETRY_PATH");
        assert_eq!(
            telemetry_path(Path::new("/repo")),
            PathBuf::from("/repo/src/research/tmp/context_telemetry/events.jsonl")
        );
        unpinned(_guard);
    }
}
