//! Provider-neutral context telemetry without retaining tool contents.
//!
//! Python owns only argparse, timestamps, and its exact JSON encoder. This module
//! owns normalization, event construction, bounded locked storage, rotation, and
//! NDJSON aggregation.

use std::cmp::Reverse;
use std::collections::HashMap;
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use pyo3::exceptions::{PyOSError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyBool, PyDict, PyInt, PyString};
use serde::Serialize;
use serde_json::{json, Value};

const MAX_PROVIDER_CHARS: usize = 32;
const MAX_TOOL_CHARS: usize = 100;
const USAGE_KEYS: [&str; 4] = ["usage", "usageMetadata", "usage_metadata", "token_usage"];
const USAGE_CONTAINERS: [&str; 2] = ["response", "metadata"];

fn io_error(error: std::io::Error) -> PyErr {
    PyOSError::new_err(error.to_string())
}

fn provider() -> String {
    fn present(name: &str) -> bool {
        env::var_os(name).is_some_and(|value| !value.is_empty())
    }

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

fn first_field<'py>(
    payload: &Bound<'py, PyDict>,
    names: &[&str],
) -> PyResult<Option<Bound<'py, PyAny>>> {
    for name in names {
        if let Some(value) = payload.get_item(name)? {
            return Ok(Some(value));
        }
    }
    Ok(None)
}

fn field_string(payload: &Bound<'_, PyDict>, names: &[&str], fallback: &str) -> PyResult<String> {
    let Some(value) = first_field(payload, names)? else {
        return Ok(fallback.to_owned());
    };
    if !value.is_truthy()? {
        return Ok(fallback.to_owned());
    }
    Ok(value.str()?.to_string_lossy().into_owned())
}

fn mapping_type(py: Python<'_>) -> PyResult<Bound<'_, PyAny>> {
    py.import("collections.abc")?.getattr("Mapping")
}

fn is_mapping(value: &Bound<'_, PyAny>, mapping: &Bound<'_, PyAny>) -> PyResult<bool> {
    value.is_instance(mapping)
}

fn mapping_get<'py>(mapping: &Bound<'py, PyAny>, key: &str) -> PyResult<Bound<'py, PyAny>> {
    mapping.call_method1("get", (key,))
}

fn mapping_has(mapping: &Bound<'_, PyAny>, key: &str) -> PyResult<bool> {
    mapping.call_method1("__contains__", (key,))?.is_truthy()
}

fn json_bytes(py: Python<'_>, value: &Bound<'_, PyAny>) -> Vec<u8> {
    if value.is_none() {
        return Vec::new();
    }
    let encoded = (|| -> PyResult<Vec<u8>> {
        let json_module = py.import("json")?;
        let kwargs = PyDict::new(py);
        kwargs.set_item("ensure_ascii", false)?;
        kwargs.set_item("separators", (",", ":"))?;
        let text = json_module
            .getattr("dumps")?
            .call((value,), Some(&kwargs))?
            .extract::<String>()?;
        Ok(text.into_bytes())
    })();
    encoded.unwrap_or_default()
}

fn model_visible_output<'py>(
    py: Python<'py>,
    tool_name: &str,
    tool_output: Bound<'py, PyAny>,
    mapping: &Bound<'py, PyAny>,
) -> PyResult<Bound<'py, PyAny>> {
    let fields: &[&str] = match tool_name {
        "Edit" => &["filePath", "structuredPatch", "userModified"],
        "Write" => &["type", "filePath"],
        _ => return Ok(tool_output),
    };
    if !is_mapping(&tool_output, mapping)? {
        return Ok(tool_output);
    }
    let projected = PyDict::new(py);
    for field in fields {
        if mapping_has(&tool_output, field)? {
            projected.set_item(field, tool_output.get_item(field)?)?;
        }
    }
    Ok(projected.into_any())
}

#[pyfunction]
fn context_telemetry_model_visible_output_native<'py>(
    py: Python<'py>,
    tool_name: &str,
    tool_output: Bound<'py, PyAny>,
) -> PyResult<Bound<'py, PyAny>> {
    let mapping = mapping_type(py)?;
    model_visible_output(py, tool_name, tool_output, &mapping)
}

fn nonnegative_int(value: &Bound<'_, PyAny>) -> PyResult<Option<Py<PyAny>>> {
    if value.is_none() || value.is_instance_of::<PyBool>() {
        return Ok(None);
    }
    if value.is_instance_of::<PyInt>() {
        if value.lt(0)? {
            return Ok(None);
        }
        return Ok(Some(value.clone().unbind()));
    }
    if value.is_instance_of::<PyString>() && value.call_method0("isdecimal")?.is_truthy()? {
        let builtins = value.py().import("builtins")?;
        return Ok(Some(builtins.getattr("int")?.call1((value,))?.unbind()));
    }
    Ok(None)
}

fn usage_value(usage: &Bound<'_, PyAny>, names: &[&str]) -> PyResult<Option<(Py<PyAny>, String)>> {
    for name in names {
        if let Some(value) = nonnegative_int(&mapping_get(usage, name)?)? {
            return Ok(Some((value, (*name).to_owned())));
        }
    }
    Ok(None)
}

fn usage_mappings<'py>(
    payload: &Bound<'py, PyDict>,
    mapping: &Bound<'py, PyAny>,
) -> PyResult<Vec<(Bound<'py, PyAny>, String)>> {
    let mut found = Vec::new();
    for key in USAGE_KEYS {
        if let Some(value) = payload.get_item(key)? {
            if is_mapping(&value, mapping)? {
                found.push((value, key.to_owned()));
            }
        }
    }
    for container_key in USAGE_CONTAINERS {
        let Some(container) = payload.get_item(container_key)? else {
            continue;
        };
        if !is_mapping(&container, mapping)? {
            continue;
        }
        for usage_key in USAGE_KEYS {
            let value = mapping_get(&container, usage_key)?;
            if is_mapping(&value, mapping)? {
                found.push((value, format!("{container_key}.{usage_key}")));
            }
        }
    }
    Ok(found)
}

fn add_usage_field(
    output: &Bound<'_, PyDict>,
    usage: &Bound<'_, PyAny>,
    field_names: &mut Vec<String>,
    output_name: &str,
    names: &[&str],
) -> PyResult<()> {
    if let Some((value, matched)) = usage_value(usage, names)? {
        output.set_item(output_name, value)?;
        field_names.push(matched);
    }
    Ok(())
}

fn add_native_usage(
    output: &Bound<'_, PyDict>,
    payload: &Bound<'_, PyDict>,
    mapping: &Bound<'_, PyAny>,
) -> PyResult<()> {
    for (usage, usage_path) in usage_mappings(payload, mapping)? {
        let fields = PyDict::new(payload.py());
        let mut field_names = Vec::new();
        add_usage_field(
            &fields,
            &usage,
            &mut field_names,
            "input_tokens",
            &["input_tokens", "prompt_tokens", "prompt_eval_count"],
        )?;
        add_usage_field(
            &fields,
            &usage,
            &mut field_names,
            "output_tokens",
            &["output_tokens", "completion_tokens", "eval_count"],
        )?;
        add_usage_field(
            &fields,
            &usage,
            &mut field_names,
            "cached_input_tokens",
            &[
                "cached_tokens",
                "cache_read_input_tokens",
                "cache_read_tokens",
            ],
        )?;
        add_usage_field(
            &fields,
            &usage,
            &mut field_names,
            "cache_creation_input_tokens",
            &["cache_creation_input_tokens", "cache_creation_tokens"],
        )?;
        add_usage_field(
            &fields,
            &usage,
            &mut field_names,
            "reasoning_tokens",
            &["reasoning_tokens"],
        )?;
        add_usage_field(
            &fields,
            &usage,
            &mut field_names,
            "total_tokens",
            &["total_tokens"],
        )?;

        for detail_key in [
            "prompt_tokens_details",
            "input_tokens_details",
            "promptTokenDetails",
        ] {
            let details = mapping_get(&usage, detail_key)?;
            if is_mapping(&details, mapping)? && !fields.contains("cached_input_tokens")? {
                if let Some((value, matched)) = usage_value(
                    &details,
                    &[
                        "cached_tokens",
                        "cache_read_input_tokens",
                        "cache_read_tokens",
                    ],
                )? {
                    fields.set_item("cached_input_tokens", value)?;
                    field_names.push(format!("{detail_key}.{matched}"));
                }
            }
        }
        for detail_key in [
            "completion_tokens_details",
            "output_tokens_details",
            "completionTokenDetails",
        ] {
            let details = mapping_get(&usage, detail_key)?;
            if is_mapping(&details, mapping)? && !fields.contains("reasoning_tokens")? {
                if let Some((value, matched)) = usage_value(&details, &["reasoning_tokens"])? {
                    fields.set_item("reasoning_tokens", value)?;
                    field_names.push(format!("{detail_key}.{matched}"));
                }
            }
        }

        if !fields.is_empty() {
            for (key, value) in fields.iter() {
                output.set_item(key, value)?;
            }
            field_names.sort();
            output.set_item("usage_source", "native")?;
            output.set_item("native_usage_path", usage_path)?;
            output.set_item("native_usage_fields", field_names)?;
            return Ok(());
        }
    }
    for name in [
        "input_tokens",
        "output_tokens",
        "cached_input_tokens",
        "cache_creation_input_tokens",
        "reasoning_tokens",
        "total_tokens",
    ] {
        output.set_item(name, payload.py().None())?;
    }
    output.set_item("usage_source", "none")?;
    output.set_item("native_usage_path", payload.py().None())?;
    output.set_item("native_usage_fields", Vec::<String>::new())?;
    Ok(())
}

fn bounded_output(
    value: &Bound<'_, PyAny>,
    encoded: &[u8],
    mapping: &Bound<'_, PyAny>,
) -> PyResult<bool> {
    if is_mapping(value, mapping)? {
        for key in ["elided", "truncated", "output_bounded", "outputBounded"] {
            let marker = mapping_get(value, key)?;
            if marker.is_instance_of::<PyBool>() && marker.is_truthy()? {
                return Ok(true);
            }
            if marker.is_instance_of::<PyString>() && marker.is_truthy()? {
                return Ok(true);
            }
        }
    }
    let lowered = String::from_utf8_lossy(encoded).to_lowercase();
    Ok(lowered.contains("\"elided\"") || lowered.contains("\"truncated\""))
}

#[pyfunction]
fn context_telemetry_event_native<'py>(
    py: Python<'py>,
    payload: &Bound<'py, PyAny>,
    timestamp: &str,
) -> PyResult<Bound<'py, PyDict>> {
    let empty = PyDict::new(py);
    let payload = payload.cast::<PyDict>().unwrap_or(&empty);
    let mapping = mapping_type(py)?;
    let tool_input = first_field(payload, &["tool_input", "toolInput"])?
        .unwrap_or_else(|| py.None().into_bound(py));
    let mut tool_output = first_field(
        payload,
        &[
            "tool_response",
            "toolResult",
            "tool_output",
            "toolOutput",
            "tool_result",
        ],
    )?
    .unwrap_or_else(|| py.None().into_bound(py));
    if tool_output.is_none() {
        tool_output = payload
            .get_item("output")?
            .unwrap_or_else(|| py.None().into_bound(py));
    }
    let event_name = field_string(
        payload,
        &["hook_event_name", "hookEventName"],
        "PostToolUse",
    )?;
    let tool_name = field_string(payload, &["tool_name", "toolName"], "unknown")?;
    let tool_output = model_visible_output(py, &tool_name, tool_output, &mapping)?;
    let input_encoded = json_bytes(py, &tool_input);
    let output_encoded = json_bytes(py, &tool_output);
    let input_bytes = input_encoded.len();
    let output_bytes = output_encoded.len();

    let result = PyDict::new(py);
    result.set_item("timestamp", timestamp)?;
    result.set_item("pid", std::process::id())?;
    result.set_item("provider", provider())?;
    result.set_item("event", truncate_chars(&event_name, MAX_TOOL_CHARS))?;
    result.set_item("tool", truncate_chars(&tool_name, MAX_TOOL_CHARS))?;
    result.set_item("input_bytes", input_bytes)?;
    result.set_item("output_bytes", output_bytes)?;
    result.set_item("tool_input_tokens_estimate", input_bytes.div_ceil(4))?;
    result.set_item("output_tokens_estimate", output_bytes.div_ceil(4))?;
    result.set_item("output_tokens_estimate_scope", "tool-output-bytes")?;
    result.set_item(
        "output_bounded",
        bounded_output(&tool_output, &output_encoded, &mapping)?,
    )?;
    add_native_usage(&result, payload, &mapping)?;
    Ok(result)
}

#[pyfunction]
fn context_telemetry_hook_event_native<'py>(
    py: Python<'py>,
    hook: &str,
    hook_json: &Bound<'py, PyAny>,
    event_name: &str,
    timestamp: &str,
) -> PyResult<Bound<'py, PyDict>> {
    let mapping = mapping_type(py)?;
    let mut context = String::new();
    let mut selected_event = event_name.to_owned();
    if let Ok(payload) = hook_json.cast::<PyDict>() {
        if let Some(specific) = payload.get_item("hookSpecificOutput")? {
            if is_mapping(&specific, &mapping)? {
                for key in ["additionalContext", "permissionDecisionReason"] {
                    let value = mapping_get(&specific, key)?;
                    if value.is_truthy()? {
                        context = value.str()?.to_string_lossy().into_owned();
                        break;
                    }
                }
                if selected_event.is_empty() {
                    let value = mapping_get(&specific, "hookEventName")?;
                    if value.is_truthy()? {
                        selected_event = value.str()?.to_string_lossy().into_owned();
                    }
                }
            }
        }
    }
    let output_bytes = context.len();
    let result = PyDict::new(py);
    result.set_item("timestamp", timestamp)?;
    result.set_item("pid", std::process::id())?;
    result.set_item("provider", provider())?;
    result.set_item("event", "HookContext")?;
    result.set_item(
        "hook_event",
        truncate_chars(
            if selected_event.is_empty() {
                "unknown"
            } else {
                &selected_event
            },
            MAX_TOOL_CHARS,
        ),
    )?;
    result.set_item("tool", truncate_chars(hook, MAX_TOOL_CHARS))?;
    result.set_item("input_bytes", 0)?;
    result.set_item("output_bytes", output_bytes)?;
    result.set_item("tool_input_tokens_estimate", 0)?;
    result.set_item("output_tokens_estimate", output_bytes.div_ceil(4))?;
    result.set_item(
        "output_tokens_estimate_scope",
        "hook-additional-context-bytes",
    )?;
    result.set_item("output_bounded", false)?;
    Ok(result)
}

fn append_bytes(path: &Path, encoded: &[u8], max_log_bytes: u64) -> std::io::Result<bool> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut handle = OpenOptions::new()
        .create(true)
        .read(true)
        .append(true)
        .open(path)?;
    handle.lock()?;
    let result: std::io::Result<bool> = (|| {
        let length = handle.seek(SeekFrom::End(0))?;
        if length.saturating_add(encoded.len() as u64) > max_log_bytes {
            return Ok(false);
        }
        handle.write_all(encoded)?;
        handle.flush()?;
        Ok(true)
    })();
    let appended = result?;
    handle.unlock()?;
    Ok(appended)
}

fn rotated_path(path: &Path, stamp: &str) -> PathBuf {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    path.with_file_name(format!("{name}.{stamp}.full"))
}

#[pyfunction]
fn context_telemetry_append_native(
    path: &str,
    encoded: &[u8],
    max_log_bytes: u64,
) -> PyResult<bool> {
    append_bytes(Path::new(path), encoded, max_log_bytes).map_err(io_error)
}

#[pyfunction]
fn context_telemetry_rotate_native(path: &str, stamp: &str) -> PyResult<String> {
    let path = Path::new(path);
    let target = rotated_path(path, stamp);
    fs::rename(path, &target).map_err(io_error)?;
    Ok(target.to_string_lossy().into_owned())
}

#[pyfunction]
fn context_telemetry_record_native(
    path: &str,
    encoded: &[u8],
    max_log_bytes: u64,
    stamp: &str,
) -> PyResult<Option<String>> {
    let path = Path::new(path);
    if append_bytes(path, encoded, max_log_bytes).map_err(io_error)? {
        return Ok(None);
    }
    let target = rotated_path(path, stamp);
    fs::rename(path, &target).map_err(io_error)?;
    if !append_bytes(path, encoded, max_log_bytes).map_err(io_error)? {
        return Err(PyOSError::new_err(format!(
            "context telemetry record exceeds the log budget ({})",
            path.display()
        )));
    }
    Ok(Some(target.to_string_lossy().into_owned()))
}

#[derive(Debug, Default, Serialize)]
struct SummaryRow {
    event: String,
    tool: String,
    count: u128,
    output_bytes: u128,
    output_tokens_estimate: u128,
    over_bound: u128,
    over_bound_bytes: u128,
    share: f64,
}

fn json_nonnegative(value: Option<&Value>) -> u128 {
    let Some(value) = value else {
        return 0;
    };
    match value {
        Value::Number(number) => number.to_string().parse().unwrap_or(0),
        Value::String(text) if text.chars().all(|character| character.is_ascii_digit()) => {
            text.parse().unwrap_or(0)
        }
        _ => 0,
    }
}

fn summarize(paths: &[String], bound_bytes: i128) -> std::io::Result<Value> {
    let mut positions: HashMap<(String, String), usize> = HashMap::new();
    let mut rows: Vec<SummaryRow> = Vec::new();
    for path in paths {
        let mut reader = BufReader::new(File::open(path)?);
        let mut line = Vec::new();
        while reader.read_until(b'\n', &mut line)? != 0 {
            let Ok(Value::Object(item)) = serde_json::from_slice::<Value>(&line) else {
                line.clear();
                continue;
            };
            let event = item.get("event").map_or_else(
                || "?".to_owned(),
                |value| match value {
                    Value::String(text) => text.clone(),
                    Value::Null => "None".to_owned(),
                    other => other.to_string(),
                },
            );
            let tool = item.get("tool").map_or_else(
                || "?".to_owned(),
                |value| match value {
                    Value::String(text) => text.clone(),
                    Value::Null => "None".to_owned(),
                    other => other.to_string(),
                },
            );
            let key = (event.clone(), tool.clone());
            let index = *positions.entry(key).or_insert_with(|| {
                rows.push(SummaryRow {
                    event,
                    tool,
                    ..SummaryRow::default()
                });
                rows.len() - 1
            });
            let row = &mut rows[index];
            let output_bytes = json_nonnegative(item.get("output_bytes"));
            row.count += 1;
            row.output_bytes += output_bytes;
            row.output_tokens_estimate += json_nonnegative(item.get("output_tokens_estimate"));
            if (output_bytes as i128) > bound_bytes {
                row.over_bound += 1;
                row.over_bound_bytes += ((output_bytes as i128) - bound_bytes) as u128;
            }
            line.clear();
        }
    }
    rows.sort_by_key(|row| Reverse(row.output_bytes));
    let total: u128 = rows.iter().map(|row| row.output_bytes).sum();
    for row in &mut rows {
        row.share = if total == 0 {
            0.0
        } else {
            ((row.output_bytes as f64 / total as f64) * 10_000.0).round() / 10_000.0
        };
    }
    let hook_context_bytes: u128 = rows
        .iter()
        .filter(|row| row.event == "HookContext")
        .map(|row| row.output_bytes)
        .sum();
    let events: u128 = rows.iter().map(|row| row.count).sum();
    let output_bytes: u128 = rows.iter().map(|row| row.output_bytes).sum();
    Ok(json!({
        "schema_version": "llm.context-telemetry.summary.v1",
        "files": paths,
        "bound_bytes": bound_bytes,
        "events": events,
        "output_bytes": output_bytes,
        "hook_context_bytes": hook_context_bytes,
        "rows": rows,
    }))
}

#[pyfunction]
fn context_telemetry_summarize_native(paths: Vec<String>, bound_bytes: i128) -> PyResult<String> {
    let summary = summarize(&paths, bound_bytes).map_err(io_error)?;
    serde_json::to_string(&summary).map_err(|error| PyValueError::new_err(error.to_string()))
}

pub(crate) fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(context_telemetry_event_native, module)?)?;
    module.add_function(wrap_pyfunction!(
        context_telemetry_model_visible_output_native,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(
        context_telemetry_hook_event_native,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(context_telemetry_append_native, module)?)?;
    module.add_function(wrap_pyfunction!(context_telemetry_rotate_native, module)?)?;
    module.add_function(wrap_pyfunction!(context_telemetry_record_native, module)?)?;
    module.add_function(wrap_pyfunction!(
        context_telemetry_summarize_native,
        module
    )?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temporary_path(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock is after epoch")
            .as_nanos();
        env::temp_dir().join(format!("conductor-context-telemetry-{name}-{nonce}"))
    }

    #[test]
    fn append_accepts_exact_cap_and_refuses_overflow() {
        let path = temporary_path("cap").join("events.jsonl");
        assert!(append_bytes(&path, b"one\n", 4).expect("append at cap"));
        assert!(!append_bytes(&path, b"two\n", 4).expect("refuse overflow"));
        assert_eq!(fs::read(&path).expect("read log"), b"one\n");
        fs::remove_dir_all(path.parent().expect("parent")).expect("remove temp");
    }

    #[test]
    fn summary_skips_invalid_lines_and_keeps_first_seen_ties() {
        let path = temporary_path("summary");
        fs::write(
            &path,
            b"{\"event\":\"PostToolUse\",\"tool\":\"B\",\"output_bytes\":4,\"output_tokens_estimate\":1}\nnot json\n{\"event\":\"HookContext\",\"tool\":\"A\",\"output_bytes\":4,\"output_tokens_estimate\":1}\n",
        )
        .expect("write events");
        let result = summarize(&[path.to_string_lossy().into_owned()], 3).expect("summary");
        assert_eq!(result["events"], 2);
        assert_eq!(result["hook_context_bytes"], 4);
        assert_eq!(result["rows"][0]["tool"], "B");
        assert_eq!(result["rows"][0]["over_bound_bytes"], 1);
        fs::remove_file(path).expect("remove temp");
    }

    #[test]
    fn rotated_name_preserves_original_filename() {
        assert_eq!(
            rotated_path(Path::new("/tmp/events.jsonl"), "20260904T010203Z"),
            PathBuf::from("/tmp/events.jsonl.20260904T010203Z.full")
        );
    }

    #[test]
    fn truncation_counts_characters_not_bytes() {
        assert_eq!(truncate_chars("ééé", 2), "éé");
    }
}
