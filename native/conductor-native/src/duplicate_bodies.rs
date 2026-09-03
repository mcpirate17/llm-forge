//! Batched Python function-body fingerprints for governance duplicate detection.
//!
//! Git selection, snapshot reads, changed-line attribution, move detection, and
//! finding wording remain in Python. This module owns the repeated parse and
//! location-free structural canonicalization shared by the two policy callers.

use std::collections::{HashMap, HashSet};
use std::env;
use std::fs;
use std::path::{Component, Path, PathBuf};

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyBool, PyDict, PyInt, PyList};
use ruff_python_ast as ast;
use ruff_python_ast::comparable::{ComparableExpr, ComparableParameters, ComparableStmt};
use ruff_python_ast::visitor::source_order::{walk_stmt, SourceOrderVisitor};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

#[derive(Clone, Copy)]
enum FingerprintPolicy {
    Candidate,
    Standalone,
}

impl FingerprintPolicy {
    fn parse(value: &str) -> PyResult<Self> {
        match value {
            "candidate" => Ok(Self::Candidate),
            "standalone" => Ok(Self::Standalone),
            _ => Err(PyValueError::new_err(format!(
                "unknown duplicate-body policy: {value}"
            ))),
        }
    }
}

#[derive(Debug, Serialize)]
struct FunctionFingerprint {
    name: String,
    lineno: usize,
    end_lineno: usize,
    digest: String,
}

#[derive(Debug, Serialize)]
struct FileFingerprints {
    path: String,
    parse_error: bool,
    functions: Vec<FunctionFingerprint>,
}

#[derive(Debug, Serialize)]
struct DuplicateEntry {
    key: String,
    #[serde(rename = "firstFile")]
    first_file: String,
    #[serde(rename = "secondFile")]
    second_file: String,
    lines: Value,
}

#[derive(Debug, Deserialize)]
struct RawDuplicateRow {
    first_path: String,
    second_path: String,
    fragment: String,
    lines: Value,
}

type DuplicateBaselineComparison = (usize, usize, Vec<String>, Vec<String>, Vec<String>);

fn python_object_repr(value: &Bound<'_, PyAny>) -> PyResult<String> {
    value.repr()?.extract::<String>()
}

fn sorted_dict_keys_repr(value: &Bound<'_, PyDict>) -> PyResult<String> {
    let keys = value.keys();
    keys.call_method0("sort")?;
    keys.repr()?.extract::<String>()
}

fn validate_baseline_payload(payload: &Bound<'_, PyAny>, path: &str) -> PyResult<HashSet<String>> {
    let baseline = payload
        .cast::<PyDict>()
        .map_err(|_| PyValueError::new_err(format!("baseline root must be an object: {path}")))?;
    let expected_keys = ["_comment", "count", "entries"];
    let has_exact_keys = baseline.len() == expected_keys.len()
        && expected_keys
            .iter()
            .all(|key| baseline.contains(*key).unwrap_or(false));
    if !has_exact_keys {
        return Err(PyValueError::new_err(format!(
            "baseline keys are invalid ({path}): expected ['_comment', 'count', 'entries'], got {}",
            sorted_dict_keys_repr(baseline)?
        )));
    }

    let count = baseline
        .get_item("count")?
        .ok_or_else(|| PyValueError::new_err("baseline count disappeared during validation"))?;
    let entries_value = baseline
        .get_item("entries")?
        .ok_or_else(|| PyValueError::new_err("baseline entries disappeared during validation"))?;
    if count.is_instance_of::<PyBool>() || !count.is_instance_of::<PyInt>() || count.lt(0)? {
        return Err(PyValueError::new_err(format!(
            "baseline count must be a non-negative integer ({path}); got {}",
            python_object_repr(&count)?
        )));
    }
    let entries = entries_value.cast::<PyDict>().map_err(|_| {
        PyValueError::new_err(format!("baseline entries must be an object: {path}"))
    })?;
    if !count.eq(entries.len())? {
        return Err(PyValueError::new_err(format!(
            "baseline count mismatch ({path}): declared {}, found {}",
            python_object_repr(&count)?,
            entries.len()
        )));
    }

    let mut validated = HashSet::with_capacity(entries.len());
    for (key_value, entry_value) in entries.iter() {
        let key_repr = python_object_repr(&key_value)?;
        let key = key_value.extract::<String>().map_err(|_| {
            PyValueError::new_err(format!(
                "baseline contains an invalid entry key: {}",
                key_repr
            ))
        })?;
        if key.is_empty() {
            return Err(PyValueError::new_err(format!(
                "baseline contains an invalid entry key: {key_repr}"
            )));
        }
        let entry = entry_value.cast::<PyDict>().map_err(|_| {
            PyValueError::new_err(format!(
                "baseline entry {key_repr} must contain exactly 'files' and 'lines'"
            ))
        })?;
        if entry.len() != 2 || !entry.contains("files")? || !entry.contains("lines")? {
            return Err(PyValueError::new_err(format!(
                "baseline entry {key_repr} must contain exactly 'files' and 'lines'"
            )));
        }

        let files_value = entry
            .get_item("files")?
            .ok_or_else(|| PyValueError::new_err("baseline files disappeared during validation"))?;
        let files = files_value.cast::<PyList>().ok();
        let valid_files = if let Some(files) = files {
            if files.len() != 2 {
                None
            } else {
                let first = files.get_item(0)?.extract::<String>().ok();
                let second = files.get_item(1)?.extract::<String>().ok();
                match (first, second) {
                    (Some(first), Some(second))
                        if !first.is_empty() && !second.is_empty() && first <= second =>
                    {
                        Some((first, second))
                    }
                    _ => None,
                }
            }
        } else {
            None
        };
        let Some((first_file, second_file)) = valid_files else {
            return Err(PyValueError::new_err(format!(
                "baseline entry {key_repr} has invalid sorted file-pair metadata"
            )));
        };

        let lines = entry
            .get_item("lines")?
            .ok_or_else(|| PyValueError::new_err("baseline lines disappeared during validation"))?;
        if lines.is_instance_of::<PyBool>() || !lines.is_instance_of::<PyInt>() || lines.le(0)? {
            return Err(PyValueError::new_err(format!(
                "baseline entry {key_repr} has invalid line count {}",
                python_object_repr(&lines)?
            )));
        }

        let valid_key = key.rsplit_once("::").is_some_and(|(pair, digest)| {
            pair == format!("{first_file}::{second_file}")
                && digest.len() == 16
                && digest
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        });
        if !valid_key {
            return Err(PyValueError::new_err(format!(
                "baseline entry key does not match its file pair/hash schema: {key_repr}"
            )));
        }
        validated.insert(key);
    }
    Ok(validated)
}

#[pyfunction]
fn compare_duplicate_baseline_native(
    payload: &Bound<'_, PyAny>,
    current_entries: Vec<(String, String, String)>,
    changed_files: Option<Vec<String>>,
    path: &str,
) -> PyResult<DuplicateBaselineComparison> {
    let baseline = validate_baseline_payload(payload, path)?;
    let current = current_entries
        .into_iter()
        .map(|(key, first_file, second_file)| (key, (first_file, second_file)))
        .collect::<HashMap<_, _>>();
    let mut new_keys = current
        .keys()
        .filter(|key| !baseline.contains(*key))
        .cloned()
        .collect::<Vec<_>>();
    new_keys.sort();

    let Some(changed_files) = changed_files else {
        return Ok((
            current.len(),
            baseline.len(),
            new_keys,
            Vec::new(),
            Vec::new(),
        ));
    };
    let changed_files = changed_files.into_iter().collect::<HashSet<_>>();
    let (caused_keys, inherited_keys) = new_keys.iter().cloned().partition(|key| {
        let (first_file, second_file) = &current[key];
        changed_files.contains(first_file) || changed_files.contains(second_file)
    });
    Ok((
        current.len(),
        baseline.len(),
        new_keys,
        caused_keys,
        inherited_keys,
    ))
}

fn positive_integer(value: &Value) -> bool {
    let Value::Number(number) = value else {
        return false;
    };
    let text = number.to_string();
    text.bytes().all(|byte| byte.is_ascii_digit()) && text.bytes().any(|byte| byte != b'0')
}

fn python_repr(value: &Value) -> String {
    match value {
        Value::Null => "None".to_owned(),
        Value::Bool(true) => "True".to_owned(),
        Value::Bool(false) => "False".to_owned(),
        Value::String(text) => format!("{text:?}"),
        _ => value.to_string(),
    }
}

fn split_python_lines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((index, character)) = chars.next() {
        if !matches!(
            character,
            '\n' | '\r'
                | '\u{000b}'
                | '\u{000c}'
                | '\u{001c}'
                | '\u{001d}'
                | '\u{001e}'
                | '\u{0085}'
                | '\u{2028}'
                | '\u{2029}'
        ) {
            continue;
        }
        lines.push(&text[start..index]);
        let mut end = index + character.len_utf8();
        if character == '\r' {
            if let Some(&(next_index, '\n')) = chars.peek() {
                chars.next();
                end = next_index + 1;
            }
        }
        start = end;
    }
    if start < text.len() {
        lines.push(&text[start..]);
    }
    lines
}

fn normalized_fragment_bytes(fragment: &str) -> Vec<u8> {
    split_python_lines(fragment.trim_matches('\n'))
        .into_iter()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
        .into_bytes()
}

fn stable_dup_key(first_path: &str, second_path: &str, normalized_fragment: &[u8]) -> String {
    let digest = format!("{:x}", Sha256::digest(normalized_fragment));
    let (first, second) = if first_path <= second_path {
        (first_path, second_path)
    } else {
        (second_path, first_path)
    };
    format!("{first}::{second}::{}", &digest[..16])
}

#[pyfunction]
fn stable_duplicate_key_native(
    first_path: &str,
    second_path: &str,
    normalized_fragment: &[u8],
) -> String {
    stable_dup_key(first_path, second_path, normalized_fragment)
}

fn normalize_row(row: RawDuplicateRow, label: &str, index: usize) -> PyResult<DuplicateEntry> {
    if row.first_path.is_empty() {
        return Err(PyValueError::new_err(format!(
            "{label} {index} has an invalid firstFile.name"
        )));
    }
    if row.second_path.is_empty() {
        return Err(PyValueError::new_err(format!(
            "{label} {index} has an invalid secondFile.name"
        )));
    }
    if row.fragment.is_empty() {
        return Err(PyValueError::new_err(format!(
            "{label} {index} has an invalid fragment"
        )));
    }
    if !positive_integer(&row.lines) {
        return Err(PyValueError::new_err(format!(
            "{label} {index} has an invalid line count {}",
            python_repr(&row.lines)
        )));
    }
    Ok(DuplicateEntry {
        key: stable_dup_key(
            &row.first_path,
            &row.second_path,
            &normalized_fragment_bytes(&row.fragment),
        ),
        first_file: row.first_path,
        second_file: row.second_path,
        lines: row.lines,
    })
}

fn serialize_entries(entries: &[DuplicateEntry]) -> PyResult<String> {
    serde_json::to_string(entries).map_err(|error| PyValueError::new_err(error.to_string()))
}

#[pyfunction]
fn normalize_duplicate_rows_native(rows_json: &str, label: &str) -> PyResult<String> {
    let rows = serde_json::from_str::<Vec<RawDuplicateRow>>(rows_json).map_err(|error| {
        PyValueError::new_err(format!("duplicate rows are not valid JSON: {error}"))
    })?;
    let entries = rows
        .into_iter()
        .enumerate()
        .map(|(index, row)| normalize_row(row, label, index))
        .collect::<PyResult<Vec<_>>>()?;
    serialize_entries(&entries)
}

fn object_string<'a>(object: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    object.get(key)?.as_str().filter(|value| !value.is_empty())
}

fn lexical_absolute(path: &Path) -> Option<PathBuf> {
    let source = if path.is_absolute() {
        path.to_path_buf()
    } else {
        env::current_dir().ok()?.join(path)
    };
    let mut normalized = PathBuf::new();
    for component in source.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(Path::new("/")),
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(part) => normalized.push(part),
        }
    }
    Some(normalized)
}

fn resolved(path: &Path) -> Option<PathBuf> {
    fs::canonicalize(path)
        .ok()
        .or_else(|| lexical_absolute(path))
}

fn relativize(path: &str, root: &Path) -> String {
    let Some(path_resolved) = resolved(Path::new(path)) else {
        return path.to_owned();
    };
    let Some(root_resolved) = resolved(root) else {
        return path.to_owned();
    };
    path_resolved
        .strip_prefix(root_resolved)
        .ok()
        .map(|relative| relative.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|| path.to_owned())
}

#[pyfunction]
fn normalize_jscpd_report_native(report_json: &str, root: &str) -> PyResult<String> {
    let payload = serde_json::from_str::<Value>(report_json).map_err(|error| {
        PyValueError::new_err(format!("jscpd report is not valid JSON: {error}"))
    })?;
    let duplicates = payload
        .as_object()
        .and_then(|object| object.get("duplicates"))
        .and_then(Value::as_array)
        .ok_or_else(|| {
            PyValueError::new_err("jscpd report must be an object containing a duplicates array")
        })?;
    let mut entries = Vec::with_capacity(duplicates.len());
    for (index, duplicate) in duplicates.iter().enumerate() {
        let duplicate = duplicate.as_object().ok_or_else(|| {
            PyValueError::new_err(format!("jscpd duplicate {index} must be an object"))
        })?;
        let nested_name = |key: &str| {
            duplicate
                .get(key)
                .and_then(Value::as_object)
                .and_then(|object| object_string(object, "name"))
        };
        let row = RawDuplicateRow {
            first_path: nested_name("firstFile")
                .map(|path| relativize(path, Path::new(root)))
                .unwrap_or_default(),
            second_path: nested_name("secondFile")
                .map(|path| relativize(path, Path::new(root)))
                .unwrap_or_default(),
            fragment: object_string(duplicate, "fragment")
                .unwrap_or_default()
                .to_owned(),
            lines: duplicate.get("lines").cloned().unwrap_or(Value::Null),
        };
        entries.push(normalize_row(row, "jscpd duplicate", index)?);
    }
    serialize_entries(&entries)
}

struct FunctionCollector<'a> {
    line_starts: Vec<usize>,
    policy: FingerprintPolicy,
    statement_depth: usize,
    functions: Vec<(usize, usize, FunctionFingerprint)>,
    marker: std::marker::PhantomData<&'a str>,
}

impl FunctionCollector<'_> {
    fn line_number(&self, byte_offset: usize) -> usize {
        self.line_starts
            .partition_point(|start| *start <= byte_offset)
    }

    fn digest(value: &impl std::fmt::Debug) -> String {
        format!("{:x}", Sha256::digest(format!("{value:?}").as_bytes()))
    }

    fn is_docstring(statement: &ast::Stmt) -> bool {
        matches!(
            statement,
            ast::Stmt::Expr(expr)
                if matches!(expr.value.as_ref(), ast::Expr::StringLiteral(_))
        )
    }

    fn candidate_digest(&self, function: &ast::StmtFunctionDef, length: usize) -> Option<String> {
        let body = if function.body.first().is_some_and(Self::is_docstring) {
            &function.body[1..]
        } else {
            &function.body
        };
        if length < 10 || body.is_empty() {
            return None;
        }
        let body = body.iter().map(ComparableStmt::from).collect::<Vec<_>>();
        Some(Self::digest(&("Module", body)))
    }

    fn standalone_digest(&self, function: &ast::StmtFunctionDef, length: usize) -> Option<String> {
        if length < 8 {
            return None;
        }
        let parameters = ComparableParameters::from(&function.parameters);
        let body = function
            .body
            .iter()
            .map(ComparableStmt::from)
            .collect::<Vec<_>>();
        let returns: Option<ComparableExpr<'_>> = function.returns.as_ref().map(Into::into);
        Some(Self::digest(&("FunctionDef", parameters, body, returns)))
    }

    fn fingerprint(&self, function: &ast::StmtFunctionDef) -> Option<FunctionFingerprint> {
        let lineno = self.line_number(function.name.range.start().to_usize());
        let end_lineno = self.line_number(function.range.end().to_usize());
        let length = end_lineno.saturating_sub(lineno) + 1;
        let digest = match self.policy {
            FingerprintPolicy::Candidate => self.candidate_digest(function, length),
            FingerprintPolicy::Standalone => self.standalone_digest(function, length),
        }?;
        Some(FunctionFingerprint {
            name: function.name.as_str().to_owned(),
            lineno,
            end_lineno,
            digest,
        })
    }
}

impl<'a> SourceOrderVisitor<'a> for FunctionCollector<'a> {
    fn visit_stmt(&mut self, statement: &'a ast::Stmt) {
        if let ast::Stmt::FunctionDef(function) = statement {
            if let Some(fingerprint) = self.fingerprint(function) {
                self.functions.push((
                    self.statement_depth,
                    function.name.range.start().to_usize(),
                    fingerprint,
                ));
            }
        }
        self.statement_depth += 1;
        walk_stmt(self, statement);
        self.statement_depth -= 1;
    }
}

fn analyze_file(path: String, source: String, policy: FingerprintPolicy) -> FileFingerprints {
    let Ok(module) = ruff_python_parser::parse_module(&source) else {
        return FileFingerprints {
            path,
            parse_error: true,
            functions: Vec::new(),
        };
    };
    let syntax = module.into_syntax();
    let mut line_starts = vec![0];
    line_starts.extend(
        source
            .as_bytes()
            .iter()
            .enumerate()
            .filter_map(|(index, byte)| (*byte == b'\n').then_some(index + 1)),
    );
    let mut collector = FunctionCollector {
        line_starts,
        policy,
        statement_depth: 0,
        functions: Vec::new(),
        marker: std::marker::PhantomData,
    };
    for statement in &syntax.body {
        collector.visit_stmt(statement);
    }
    collector
        .functions
        .sort_by_key(|(depth, byte_offset, _)| (*depth, *byte_offset));
    FileFingerprints {
        path,
        parse_error: false,
        functions: collector
            .functions
            .into_iter()
            .map(|(_, _, fingerprint)| fingerprint)
            .collect(),
    }
}

#[pyfunction]
fn duplicate_body_fingerprints_native(
    py: Python<'_>,
    records: Vec<(String, String)>,
    policy: &str,
) -> PyResult<String> {
    let policy = FingerprintPolicy::parse(policy)?;
    let fingerprints = py.detach(|| {
        records
            .into_iter()
            .map(|(path, source)| analyze_file(path, source, policy))
            .collect::<Vec<_>>()
    });
    serde_json::to_string(&fingerprints).map_err(|error| PyValueError::new_err(error.to_string()))
}

pub(crate) fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(compare_duplicate_baseline_native, module)?)?;
    module.add_function(wrap_pyfunction!(
        duplicate_body_fingerprints_native,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(stable_duplicate_key_native, module)?)?;
    module.add_function(wrap_pyfunction!(normalize_duplicate_rows_native, module)?)?;
    module.add_function(wrap_pyfunction!(normalize_jscpd_report_native, module)?)?;
    Ok(())
}
