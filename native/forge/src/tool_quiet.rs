//! `PostToolUse` output bounding: the native port of
//! `src/tooling/hooks/claude/_bash_quiet.py` and `.../post_tool_quiet.py`.
//!
//! Both Python hooks share one contract: a `tool_response` field over a byte
//! cap is rewritten to head + an elision marker + tail, and the elided middle
//! either names a spill file (Bash `stdout`/`stderr`/`output`, Grep strings,
//! MCP text blocks) or a `Read(offset=..., limit=...)` resume point (Read's
//! `file.content`, never spilled -- the file is already on disk). This module
//! is that one bounding core; `handlers::PostBashQuiet` and
//! `handlers::PostToolQuiet` each wire it to their own response shape and
//! byte cap, exactly as `_bash_quiet.bound_response` and
//! `post_tool_quiet.bound_response` do today.
//!
//! Byte-for-byte parity with Python (including the spill filename, which
//! embeds a caller-supplied timestamp) is pinned by
//! `native/forge/tests/tool_quiet_parity.rs` against the frozen fixtures
//! under `src/tooling/hooks/claude/fixtures/`. Known gap: Python's
//! `str.encode("utf-8", "surrogateescape")` lets a lone UTF-16 surrogate from
//! a malformed `\uXXXX` JSON escape round-trip through the byte-length check;
//! `serde_json` replaces such an escape with U+FFFD during parsing, so a
//! payload built entirely around that edge case can disagree on byte counts.
//! No case in the corpus exercises it, and it is reported here rather than
//! hand-patched.

use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

pub const HEAD_LINES: usize = 60;
pub const TAIL_LINES: usize = 30;
pub const BASH_QUIET_LIMIT_DEFAULT: usize = 8000;
pub const TOOL_OUTPUT_QUIET_DEFAULT: usize = 16000;
const TEXT_FIELDS: [&str; 3] = ["stdout", "stderr", "output"];
const DEFAULT_OUTPUT_FIELD: &str = "updatedToolOutput";

/// Everything the bounding core needs beyond the bytes and the cap: where a
/// spilled file is written, what a spill path is displayed relative to, the
/// timestamp embedded in a spill filename, and which envelope key a rewrite
/// lands under (Claude Code's `updatedToolOutput` vs. Codex's
/// `updatedMCPToolOutput`, `_bash_quiet.OUTPUT_FIELD`'s job).
pub struct QuietConfig<'a> {
    pub save_dir: &'a Path,
    pub repo_root: &'a Path,
    pub now_stamp: &'a str,
    pub output_field: &'a str,
}

impl<'a> QuietConfig<'a> {
    pub fn output_field_or_default(&self) -> &str {
        if self.output_field.is_empty() {
            DEFAULT_OUTPUT_FIELD
        } else {
            self.output_field
        }
    }
}

/// `bytes.splitlines(keepends=True)` for exactly the boundary bytes CPython
/// recognizes on a `bytes` object: `\n`, `\r`, `\r\n`, `\v`, `\f`, `\x1c`,
/// `\x1d`, `\x1e`. Unlike `str.splitlines`, `\x85` and the Unicode line
/// separators are NOT boundaries here -- those only apply to decoded text,
/// and `_bash_quiet.split_head_tail` always operates on encoded bytes.
fn splitlines_keepends(data: &[u8]) -> Vec<&[u8]> {
    let mut lines = Vec::new();
    let mut start = 0usize;
    let mut i = 0usize;
    let n = data.len();
    while i < n {
        match data[i] {
            0x0A | 0x0B | 0x0C | 0x1C | 0x1D | 0x1E => {
                lines.push(&data[start..=i]);
                i += 1;
                start = i;
            }
            0x0D => {
                if i + 1 < n && data[i + 1] == 0x0A {
                    lines.push(&data[start..=i + 1]);
                    i += 2;
                } else {
                    lines.push(&data[start..=i]);
                    i += 1;
                }
                start = i;
            }
            _ => i += 1,
        }
    }
    if start < n {
        lines.push(&data[start..n]);
    }
    lines
}

/// Python's `data[-(k):]`: `k == 0` (from `-0`) or `k >= len` both mean "the
/// whole slice", never empty -- there is no negative-index case in Rust's
/// `usize` world, so this just names Python's own quirk explicitly.
fn py_tail_slice(data: &[u8], k: usize) -> &[u8] {
    if k == 0 || k >= data.len() {
        data
    } else {
        &data[data.len() - k..]
    }
}

/// Mirrors `_bash_quiet.split_head_tail`: a line-count split first
/// (`HEAD_LINES` + `TAIL_LINES` whole lines); a response of few but very long
/// lines falls back to a byte split. Returns
/// `(head, tail, elided_bytes, elided_lines)`.
pub fn split_head_tail(data: &[u8], limit_bytes: usize) -> (Vec<u8>, Vec<u8>, usize, usize) {
    let lines = splitlines_keepends(data);
    if lines.len() <= HEAD_LINES + TAIL_LINES {
        let head_len = (limit_bytes / 2).min(data.len());
        let head = &data[..head_len];
        let tail = py_tail_slice(data, limit_bytes / 4);
        let elided = data.len().saturating_sub(head.len() + tail.len());
        return (head.to_vec(), tail.to_vec(), elided, 0);
    }
    let head: Vec<u8> = lines[..HEAD_LINES].concat();
    let tail: Vec<u8> = lines[lines.len() - TAIL_LINES..].concat();
    let elided_bytes = data.len().saturating_sub(head.len() + tail.len());
    let elided_lines = lines.len() - HEAD_LINES - TAIL_LINES;
    (head, tail, elided_bytes, elided_lines)
}

/// `f"{n:,}"`: thousands-grouped decimal, the only numeric format
/// `_bash_quiet`'s markers use.
fn comma(n: usize) -> String {
    let digits = n.to_string();
    let bytes = digits.as_bytes();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 && (bytes.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(*b as char);
    }
    out
}

/// `_bash_quiet._save`: writes `data` under `save_dir` as
/// `{now_stamp}-{sha256(data)[:10]}.txt`, creating `save_dir` if needed.
fn save(data: &[u8], save_dir: &Path, now_stamp: &str) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(save_dir)?;
    let digest = Sha256::digest(data);
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    let path = save_dir.join(format!("{now_stamp}-{}.txt", &hex[..10]));
    std::fs::write(&path, data)?;
    Ok(path)
}

/// `saved.relative_to(REPO_ROOT) if saved.is_relative_to(REPO_ROOT) else saved`,
/// rendered as `_bash_quiet` renders a `Path` -- POSIX `/` separators.
fn where_display(saved: &Path, repo_root: &Path) -> String {
    let shown = saved.strip_prefix(repo_root).unwrap_or(saved);
    shown.to_string_lossy().replace('\\', "/")
}

/// `_bash_quiet.bound`: unchanged when `data` fits in `limit_bytes`; else
/// head + marker + tail, having spilled the full `data` to `cfg.save_dir`.
pub fn bound(data: &[u8], limit_bytes: usize, cfg: &QuietConfig) -> Vec<u8> {
    if data.len() <= limit_bytes {
        return data.to_vec();
    }
    let saved = save(data, cfg.save_dir, cfg.now_stamp).expect("tool_quiet spill write");
    let where_str = where_display(&saved, cfg.repo_root);
    let (head, tail, elided_bytes, elided_lines) = split_head_tail(data, limit_bytes);
    let marker = if elided_lines == 0 {
        format!(
            "\n... [elided {} bytes; full output: {where_str}] ...\n",
            comma(elided_bytes)
        )
    } else {
        format!(
            "... [elided {} lines / {} KB; full output: {where_str}] ...\n",
            comma(elided_lines),
            data.len() / 1024
        )
    };
    let mut out = head;
    out.extend_from_slice(marker.as_bytes());
    out.extend_from_slice(&tail);
    out
}

fn utf8_lossy(bytes: Vec<u8>) -> String {
    String::from_utf8_lossy(&bytes).into_owned()
}

/// `_bash_quiet.bound_response`: bounds a Bash tool response, a plain string
/// or a dict with `stdout`/`stderr`/`output` text fields. `None` means
/// nothing exceeded `limit_bytes` -- no rewrite.
pub fn bound_bash_response(
    response: &Value,
    limit_bytes: usize,
    cfg: &QuietConfig,
) -> Option<Value> {
    match response {
        Value::String(s) => {
            let data = s.as_bytes();
            if data.len() <= limit_bytes {
                return None;
            }
            Some(Value::String(utf8_lossy(bound(data, limit_bytes, cfg))))
        }
        Value::Object(map) => {
            let mut updated = map.clone();
            let mut changed = false;
            for key in TEXT_FIELDS {
                if let Some(Value::String(s)) = map.get(key) {
                    if s.len() > limit_bytes {
                        updated.insert(
                            key.to_string(),
                            Value::String(utf8_lossy(bound(s.as_bytes(), limit_bytes, cfg))),
                        );
                        changed = true;
                    }
                }
            }
            if changed {
                Some(Value::Object(updated))
            } else {
                None
            }
        }
        _ => None,
    }
}

/// `post_tool_quiet._bounded_with_spill`: the Bash treatment applied to a
/// Grep string or one MCP text block -- head + marker naming the spill path
/// + tail.
fn bounded_with_spill(text: &str, cap: usize, cfg: &QuietConfig) -> String {
    utf8_lossy(bound(text.as_bytes(), cap, cfg))
}

/// `post_tool_quiet._bounded_read`: `file.content` bounded with a resume
/// pointer instead of a spill -- the file is already on disk.
fn bounded_read(content: &str, cap: usize) -> String {
    let data = content.as_bytes();
    let (head, tail, elided, _) = split_head_tail(data, cap);
    let cut = head.len();
    let resume_line = head.iter().filter(|b| **b == b'\n').count() + 1;
    let marker = format!(
        "\n... [elided {} bytes at byte {} (line {}); read the rest with Read(offset={resume_line}, limit=...)] ...\n",
        comma(elided),
        comma(cut),
        comma(resume_line),
    );
    let mut out = head;
    out.extend_from_slice(marker.as_bytes());
    out.extend_from_slice(&tail);
    utf8_lossy(out)
}

/// Outcome of bounding a Read/Grep/MCP `tool_response`, mirroring
/// `post_tool_quiet.bound_response`'s three-way return: no rewrite needed,
/// a rewritten value, or a shape the hook does not recognize (which still
/// passes through unbounded, but warns).
pub enum ToolOutcome {
    NoChange,
    Updated(Value),
    Unrecognized(&'static str),
}

fn json_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(n) => {
            if n.is_i64() || n.is_u64() {
                "int"
            } else {
                "float"
            }
        }
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

/// `post_tool_quiet.bound_response`. `cap <= 0` (the `TOOL_OUTPUT_QUIET_BYTES=0`
/// escape hatch) always returns `NoChange` before inspecting the shape at all.
pub fn bound_tool_response(
    response: &Value,
    cap: usize,
    cap_disabled: bool,
    cfg: &QuietConfig,
) -> ToolOutcome {
    if cap_disabled {
        return ToolOutcome::NoChange;
    }
    match response {
        Value::String(s) => {
            if s.len() <= cap {
                ToolOutcome::NoChange
            } else {
                ToolOutcome::Updated(Value::String(bounded_with_spill(s, cap, cfg)))
            }
        }
        Value::Object(map) => bound_tool_object(map, cap, cfg),
        Value::Array(items) => bound_tool_blocks(items, cap, cfg),
        other => ToolOutcome::Unrecognized(json_type_name(other)),
    }
}

fn bound_tool_object(map: &Map<String, Value>, cap: usize, cfg: &QuietConfig) -> ToolOutcome {
    if let Some(Value::Object(file_field)) = map.get("file") {
        if let Some(Value::String(content)) = file_field.get("content") {
            let type_ok = matches!(map.get("type"), None | Some(Value::Null))
                || map.get("type") == Some(&Value::String("text".to_string()));
            if !type_ok {
                return ToolOutcome::Unrecognized(json_type_name(
                    map.get("type").unwrap_or(&Value::Null),
                ));
            }
            if content.len() <= cap {
                return ToolOutcome::NoChange;
            }
            let mut new_file = file_field.clone();
            new_file.insert(
                "content".to_string(),
                Value::String(bounded_read(content, cap)),
            );
            let mut new_map = map.clone();
            new_map.insert("file".to_string(), Value::Object(new_file));
            return ToolOutcome::Updated(Value::Object(new_map));
        }
    }
    if let Some(Value::String(text)) = map.get("text") {
        if text.len() <= cap {
            return ToolOutcome::NoChange;
        }
        let mut new_map = map.clone();
        new_map.insert(
            "text".to_string(),
            Value::String(bounded_with_spill(text, cap, cfg)),
        );
        return ToolOutcome::Updated(Value::Object(new_map));
    }
    ToolOutcome::Unrecognized("dict")
}

fn bound_tool_blocks(items: &[Value], cap: usize, cfg: &QuietConfig) -> ToolOutcome {
    for item in items {
        let ok =
            matches!(item, Value::Object(o) if matches!(o.get("text"), Some(Value::String(_))));
        if !ok {
            return ToolOutcome::Unrecognized("list");
        }
    }
    let mut changed = false;
    let mut updated = Vec::with_capacity(items.len());
    for item in items {
        let Value::Object(block) = item else {
            unreachable!("validated above")
        };
        let Some(Value::String(text)) = block.get("text") else {
            unreachable!("validated above")
        };
        if text.len() > cap {
            let mut new_block = block.clone();
            new_block.insert(
                "text".to_string(),
                Value::String(bounded_with_spill(text, cap, cfg)),
            );
            updated.push(Value::Object(new_block));
            changed = true;
        } else {
            updated.push(item.clone());
        }
    }
    if changed {
        ToolOutcome::Updated(Value::Array(updated))
    } else {
        ToolOutcome::NoChange
    }
}

/// `_bash_quiet.rewrite_envelope`: the shared `PostToolUse` envelope both
/// quiet hooks return. `payload` must be a JSON object with a
/// `tool_response` field; anything else yields the bare envelope, no rewrite.
pub fn rewrite_envelope_bash(payload: &Value, limit_bytes: usize, cfg: &QuietConfig) -> Value {
    let mut out = base_envelope();
    let Value::Object(payload_map) = payload else {
        return out;
    };
    let Some(response) = payload_map.get("tool_response") else {
        return out;
    };
    if let Some(updated) = bound_bash_response(response, limit_bytes, cfg) {
        insert_output(&mut out, cfg, updated);
    }
    out
}

/// The `post_tool_quiet` twin of `rewrite_envelope_bash`. Returns the
/// envelope and, when the response shape was not recognized, the Python type
/// name a caller should warn about (matching
/// `post_tool_quiet._warn_unrecognized`'s stderr message).
pub fn rewrite_envelope_tool(
    payload: &Value,
    cap: usize,
    cap_disabled: bool,
    cfg: &QuietConfig,
) -> (Value, Option<&'static str>) {
    let mut out = base_envelope();
    let Value::Object(payload_map) = payload else {
        return (out, None);
    };
    let Some(response) = payload_map.get("tool_response") else {
        return (out, None);
    };
    match bound_tool_response(response, cap, cap_disabled, cfg) {
        ToolOutcome::NoChange => (out, None),
        ToolOutcome::Updated(value) => {
            insert_output(&mut out, cfg, value);
            (out, None)
        }
        ToolOutcome::Unrecognized(kind) => (out, Some(kind)),
    }
}

fn base_envelope() -> Value {
    serde_json::json!({"hookSpecificOutput": {"hookEventName": "PostToolUse"}})
}

fn insert_output(envelope: &mut Value, cfg: &QuietConfig, value: Value) {
    if let Value::Object(inner) = envelope
        .get_mut("hookSpecificOutput")
        .expect("base envelope")
    {
        inner.insert(cfg.output_field_or_default().to_string(), value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    /// A private scratch dir, removed on drop -- avoids pulling in a
    /// `tempfile` dev-dependency for a handful of unit tests.
    struct ScratchDir(PathBuf);

    impl ScratchDir {
        fn new() -> Self {
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!("forge-tool-quiet-{}-{n}", std::process::id()));
            std::fs::create_dir_all(&path).unwrap();
            ScratchDir(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for ScratchDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn tempdir() -> ScratchDir {
        ScratchDir::new()
    }

    fn cfg<'a>(dir: &'a Path, root: &'a Path) -> QuietConfig<'a> {
        QuietConfig {
            save_dir: dir,
            repo_root: root,
            now_stamp: "20260101T000000",
            output_field: "updatedToolOutput",
        }
    }

    #[test]
    fn small_bash_output_passes_through_unbounded() {
        let dir = tempdir();
        let c = cfg(dir.path(), dir.path());
        let payload =
            serde_json::json!({"tool_response": {"stdout": "hi", "stderr": "", "output": null}});
        let out = rewrite_envelope_bash(&payload, 8000, &c);
        assert_eq!(
            out,
            serde_json::json!({"hookSpecificOutput": {"hookEventName": "PostToolUse"}})
        );
    }

    #[test]
    fn oversized_bash_stdout_spills_and_points_at_it() {
        let dir = tempdir();
        let c = cfg(dir.path(), dir.path());
        let big = "x".repeat(20_000);
        let payload = serde_json::json!({"tool_response": {"stdout": big.clone()}});
        let out = rewrite_envelope_bash(&payload, 8000, &c);
        let rewritten = out["hookSpecificOutput"]["updatedToolOutput"]["stdout"]
            .as_str()
            .unwrap();
        assert!(rewritten.contains("full output: "));
        let spilled: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
        assert_eq!(spilled.len(), 1);
        let spilled_path = spilled[0].as_ref().unwrap().path();
        assert_eq!(std::fs::read_to_string(spilled_path).unwrap(), big);
    }

    #[test]
    fn cap_zero_disables_bounding() {
        let dir = tempdir();
        let c = cfg(dir.path(), dir.path());
        let payload = serde_json::json!({"tool_response": "x".repeat(20_000)});
        let (out, warned) = rewrite_envelope_tool(&payload, 0, true, &c);
        assert!(warned.is_none());
        assert_eq!(
            out,
            serde_json::json!({"hookSpecificOutput": {"hookEventName": "PostToolUse"}})
        );
    }

    #[test]
    fn malformed_shape_is_reported_and_passed_through() {
        let dir = tempdir();
        let c = cfg(dir.path(), dir.path());
        let payload = serde_json::json!({"tool_response": 5});
        let (out, warned) = rewrite_envelope_tool(&payload, 16000, false, &c);
        assert_eq!(warned, Some("int"));
        assert_eq!(
            out,
            serde_json::json!({"hookSpecificOutput": {"hookEventName": "PostToolUse"}})
        );
    }

    #[test]
    fn read_content_bounded_names_a_resume_offset() {
        let dir = tempdir();
        let c = cfg(dir.path(), dir.path());
        let lines: String = (0..300).map(|i| format!("line {i}\n")).collect();
        let payload =
            serde_json::json!({"tool_response": {"type": "text", "file": {"content": lines}}});
        let (out, warned) = rewrite_envelope_tool(&payload, 100, false, &c);
        assert!(warned.is_none());
        let content = out["hookSpecificOutput"]["updatedToolOutput"]["file"]["content"]
            .as_str()
            .unwrap();
        assert!(content.contains("Read(offset="));
        assert!(std::fs::read_dir(dir.path()).unwrap().next().is_none());
    }

    #[test]
    fn comma_formats_thousands() {
        assert_eq!(comma(0), "0");
        assert_eq!(comma(999), "999");
        assert_eq!(comma(1000), "1,000");
        assert_eq!(comma(1_234_567), "1,234,567");
    }
}
