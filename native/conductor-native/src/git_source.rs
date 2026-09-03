//! Deterministic decoders for Git plumbing used by candidate review.
//!
//! Python retains subprocess execution and filesystem policy. This module owns
//! the byte-oriented parsing loops so large trees and repeated review diffs do
//! not become Python object-processing hot paths.

use std::collections::{HashMap, HashSet};

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::PyBytes;

type DiffChange = (
    String,
    Py<PyBytes>,
    Option<Py<PyBytes>>,
    String,
    String,
    String,
    String,
);
type TreeRecord = (Py<PyBytes>, String, String, String, Option<u64>);

fn split_nul(raw: &[u8]) -> impl Iterator<Item = &[u8]> {
    raw.split(|byte| *byte == 0)
}

fn ascii<'a>(raw: &'a [u8], context: &str) -> Result<&'a str, String> {
    std::str::from_utf8(raw).map_err(|_| format!("{context} is not ASCII"))
}

fn decode_diff<'py>(py: Python<'py>, raw: &[u8]) -> Result<Vec<DiffChange>, String> {
    let fields: Vec<&[u8]> = split_nul(raw).collect();
    let mut changes = Vec::new();
    let mut cursor = 0usize;
    while cursor < fields.len() && !fields[cursor].is_empty() {
        let header = ascii(fields[cursor], "raw Git diff header")?;
        cursor += 1;
        if !header.starts_with(':') {
            return Err(format!("malformed raw Git diff header: {header:?}"));
        }
        let parts: Vec<&str> = header[1..].split_whitespace().collect();
        if parts.len() != 5 {
            return Err(format!("malformed raw Git diff header: {header:?}"));
        }
        if cursor >= fields.len() || fields[cursor].is_empty() {
            return Err("raw Git diff omitted a candidate path".to_owned());
        }
        let first_path = fields[cursor];
        cursor += 1;
        let status = parts[4];
        let (path, old_path) = if status.starts_with(['R', 'C']) {
            if cursor >= fields.len() || fields[cursor].is_empty() {
                return Err("raw Git rename/copy omitted its destination path".to_owned());
            }
            let destination = fields[cursor];
            cursor += 1;
            (
                PyBytes::new(py, destination).unbind(),
                Some(PyBytes::new(py, first_path).unbind()),
            )
        } else {
            (PyBytes::new(py, first_path).unbind(), None)
        };
        changes.push((
            status.to_owned(),
            path,
            old_path,
            parts[0].to_owned(),
            parts[1].to_owned(),
            parts[2].to_owned(),
            parts[3].to_owned(),
        ));
    }
    Ok(changes)
}

fn decode_tree<'py>(py: Python<'py>, raw: &[u8]) -> Result<Vec<TreeRecord>, String> {
    let mut entries = Vec::new();
    for record in split_nul(raw).filter(|record| !record.is_empty()) {
        let Some(tab) = record.iter().position(|byte| *byte == b'\t') else {
            return Err("malformed git ls-tree record".to_owned());
        };
        let metadata = ascii(&record[..tab], "git ls-tree metadata")?;
        let fields: Vec<&str> = metadata.split_whitespace().collect();
        if fields.len() != 4 {
            return Err("malformed git ls-tree record".to_owned());
        }
        let size = if fields[3] == "-" {
            None
        } else {
            Some(
                fields[3]
                    .parse::<u64>()
                    .map_err(|_| "malformed git ls-tree record".to_owned())?,
            )
        };
        entries.push((
            PyBytes::new(py, &record[tab + 1..]).unbind(),
            fields[0].to_owned(),
            fields[1].to_owned(),
            fields[2].to_owned(),
            size,
        ));
    }
    Ok(entries)
}

fn decode_renames<'py>(py: Python<'py>, raw: &[u8]) -> Vec<(Py<PyBytes>, Py<PyBytes>)> {
    let fields: Vec<&[u8]> = split_nul(raw).collect();
    let mut pairs = Vec::with_capacity(fields.len() / 3);
    let mut cursor = 0usize;
    while cursor + 2 < fields.len() && !fields[cursor].is_empty() {
        pairs.push((
            PyBytes::new(py, fields[cursor + 1]).unbind(),
            PyBytes::new(py, fields[cursor + 2]).unbind(),
        ));
        cursor += 3;
    }
    pairs
}

fn decode_changed_lines(
    raw: &[u8],
    wanted: &[String],
) -> Result<Vec<(String, Vec<usize>)>, String> {
    let wanted: HashSet<&str> = wanted.iter().map(String::as_str).collect();
    let decoded = String::from_utf8_lossy(raw);
    let mut current: Option<&str> = None;
    let mut result: HashMap<&str, Vec<usize>> = HashMap::new();
    let mut order = Vec::new();
    for line in decoded.lines() {
        if let Some(path) = line.strip_prefix("+++ b/") {
            current = wanted.get(path).copied();
            if let Some(path) = current {
                if !result.contains_key(path) {
                    result.insert(path, Vec::new());
                    order.push(path);
                }
            }
        } else if line.starts_with("@@") {
            let Some(path) = current else { continue };
            let plus = line
                .split(' ')
                .nth(2)
                .and_then(|value| value.strip_prefix('+'))
                .ok_or_else(|| "malformed unified Git hunk header".to_owned())?;
            let (start_raw, count_raw) = plus.split_once(',').unwrap_or((plus, "1"));
            let start = start_raw
                .parse::<usize>()
                .map_err(|_| "malformed unified Git hunk start".to_owned())?;
            let count = count_raw
                .parse::<usize>()
                .map_err(|_| "malformed unified Git hunk count".to_owned())?;
            result
                .get_mut(path)
                .expect("current paths are inserted")
                .extend(start..start.saturating_add(count));
        }
    }
    Ok(order
        .into_iter()
        .map(|path| (path.to_owned(), result.remove(path).unwrap_or_default()))
        .collect())
}

#[pyfunction]
fn git_diff_changes_native(py: Python<'_>, raw: &[u8]) -> PyResult<Vec<DiffChange>> {
    decode_diff(py, raw).map_err(PyValueError::new_err)
}

#[pyfunction]
fn git_tree_entries_native(py: Python<'_>, raw: &[u8]) -> PyResult<Vec<TreeRecord>> {
    decode_tree(py, raw).map_err(PyValueError::new_err)
}

#[pyfunction]
fn git_rename_sources_native(py: Python<'_>, raw: &[u8]) -> Vec<(Py<PyBytes>, Py<PyBytes>)> {
    decode_renames(py, raw)
}

#[pyfunction]
fn git_changed_lines_native(
    raw: &[u8],
    wanted: Vec<String>,
) -> PyResult<Vec<(String, Vec<usize>)>> {
    decode_changed_lines(raw, &wanted).map_err(PyValueError::new_err)
}

pub(crate) fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(git_diff_changes_native, module)?)?;
    module.add_function(wrap_pyfunction!(git_tree_entries_native, module)?)?;
    module.add_function(wrap_pyfunction!(git_rename_sources_native, module)?)?;
    module.add_function(wrap_pyfunction!(git_changed_lines_native, module)?)?;
    Ok(())
}
