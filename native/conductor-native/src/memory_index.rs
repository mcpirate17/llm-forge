//! Streaming validation and retrieval for the workspace memory JSONL index.
//!
//! The Python boundary still owns catalog discovery, embedding requests, and
//! atomic writes. Rust owns the large read-only data path so a query does not
//! materialize hundreds of megabytes of vectors as Python objects.

use std::cmp::Ordering;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::PyDict;
use serde_json::{Map, Value};

const SCHEMA_VERSION: i64 = 3;
const PINNED_NUM_CTX: i64 = 2_048;
const MAX_HIT_TEXT_CHARS: usize = 500;

type NativeHit = (f64, String, String, String, String);

#[derive(Debug)]
struct EmbeddingMeta<'a> {
    fingerprint: &'a str,
    dimension: usize,
}

#[derive(Debug)]
struct Hit {
    ordinal: usize,
    score: f64,
    source: String,
    path: String,
    title: String,
    text: String,
}

fn python_repr(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => "None".to_owned(),
        Some(Value::Bool(value)) => if *value { "True" } else { "False" }.to_owned(),
        Some(Value::String(value)) => format!("'{value}'"),
        Some(value) => value.to_string(),
    }
}

fn row_object(value: &Value) -> Result<&Map<String, Value>, String> {
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

fn validate_row(row: &Map<String, Value>) -> Result<EmbeddingMeta<'_>, String> {
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

fn visit_rows(
    path: &str,
    mut visit: impl FnMut(&Map<String, Value>, EmbeddingMeta<'_>) -> Result<(), String>,
) -> Result<usize, String> {
    if !Path::new(path).is_file() {
        return Err(format!("memory index missing: {path}"));
    }
    let file =
        File::open(path).map_err(|error| format!("cannot open memory index {path}: {error}"))?;
    let mut count = 0usize;
    for line in BufReader::new(file).lines() {
        let line = line.map_err(|error| format!("cannot read memory index {path}: {error}"))?;
        if line.trim().is_empty() {
            continue;
        }
        let value = serde_json::from_str::<Value>(&line)
            .map_err(|error| format!("invalid memory index JSON: {error}"))?;
        let row = row_object(&value)?;
        let metadata = validate_row(row)?;
        visit(row, metadata)?;
        count += 1;
    }
    if count == 0 {
        return Err("memory index is empty".to_owned());
    }
    Ok(count)
}

// Match the improved Kahan-Babuska/Neumaier loop used by CPython 3.12 sum().
fn compensated_dot(query: &[f64], vector: impl Iterator<Item = f64>) -> f64 {
    let mut total = 0.0_f64;
    let mut compensation = 0.0_f64;
    for (query_value, value) in query.iter().zip(vector) {
        let product = *query_value * value;
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
    total
}

fn row_string(row: &Map<String, Value>, field: &str) -> Result<String, String> {
    row.get(field)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("memory index row field {field:?} must be a string"))
}

fn score_json_row(row: &Map<String, Value>, query: &[f64]) -> Result<Hit, String> {
    let vector = row["vector"].as_array().expect("validated vector");
    if vector.len() != query.len() {
        return Err(format!("dim mismatch in {}", python_repr(row.get("path"))));
    }
    let mut total = 0.0_f64;
    let mut compensation = 0.0_f64;
    for (query_value, value) in query.iter().zip(vector) {
        let value = value
            .as_f64()
            .ok_or_else(|| "memory index vector values must be numbers".to_owned())?;
        let product = *query_value * value;
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
    Ok(Hit {
        ordinal: 0,
        score: total,
        source: row_string(row, "source")?,
        path: row_string(row, "path")?,
        title: row_string(row, "title")?,
        text: row_string(row, "text")?
            .chars()
            .take(MAX_HIT_TEXT_CHARS)
            .collect(),
    })
}

fn rank(mut hits: Vec<Hit>, top_k: usize) -> Vec<NativeHit> {
    hits.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(Ordering::Equal)
            .then_with(|| left.ordinal.cmp(&right.ordinal))
    });
    hits.into_iter()
        .take(top_k)
        .map(|hit| (hit.score, hit.source, hit.path, hit.title, hit.text))
        .collect()
}

#[pyfunction]
fn memory_index_metadata_native(path: &str) -> PyResult<(String, usize, usize)> {
    let mut fingerprint: Option<String> = None;
    let mut dimension = 0usize;
    let count = visit_rows(path, |_row, metadata| {
        if fingerprint
            .as_deref()
            .is_some_and(|existing| existing != metadata.fingerprint)
        {
            return Err("memory index contains mixed embedding fingerprints".to_owned());
        }
        fingerprint.get_or_insert_with(|| metadata.fingerprint.to_owned());
        if dimension == 0 {
            dimension = metadata.dimension;
        }
        Ok(())
    })
    .map_err(PyValueError::new_err)?;
    Ok((fingerprint.expect("nonempty index"), dimension, count))
}

#[pyfunction]
fn memory_index_query_file_native(
    path: &str,
    query: Vec<f64>,
    top_k: usize,
    required_fingerprint: &str,
) -> PyResult<Vec<NativeHit>> {
    let mut hits = Vec::new();
    visit_rows(path, |row, metadata| {
        if metadata.fingerprint != required_fingerprint {
            return Err("memory index changed embedding fingerprint during query".to_owned());
        }
        let mut hit = score_json_row(row, &query)?;
        hit.ordinal = hits.len();
        hits.push(hit);
        Ok(())
    })
    .map_err(PyValueError::new_err)?;
    Ok(rank(hits, top_k))
}

fn dict_string(row: &Bound<'_, PyDict>, field: &str) -> PyResult<String> {
    row.get_item(field)?
        .ok_or_else(|| PyValueError::new_err(format!("memory index row is missing {field:?}")))?
        .extract::<String>()
        .map_err(|_| {
            PyValueError::new_err(format!("memory index row field {field:?} must be a string"))
        })
}

#[pyfunction]
fn memory_index_score_rows_native(
    query: Vec<f64>,
    rows: Vec<Bound<'_, PyDict>>,
    top_k: usize,
) -> PyResult<Vec<NativeHit>> {
    let mut hits = Vec::with_capacity(rows.len());
    for (ordinal, row) in rows.into_iter().enumerate() {
        let path = dict_string(&row, "path")?;
        let vector = row
            .get_item("vector")?
            .ok_or_else(|| PyValueError::new_err("memory index row is missing \"vector\""))?
            .extract::<Vec<f64>>()?;
        if vector.len() != query.len() {
            return Err(PyValueError::new_err(format!("dim mismatch in {path}")));
        }
        let score = compensated_dot(&query, vector.into_iter());
        let text = dict_string(&row, "text")?
            .chars()
            .take(MAX_HIT_TEXT_CHARS)
            .collect();
        hits.push(Hit {
            ordinal,
            score,
            source: dict_string(&row, "source")?,
            path,
            title: dict_string(&row, "title")?,
            text,
        });
    }
    Ok(rank(hits, top_k))
}

pub(crate) fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(memory_index_metadata_native, module)?)?;
    module.add_function(wrap_pyfunction!(memory_index_query_file_native, module)?)?;
    module.add_function(wrap_pyfunction!(memory_index_score_rows_native, module)?)?;
    Ok(())
}
