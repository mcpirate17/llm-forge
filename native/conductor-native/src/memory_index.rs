//! Validation and preview decoding for native mapped workspace memory.
use std::io::BufRead;

use serde_json::{Map, Value};
pub(crate) const MAX_ROW_BYTES: usize = 1024 * 1024;

const SCHEMA_VERSION: i64 = 3;
const PINNED_NUM_CTX: i64 = 2_048;
const MAX_HIT_TEXT_CHARS: usize = 500;

pub type NativeHit = (f64, String, String, String, String);

#[derive(Debug)]
pub(crate) struct EmbeddingMeta<'a> {
    pub(crate) fingerprint: &'a str,
    pub(crate) dimension: usize,
}

fn python_repr(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => "None".to_owned(),
        Some(Value::Bool(value)) => if *value { "True" } else { "False" }.to_owned(),
        Some(Value::String(value)) => format!("'{value}'"),
        Some(value) => value.to_string(),
    }
}

pub(crate) fn row_object(value: &Value) -> Result<&Map<String, Value>, String> {
    value
        .as_object()
        .ok_or_else(|| "memory index row must be a JSON object".to_owned())
}

fn schema_matches(value: Option<&Value>) -> bool {
    value.is_some_and(|value| {
        !value.is_boolean()
            && (value.as_i64() == Some(SCHEMA_VERSION)
                || value.as_f64() == Some(SCHEMA_VERSION as f64))
    })
}

fn integer(value: Option<&Value>) -> Option<i64> {
    value.filter(|value| !value.is_boolean())?.as_i64()
}

pub(crate) fn validate_row(row: &Map<String, Value>) -> Result<EmbeddingMeta<'_>, String> {
    if !schema_matches(row.get("schema_version")) {
        return Err(format!(
            "unsupported memory index schema {}; reindex required",
            python_repr(row.get("schema_version"))
        ));
    }
    let metadata = row
        .get("embedding")
        .and_then(Value::as_object)
        .ok_or_else(|| "index embedding metadata is missing".to_owned())?;
    let fingerprint = metadata
        .get("fingerprint")
        .and_then(Value::as_str)
        .filter(|value| value.starts_with("sha256:"))
        .ok_or_else(|| "index embedding fingerprint is invalid".to_owned())?;
    let dimension = integer(metadata.get("dimension"))
        .filter(|value| *value >= 1)
        .map(|value| value as usize)
        .ok_or_else(|| {
            format!(
                "index embedding dimension is invalid: {}",
                python_repr(metadata.get("dimension"))
            )
        })?;
    let paid = metadata
        .get("paid")
        .and_then(Value::as_bool)
        .ok_or_else(|| "index embedding paid flag is invalid".to_owned())?;
    if !paid {
        let gpu = integer(metadata.get("num_gpu"));
        if !matches!(gpu, Some(0 | 99)) {
            return Err(format!(
                "index num_gpu={} not in [0, 99] (0=CPU, 99=GPU guest)",
                python_repr(metadata.get("num_gpu"))
            ));
        }
        if metadata.get("num_ctx").and_then(Value::as_f64) != Some(PINNED_NUM_CTX as f64) {
            return Err(format!(
                "index num_ctx={} must be {PINNED_NUM_CTX}",
                python_repr(metadata.get("num_ctx"))
            ));
        }
    }
    let source_sha256 = row.get("source_sha256").and_then(Value::as_str);
    if source_sha256.is_none_or(|value| value.len() != 64) {
        return Err(format!(
            "memory index row {} lacks source_sha256",
            python_repr(row.get("path"))
        ));
    }
    let vector = row.get("vector").and_then(Value::as_array);
    let Some(vector) = vector.filter(|vector| !vector.is_empty()) else {
        return Err(format!(
            "memory index row {} has no vector",
            python_repr(row.get("path"))
        ));
    };
    if vector.len() != dimension {
        return Err(format!(
            "memory index row {} dimension disagrees with metadata",
            python_repr(row.get("path"))
        ));
    }
    Ok(EmbeddingMeta {
        fingerprint,
        dimension,
    })
}

pub(crate) fn read_line(reader: &mut impl BufRead, line: &mut Vec<u8>) -> Result<usize, String> {
    crate::bounded_io::read_line(reader, line, MAX_ROW_BYTES).map_err(|error| {
        if error.kind() == std::io::ErrorKind::InvalidData {
            "memory index row exceeds 1 MiB".into()
        } else {
            format!("memory index read: {error}")
        }
    })
}

// Match the improved Kahan-Babuska/Neumaier loop used by CPython 3.12 sum().
pub(crate) fn compensated_dot(query: &[f64], vector: impl Iterator<Item = f64>) -> f64 {
    try_compensated_dot(query, vector.map(Ok)).expect("infallible numeric iterator")
}

fn try_compensated_dot(
    query: &[f64],
    vector: impl Iterator<Item = Result<f64, String>>,
) -> Result<f64, String> {
    let mut total = 0.0_f64;
    let mut compensation = 0.0_f64;
    for (query_value, value) in query.iter().zip(vector) {
        let product = *query_value * value?;
        let next = total + product;
        if total.abs() >= product.abs() {
            compensation += (total - next) + product;
        } else {
            compensation += (product - next) + total;
        }
        total = next;
    }
    if compensation != 0.0 && compensation.is_finite() {
        total += compensation;
    }
    Ok(total)
}

fn row_string(row: &Map<String, Value>, field: &str) -> Result<String, String> {
    row.get(field)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("memory index row field {field:?} must be a string"))
}

pub(crate) fn validate_preview(row: &Map<String, Value>) -> Result<(), String> {
    for (field, maximum) in [
        ("source", 256),
        ("path", 4096),
        ("title", 4096),
        ("text", MAX_ROW_BYTES),
    ] {
        let value = row
            .get(field)
            .and_then(Value::as_str)
            .ok_or_else(|| format!("memory index row field {field:?} must be a string"))?;
        if value.len() > maximum {
            return Err(format!("memory index {field} exceeds byte limit"));
        }
    }
    Ok(())
}

pub(crate) fn preview(row: &Map<String, Value>, score: f64) -> Result<NativeHit, String> {
    validate_preview(row)?;
    Ok((
        score,
        row_string(row, "source")?,
        row_string(row, "path")?,
        row_string(row, "title")?,
        row["text"]
            .as_str()
            .expect("validated text")
            .chars()
            .take(MAX_HIT_TEXT_CHARS)
            .collect(),
    ))
}
