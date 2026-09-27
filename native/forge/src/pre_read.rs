//! Native whole-file Read guard, matching `_pre_read_skeleton.hook_output`.
//! Count newline bytes and total bytes in bounded memory: the denial reports
//! the complete file size, including invalid UTF-8 and an unterminated last line.

use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::fs::File;
use std::io::Read;
use std::path::Path;

const THRESHOLD_LINES: usize = 400;
const CODE_SUFFIXES: &[&str] = &["py", "rs", "c", "cc", "cpp", "h", "hpp", "ts", "js", "sh"];

fn quiet() -> Value {
    json!({"hookSpecificOutput": {"hookEventName": "PreToolUse"}})
}

/// JSON values use Python's truthiness for the input aliases and slice bounds.
pub(crate) fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(value) => value.as_f64() != Some(0.0),
        Value::String(value) => !value.is_empty(),
        Value::Array(value) => !value.is_empty(),
        Value::Object(value) => !value.is_empty(),
    }
}

fn count_file(path: &Path) -> std::io::Result<(usize, usize)> {
    let mut file = File::open(path)?;
    let mut buffer = [0_u8; 64 * 1024];
    let (mut lines, mut bytes) = (0, 0);
    loop {
        let count = match file.read(&mut buffer) {
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            result => result?,
        };
        if count == 0 {
            return Ok((lines, bytes));
        }
        lines += buffer[..count]
            .iter()
            .filter(|&&byte| byte == b'\n')
            .count();
        bytes += count;
    }
}

fn comma(value: usize) -> String {
    let digits = value.to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
}

/// Filesystem failures stay quiet, as the Read tool reports them itself.
/// Malformed truthy input shapes remain visible adapter errors to the merger.
pub fn hook_output(payload: &Value) -> Result<Value> {
    if !payload.is_object() {
        return Ok(quiet());
    }
    let input = payload
        .get("tool_input")
        .filter(|value| truthy(value))
        .or_else(|| payload.get("toolInput").filter(|value| truthy(value)));
    let Some(input) = input else {
        return Ok(quiet());
    };
    let input = input
        .as_object()
        .context("pre_read_skeleton tool input must be an object")?;
    let path = input
        .get("file_path")
        .filter(|value| truthy(value))
        .or_else(|| input.get("path").filter(|value| truthy(value)));
    let Some(path) = path else {
        return Ok(quiet());
    };
    if input.get("offset").is_some_and(truthy) || input.get("limit").is_some_and(truthy) {
        return Ok(quiet());
    }
    let path = Path::new(
        path.as_str()
            .context("pre_read_skeleton file path must be a string")?,
    );
    let suffix = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("");
    if !CODE_SUFFIXES.contains(&suffix) || !path.is_file() {
        return Ok(quiet());
    }
    let Ok((lines, bytes)) = count_file(path) else {
        return Ok(quiet());
    };
    if lines < THRESHOLD_LINES {
        return Ok(quiet());
    }
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let tokens = comma(bytes / 4);
    let symbol = if suffix == "py" {
        "symbol=<name>"
    } else {
        "no symbol"
    };
    let reason = format!(
        "PRE-READ DENIED: {name} is {lines} lines (~{tokens} tokens); whole-file \
         Read of a code file >= {THRESHOLD_LINES} lines is not allowed (KB-OPS-CTX-01). \
         Use mcp__code-review-graph__ast_context_tool(file_path=..., {symbol}) for signatures+callers, \
         symbol_source_tool for one definition, query_graph(file_summary) for the node \
         list, or Read with offset/limit for the slice you will edit."
    );
    Ok(json!({"hookSpecificOutput": {
        "hookEventName": "PreToolUse",
        "permissionDecision": "deny",
        "permissionDecisionReason": reason,
    }}))
}
