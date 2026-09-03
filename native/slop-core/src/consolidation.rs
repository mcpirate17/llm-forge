use pyo3::exceptions::{PyRuntimeError, PySyntaxError};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::path::PathBuf;

use crate::file_profiles::normalized_function_hash;
use crate::python_ast::{ast_children, ast_fields, is_ast, list_items, string_attr, type_name};

const HIGH_RISK_PARTS: [&str; 5] = ["generator", "mechanisms", "models", "ops", "synthesis"];

#[derive(Clone, Debug)]
struct Record {
    file: String,
    line_start: usize,
    line_end: usize,
    name: String,
    node_hash: String,
    tokens: usize,
    source: String,
}

struct LoadedSource {
    path: PathBuf,
    relative: String,
    source: String,
}

fn universal_newlines(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .replace("\r\n", "\n")
        .replace('\r', "\n")
}

fn load_sources(paths: Vec<String>, repo: String) -> Result<(Vec<LoadedSource>, usize), String> {
    let repo = PathBuf::from(repo);
    let mut loaded = Vec::with_capacity(paths.len());
    let mut unparsable = 0;
    for raw_path in paths {
        let path = PathBuf::from(raw_path);
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(_) => {
                unparsable += 1;
                continue;
            }
        };
        let relative = path
            .strip_prefix(&repo)
            .map_err(|_| {
                format!(
                    "consolidation path {} is outside repository {}",
                    path.display(),
                    repo.display()
                )
            })?
            .to_string_lossy()
            .replace('\\', "/");
        loaded.push(LoadedSource {
            path,
            relative,
            source: universal_newlines(&bytes),
        });
    }
    Ok((loaded, unparsable))
}

fn is_docstring(node: &Bound<'_, PyAny>) -> PyResult<bool> {
    if type_name(node)? != "Expr" {
        return Ok(false);
    }
    let value = node.getattr("value")?;
    Ok(type_name(&value)? == "Constant" && value.getattr("value")?.extract::<String>().is_ok())
}

fn count_list(value: &Bound<'_, PyAny>, strip_docstring: bool) -> PyResult<usize> {
    let mut items = list_items(value)?;
    if strip_docstring
        && items
            .first()
            .is_some_and(|item| is_docstring(item).unwrap_or(false))
    {
        items.remove(0);
    }
    let mut count = 0;
    for item in items {
        if is_ast(&item)? {
            count += count_node(&item)?;
        }
    }
    Ok(count)
}

fn count_node(node: &Bound<'_, PyAny>) -> PyResult<usize> {
    let kind = type_name(node)?;
    let nested_scope = matches!(
        kind.as_str(),
        "FunctionDef" | "AsyncFunctionDef" | "ClassDef"
    );
    let mut count = 1;
    for (field, value) in ast_fields(node)? {
        if is_ast(&value)? {
            count += count_node(&value)?;
        } else if value.cast::<PyList>().is_ok() {
            count += count_list(&value, nested_scope && field == "body")?;
        }
    }
    Ok(count)
}

fn normalized_node_count(node: &Bound<'_, PyAny>) -> PyResult<usize> {
    Ok(1 + count_list(&node.getattr("body")?, true)?)
}

fn line_slices(source: &str) -> Vec<&str> {
    if source.is_empty() {
        return Vec::new();
    }
    source.split_inclusive('\n').collect()
}

fn source_segment(source: &str, node: &Bound<'_, PyAny>) -> PyResult<String> {
    let start_line: usize = node.getattr("lineno")?.extract()?;
    let end_line: Option<usize> = node.getattr("end_lineno")?.extract()?;
    let start_column: usize = node.getattr("col_offset")?.extract()?;
    let end_column: Option<usize> = node.getattr("end_col_offset")?.extract()?;
    let (Some(end_line), Some(end_column)) = (end_line, end_column) else {
        return Ok(String::new());
    };
    let lines = line_slices(source);
    let first = lines.get(start_line.saturating_sub(1)).ok_or_else(|| {
        PyRuntimeError::new_err(format!("source start line {start_line} is out of range"))
    })?;
    if start_line == end_line {
        return first
            .as_bytes()
            .get(start_column..end_column)
            .and_then(|bytes| std::str::from_utf8(bytes).ok())
            .map(str::to_owned)
            .ok_or_else(|| PyRuntimeError::new_err("invalid one-line AST source offsets"));
    }
    let last = lines.get(end_line.saturating_sub(1)).ok_or_else(|| {
        PyRuntimeError::new_err(format!("source end line {end_line} is out of range"))
    })?;
    let mut bytes = Vec::new();
    bytes.extend_from_slice(
        first
            .as_bytes()
            .get(start_column..)
            .ok_or_else(|| PyRuntimeError::new_err("invalid first-line AST source offset"))?,
    );
    for line in lines.iter().take(end_line - 1).skip(start_line) {
        bytes.extend_from_slice(line.as_bytes());
    }
    bytes.extend_from_slice(
        last.as_bytes()
            .get(..end_column)
            .ok_or_else(|| PyRuntimeError::new_err("invalid final-line AST source offset"))?,
    );
    String::from_utf8(bytes)
        .map_err(|_| PyRuntimeError::new_err("AST source offsets split a UTF-8 code point"))
}

fn extract_records(
    tree: &Bound<'_, PyAny>,
    file: &LoadedSource,
    min_lines: usize,
) -> PyResult<Vec<Record>> {
    let mut output = Vec::new();
    let mut queue = VecDeque::from([tree.clone()]);
    while let Some(node) = queue.pop_front() {
        queue.extend(ast_children(&node)?);
        if !matches!(
            type_name(&node)?.as_str(),
            "FunctionDef" | "AsyncFunctionDef"
        ) {
            continue;
        }
        let line_start: usize = node.getattr("lineno")?.extract()?;
        let line_end = node
            .getattr("end_lineno")?
            .extract::<Option<usize>>()?
            .unwrap_or(line_start);
        if line_end - line_start + 1 < min_lines {
            continue;
        }
        output.push(Record {
            file: file.relative.clone(),
            line_start,
            line_end,
            name: string_attr(&node, "name")?,
            node_hash: normalized_function_hash(&node)?,
            tokens: normalized_node_count(&node)?,
            source: source_segment(&file.source, &node)?,
        });
    }
    Ok(output)
}

#[derive(Clone, Debug)]
struct Cluster {
    kind: &'static str,
    tokens: usize,
    sites: Vec<Record>,
    confidence: f64,
    value_score: i64,
    risk: &'static str,
    disposition: &'static str,
    rationale: String,
}

fn record_from(value: &Bound<'_, PyAny>) -> PyResult<Record> {
    Ok(Record {
        file: value.getattr("file")?.extract()?,
        line_start: value.getattr("line_start")?.extract()?,
        line_end: value.getattr("line_end")?.extract()?,
        name: value.getattr("name")?.extract()?,
        node_hash: value.getattr("node_hash")?.extract()?,
        tokens: value.getattr("tokens")?.extract()?,
        source: value.getattr("source")?.extract()?,
    })
}

fn records_from(values: &Bound<'_, PyList>) -> PyResult<Vec<Record>> {
    values.iter().map(|value| record_from(&value)).collect()
}

fn parent(file: &str) -> &str {
    file.rsplit_once('/').map_or("", |(parent, _)| parent)
}

fn basename(file: &str) -> &str {
    file.rsplit_once('/').map_or(file, |(_, name)| name)
}

fn scope(cluster: &Cluster) -> (HashSet<String>, bool, bool, bool, bool) {
    let files: HashSet<&str> = cluster
        .sites
        .iter()
        .map(|site| site.file.as_str())
        .collect();
    let directories: HashSet<&str> = cluster
        .sites
        .iter()
        .map(|site| parent(&site.file))
        .collect();
    let roots: HashSet<&str> = cluster
        .sites
        .iter()
        .filter_map(|site| site.file.split('/').next())
        .collect();
    let names: HashSet<String> = cluster
        .sites
        .iter()
        .map(|site| site.name.trim_start_matches('_').to_owned())
        .collect();
    let semantic_risk = cluster.sites.iter().any(|site| {
        site.file
            .split('/')
            .any(|part| HIGH_RISK_PARTS.contains(&part))
            || site.name.starts_with("tpl_")
    });
    (
        names,
        files.len() == 1,
        directories.len() == 1,
        roots.len() == 1,
        semantic_risk,
    )
}

fn evidence(cluster: &mut Cluster) {
    let (names, same_file, same_dir, same_root, semantic_risk) = scope(cluster);
    let locality = if same_file {
        1.0
    } else if same_dir {
        0.92
    } else if same_root {
        0.72
    } else {
        0.35
    };
    let mut confidence: f64 = if cluster.kind == "exact" { 0.99 } else { 0.90 };
    if names.len() > 1 {
        confidence -= 0.08;
    }
    if semantic_risk {
        confidence -= 0.28;
    }
    if !same_root {
        confidence -= 0.18;
    }
    confidence = (confidence * 100.0).round_ties_even() / 100.0;
    confidence = confidence.max(0.05);
    let risk = if semantic_risk || !same_root {
        "high"
    } else if same_dir {
        "low"
    } else {
        "medium"
    };
    let risk_factor = match risk {
        "low" => 1.0,
        "medium" => 0.65,
        _ => 0.25,
    };
    let file_count = cluster
        .sites
        .iter()
        .map(|site| site.file.as_str())
        .collect::<HashSet<_>>()
        .len();
    let est_bytes = cluster
        .tokens
        .saturating_mul(cluster.sites.len().saturating_sub(1));
    let value = (est_bytes as f64 * confidence * locality * risk_factor
        / (file_count as f64).sqrt())
    .round_ties_even() as i64;
    let auto = value >= 35 && risk == "low" && (cluster.kind == "exact" || names.len() == 1);
    let validate = value >= 20 && confidence >= 0.55;
    let mut reasons = vec![
        cluster.kind,
        if same_file {
            "same-file"
        } else if same_dir {
            "same-dir"
        } else {
            "cross-dir"
        },
    ];
    if semantic_risk {
        reasons.push("semantic-family-risk");
    }
    if names.len() > 1 {
        reasons.push("different-symbol-names");
    }
    cluster.confidence = confidence;
    cluster.value_score = value;
    cluster.risk = risk;
    cluster.disposition = if auto {
        "auto"
    } else if validate {
        "validate"
    } else {
        "ignore"
    };
    cluster.rationale = reasons.join(", ");
}

fn build(records: Vec<Record>) -> Vec<Cluster> {
    let mut group_lookup: HashMap<String, usize> = HashMap::new();
    let mut groups: Vec<Vec<Record>> = Vec::new();
    for record in records {
        let index = match group_lookup.get(&record.node_hash) {
            Some(&index) => index,
            None => {
                let index = groups.len();
                group_lookup.insert(record.node_hash.clone(), index);
                groups.push(Vec::new());
                index
            }
        };
        groups[index].push(record);
    }

    let mut clusters = Vec::new();
    for group in groups {
        let mut positions: HashMap<(String, usize), usize> = HashMap::new();
        let mut sites: Vec<Record> = Vec::new();
        for record in group {
            let key = (record.file.clone(), record.line_start);
            if let Some(&position) = positions.get(&key) {
                sites[position] = record;
            } else {
                positions.insert(key, sites.len());
                sites.push(record);
            }
        }
        if sites.len() < 2 {
            continue;
        }
        let exact = sites.iter().all(|site| site.source == sites[0].source);
        let mut cluster = Cluster {
            kind: if exact { "exact" } else { "near" },
            tokens: sites[0].tokens,
            sites,
            confidence: 0.0,
            value_score: 0,
            risk: "high",
            disposition: "ignore",
            rationale: String::new(),
        };
        evidence(&mut cluster);
        clusters.push(cluster);
    }
    clusters.sort_by(|left, right| {
        right
            .value_score
            .cmp(&left.value_score)
            .then_with(|| {
                let left_bytes = left.tokens * left.sites.len().saturating_sub(1);
                let right_bytes = right.tokens * right.sites.len().saturating_sub(1);
                right_bytes.cmp(&left_bytes)
            })
            .then(Ordering::Equal)
    });
    clusters
}

fn common_parent(files: &[String]) -> Option<String> {
    let first_parent: Vec<&str> = parent(&files[0])
        .split('/')
        .filter(|part| !part.is_empty())
        .collect();
    let mut common_len = first_parent.len();
    for file in &files[1..] {
        let parts: Vec<&str> = parent(file)
            .split('/')
            .filter(|part| !part.is_empty())
            .collect();
        common_len = common_len.min(parts.len());
        while common_len > 0 && first_parent[..common_len] != parts[..common_len] {
            common_len -= 1;
        }
    }
    (common_len > 0).then(|| first_parent[..common_len].join("/"))
}

fn purpose_stem(files: &[String]) -> String {
    let tokens = |file: &str| -> Vec<String> {
        let name = basename(file);
        let stem = name.rsplit_once('.').map_or(name, |(stem, _)| stem);
        stem.trim_matches('_')
            .split('_')
            .map(str::to_owned)
            .collect()
    };
    let all_tokens: Vec<Vec<String>> = files.iter().map(|file| tokens(file)).collect();
    let mut seen = HashSet::new();
    let shared: Vec<String> = all_tokens[0]
        .iter()
        .filter(|token| {
            token.chars().count() > 2
                && all_tokens[1..].iter().all(|other| other.contains(token))
                && seen.insert((*token).clone())
        })
        .cloned()
        .collect();
    if shared.is_empty() {
        "common".to_owned()
    } else {
        shared.join("_")
    }
}

fn suggested_home(sites: &[Record]) -> Option<String> {
    let mut files: Vec<String> = sites.iter().map(|site| site.file.clone()).collect();
    files.sort();
    files.dedup();
    if files.len() == 1 {
        return Some(files[0].clone());
    }
    let existing: Vec<&String> = files
        .iter()
        .filter(|file| {
            let name = basename(file);
            name.starts_with("_base") || name.starts_with("base") || name.starts_with("shared_")
        })
        .collect();
    if existing.len() == 1 {
        return Some(existing[0].clone());
    }
    let common = common_parent(&files)?;
    Some(format!("{common}/_{}.py", purpose_stem(&files)))
}

fn assignment(clusters: &[(String, Vec<Record>)], batch_size: usize) -> Vec<(String, isize)> {
    struct Batch {
        id: isize,
        home: Option<String>,
        files: HashSet<String>,
        count: usize,
    }
    let mut next_batch = 0_isize;
    let mut batches: Vec<Batch> = Vec::new();
    let mut output = Vec::with_capacity(clusters.len());
    for (index, (disposition, sites)) in clusters.iter().enumerate() {
        let id = format!("C{:03}", index + 1);
        if disposition != "auto" {
            output.push((id, -1));
            continue;
        }
        let home = suggested_home(sites);
        let site_files: HashSet<String> = sites.iter().map(|site| site.file.clone()).collect();
        let compatible = batches.iter().position(|batch| {
            batch.home == home && batch.count < batch_size && !batch.files.is_disjoint(&site_files)
        });
        let batch_index = match compatible {
            Some(index) => index,
            None => {
                batches.push(Batch {
                    id: next_batch,
                    home,
                    files: HashSet::new(),
                    count: 0,
                });
                next_batch += 1;
                batches.len() - 1
            }
        };
        let batch = &mut batches[batch_index];
        batch.files.extend(site_files);
        batch.count += 1;
        output.push((id, batch.id));
    }
    output
}

fn record_to_dict<'py>(py: Python<'py>, record: &Record) -> PyResult<Bound<'py, PyDict>> {
    let output = PyDict::new(py);
    output.set_item("file", &record.file)?;
    output.set_item("line_start", record.line_start)?;
    output.set_item("line_end", record.line_end)?;
    output.set_item("name", &record.name)?;
    output.set_item("node_hash", &record.node_hash)?;
    output.set_item("tokens", record.tokens)?;
    output.set_item("source", &record.source)?;
    Ok(output)
}

#[pyfunction]
pub(crate) fn audit_consolidation_normalize(node: &Bound<'_, PyAny>) -> PyResult<(String, usize)> {
    Ok((
        normalized_function_hash(node)?,
        normalized_node_count(node)?,
    ))
}

/// Read the corpus without the GIL, then retain CPython only as the grammar and
/// canonical AST provider. Traversal, normalization, hashing, token counting,
/// source slicing, and record construction remain inside this one native batch.
#[pyfunction]
pub(crate) fn audit_consolidation_collect(
    py: Python<'_>,
    paths: Vec<String>,
    repo: String,
    min_lines: usize,
) -> PyResult<(Py<PyList>, usize)> {
    let (files, mut unparsable) = py
        .detach(move || load_sources(paths, repo))
        .map_err(pyo3::exceptions::PyValueError::new_err)?;
    let ast = py.import("ast")?;
    let output = PyList::empty(py);
    for file in files {
        let tree = match ast.call_method1("parse", (&file.source, file.path.to_string_lossy())) {
            Ok(tree) => tree,
            Err(error) if error.is_instance_of::<PySyntaxError>(py) => {
                unparsable += 1;
                continue;
            }
            Err(error) => return Err(error),
        };
        let records = extract_records(&tree, &file, min_lines).map_err(|error| {
            PyRuntimeError::new_err(format!(
                "native consolidation extraction failed for {}: {error}",
                file.relative
            ))
        })?;
        for record in &records {
            output.append(record_to_dict(py, record)?)?;
        }
    }
    Ok((output.unbind(), unparsable))
}

#[pyfunction]
pub(crate) fn audit_consolidation_build(
    py: Python<'_>,
    values: &Bound<'_, PyList>,
) -> PyResult<Py<PyList>> {
    let records = records_from(values)?;
    let clusters = py.detach(move || build(records));
    let output = PyList::empty(py);
    for cluster in clusters {
        let item = PyDict::new(py);
        item.set_item("kind", cluster.kind)?;
        item.set_item("tokens", cluster.tokens)?;
        item.set_item("confidence", cluster.confidence)?;
        item.set_item("value_score", cluster.value_score)?;
        item.set_item("risk", cluster.risk)?;
        item.set_item("disposition", cluster.disposition)?;
        item.set_item("rationale", cluster.rationale)?;
        let sites = PyList::empty(py);
        for site in &cluster.sites {
            sites.append(record_to_dict(py, site)?)?;
        }
        item.set_item("sites", sites)?;
        output.append(item)?;
    }
    Ok(output.unbind())
}

#[pyfunction]
pub(crate) fn audit_consolidation_assign(
    py: Python<'_>,
    values: &Bound<'_, PyList>,
    batch_size: usize,
) -> PyResult<Vec<(String, isize)>> {
    let mut clusters = Vec::with_capacity(values.len());
    for value in values.iter() {
        let disposition = value.getattr("disposition")?.extract()?;
        let sites = value.getattr("sites")?.cast_into::<PyList>()?;
        clusters.push((disposition, records_from(&sites)?));
    }
    Ok(py.detach(move || assignment(&clusters, batch_size)))
}

#[pyfunction]
pub(crate) fn audit_consolidation_suggested_home(
    py: Python<'_>,
    values: &Bound<'_, PyList>,
) -> PyResult<Option<String>> {
    let records = records_from(values)?;
    Ok(py.detach(move || suggested_home(&records)))
}

#[pyfunction]
pub(crate) fn audit_consolidation_evidence(
    py: Python<'_>,
    kind: &str,
    tokens: usize,
    values: &Bound<'_, PyList>,
) -> PyResult<(f64, i64, String, String, String)> {
    let records = records_from(values)?;
    let owned_kind = kind.to_owned();
    Ok(py.detach(move || {
        let mut cluster = Cluster {
            kind: if owned_kind == "exact" {
                "exact"
            } else if owned_kind == "near" {
                "near"
            } else {
                "token"
            },
            tokens,
            sites: records,
            confidence: 0.0,
            value_score: 0,
            risk: "high",
            disposition: "ignore",
            rationale: String::new(),
        };
        evidence(&mut cluster);
        (
            cluster.confidence,
            cluster.value_score,
            cluster.risk.to_owned(),
            cluster.disposition.to_owned(),
            cluster.rationale,
        )
    }))
}
