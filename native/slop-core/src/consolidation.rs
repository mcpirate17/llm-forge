use pyo3::exceptions::{PyRuntimeError, PySyntaxError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};
use sha1::{Digest, Sha1};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::fs;
use std::path::PathBuf;

use crate::file_families::{
    abstraction_upper_bound, compare, minhash_signature, Profile, Similarity,
};
use crate::file_profiles::{function_profile, module_bindings, normalized_function_hash};
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
    profiles: Option<&Bound<'_, PyList>>,
) -> PyResult<Vec<Record>> {
    let mut output = Vec::new();
    let bindings = if profiles.is_some() {
        module_bindings(tree)?
    } else {
        BTreeMap::new()
    };
    let mut queue = VecDeque::from([(tree.clone(), String::new())]);
    while let Some((node, parent)) = queue.pop_front() {
        let kind = type_name(&node)?;
        let qualified = if matches!(
            kind.as_str(),
            "FunctionDef" | "AsyncFunctionDef" | "ClassDef"
        ) {
            let name = string_attr(&node, "name")?;
            if parent.is_empty() {
                name
            } else {
                format!("{parent}.{name}")
            }
        } else {
            parent
        };
        queue.extend(
            ast_children(&node)?
                .into_iter()
                .map(|child| (child, qualified.clone())),
        );
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
        let record = Record {
            file: file.relative.clone(),
            line_start,
            line_end,
            name: string_attr(&node, "name")?,
            node_hash: normalized_function_hash(&node)?,
            tokens: normalized_node_count(&node)?,
            source: source_segment(&file.source, &node)?,
        };
        if let Some(profiles) = profiles {
            let profile = function_profile(&node, &bindings)?;
            profile.set_item("file", &record.file)?;
            profile.set_item("name", &record.name)?;
            profile.set_item("qualified_name", &qualified)?;
            profile.set_item("line_start", record.line_start)?;
            profile.set_item("line_end", record.line_end)?;
            profile.set_item("node_hash", &record.node_hash)?;
            profiles.append(profile)?;
        }
        output.push(record);
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
    let (records, _, unparsable) = collect_profiles(py, paths, repo, min_lines, false)?;
    Ok((records, unparsable))
}

#[pyfunction]
pub(crate) fn audit_consolidation_collect_profiles(
    py: Python<'_>,
    paths: Vec<String>,
    repo: String,
    min_lines: usize,
) -> PyResult<(Py<PyList>, Py<PyList>, usize)> {
    collect_profiles(py, paths, repo, min_lines, true)
}

fn collect_profiles(
    py: Python<'_>,
    paths: Vec<String>,
    repo: String,
    min_lines: usize,
    with_profiles: bool,
) -> PyResult<(Py<PyList>, Py<PyList>, usize)> {
    let (files, mut unparsable) = py
        .detach(move || load_sources(paths, repo))
        .map_err(pyo3::exceptions::PyValueError::new_err)?;
    let ast = py.import("ast")?;
    let output = PyList::empty(py);
    let profiles = PyList::empty(py);
    for file in files {
        let tree = match ast.call_method1("parse", (&file.source, file.path.to_string_lossy())) {
            Ok(tree) => tree,
            Err(error) if error.is_instance_of::<PySyntaxError>(py) => {
                unparsable += 1;
                continue;
            }
            Err(error) => return Err(error),
        };
        let records = extract_records(&tree, &file, min_lines, with_profiles.then_some(&profiles))
            .map_err(|error| {
                PyRuntimeError::new_err(format!(
                    "native consolidation extraction failed for {}: {error}",
                    file.relative
                ))
            })?;
        for record in &records {
            output.append(record_to_dict(py, record)?)?;
        }
    }
    Ok((output.unbind(), profiles.unbind(), unparsable))
}

struct AbstractionProfile {
    features: Profile,
    shape: String,
    normalized: String,
    semantic: BTreeMap<String, BTreeSet<String>>,
    anchors: BTreeSet<String>,
    eligible: bool,
}

fn required<'py>(value: &Bound<'py, PyDict>, key: &str) -> PyResult<Bound<'py, PyAny>> {
    value
        .get_item(key)?
        .ok_or_else(|| PyValueError::new_err(format!("profile missing {key}")))
}

fn abstraction_profile(value: &Bound<'_, PyAny>) -> PyResult<AbstractionProfile> {
    let value = value.cast::<PyDict>()?;
    let names = ["structure", "api", "fields", "calls", "control", "imports"];
    let mut sets: [HashSet<String>; 6] = std::array::from_fn(|_| HashSet::new());
    for (index, name) in names.iter().enumerate() {
        sets[index] = required(value, name)?
            .extract::<Vec<String>>()?
            .into_iter()
            .collect();
    }
    let mut features = sets[0].clone();
    for set in &sets[1..5] {
        features.extend(set.iter().cloned());
    }
    let mut anchors: BTreeSet<String> = required(value, "schemas")?
        .extract::<Vec<String>>()?
        .into_iter()
        .collect();
    anchors.extend(sets[3].iter().cloned());
    Ok(AbstractionProfile {
        features: Profile {
            sets,
            lsh_features: features,
        },
        shape: required(value, "coarse_shape_hash")?.extract()?,
        normalized: required(value, "node_hash")?.extract()?,
        semantic: required(value, "semantic")?
            .extract::<BTreeMap<String, Vec<String>>>()?
            .into_iter()
            .map(|(key, values)| (key, values.into_iter().collect()))
            .collect(),
        anchors,
        eligible: required(value, "meaningful_operations")?.extract::<usize>()? >= 2
            && required(value, "body_statements")?.extract::<usize>()? >= 2,
    })
}

fn feature_hash(value: &str) -> u64 {
    let digest = Sha1::digest(value.as_bytes());
    u64::from_le_bytes(digest[..8].try_into().expect("eight digest bytes"))
}

fn abstraction_buckets(
    profiles: &[AbstractionProfile],
) -> (BTreeMap<String, Vec<usize>>, Vec<Vec<u64>>) {
    let mut buckets: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    let mut signatures = vec![Vec::new(); profiles.len()];
    for (index, profile) in profiles
        .iter()
        .enumerate()
        .filter(|(_, profile)| profile.eligible)
    {
        buckets
            .entry(format!("0:shape:{}", profile.shape))
            .or_default()
            .push(index);
        for anchor in &profile.anchors {
            buckets
                .entry(format!("1:anchor:{anchor}"))
                .or_default()
                .push(index);
        }
        // Structural retrieval remains independent of renamed callees and APIs.
        let mut hashes: Vec<u64> = profile.features.sets[0]
            .iter()
            .chain(profile.features.sets[4].iter())
            .map(|feature| feature_hash(feature))
            .collect();
        hashes.sort_unstable();
        hashes.dedup();
        if hashes.is_empty() {
            continue;
        }
        let signature = minhash_signature(&hashes, 48);
        for (band, values) in signature.as_chunks::<2>().0.iter().enumerate() {
            buckets
                .entry(format!(
                    "2:lsh:{band:02}:{:016x}:{:016x}",
                    values[0], values[1]
                ))
                .or_default()
                .push(index);
        }
        signatures[index] = signature;
    }
    (buckets, signatures)
}

#[derive(Default)]
struct PairCoverage {
    oversized_buckets: usize,
    oversized_pair_upper_bound: usize,
    unprocessed_buckets: usize,
    pair_limit_hit: bool,
    bucket_count: usize,
    refined_buckets: usize,
    unresolved_oversized_buckets: usize,
    bound_pruned_pair_attempts: usize,
}

impl PairCoverage {
    fn write_counts(&self, coverage: &Bound<'_, PyDict>) -> PyResult<()> {
        for (key, value) in [
            ("oversized_buckets", self.oversized_buckets),
            (
                "oversized_bucket_pair_upper_bound",
                self.oversized_pair_upper_bound,
            ),
            ("unprocessed_buckets", self.unprocessed_buckets),
            ("bucket_count", self.bucket_count),
            ("refined_buckets", self.refined_buckets),
            (
                "unresolved_oversized_buckets",
                self.unresolved_oversized_buckets,
            ),
            (
                "bound_pruned_pair_attempts",
                self.bound_pruned_pair_attempts,
            ),
        ] {
            coverage.set_item(key, value)?;
        }
        Ok(())
    }
}

/// Extra bands subdivide crowded retrieval buckets; this is still approximate
/// retrieval, not proof that cross-partition pairs are below the score cutoff.
fn refine_bucket(
    members: &[usize],
    signatures: &[Vec<u64>],
    max_bucket: usize,
    coverage: &mut PairCoverage,
) -> Vec<Vec<usize>> {
    let mut pending = VecDeque::from([(members.to_vec(), 0)]);
    let mut leaves = Vec::new();
    while let Some((members, band)) = pending.pop_front() {
        if members.len() <= max_bucket {
            leaves.push(members);
        } else if band >= 24 || members.iter().any(|&index| signatures[index].is_empty()) {
            coverage.unresolved_oversized_buckets += 1;
        } else {
            coverage.refined_buckets += 1;
            let mut split: BTreeMap<(u64, u64), Vec<usize>> = BTreeMap::new();
            for index in members {
                let signature = &signatures[index];
                split
                    .entry((signature[band * 2], signature[band * 2 + 1]))
                    .or_default()
                    .push(index);
            }
            pending.extend(split.into_values().map(|members| (members, band + 1)));
        }
    }
    leaves
}

fn admit_pairs(
    members: &[usize],
    profiles: &[AbstractionProfile],
    pairs: &mut BTreeSet<(usize, usize)>,
    max_pairs: usize,
    minimum: f64,
    coverage: &mut PairCoverage,
) -> bool {
    for left in 0..members.len() {
        for right in left + 1..members.len() {
            let pair = (
                members[left].min(members[right]),
                members[left].max(members[right]),
            );
            if pairs.contains(&pair) {
                continue;
            }
            if abstraction_upper_bound(&profiles[pair.0].features, &profiles[pair.1].features)
                .is_none_or(|bound| bound < minimum)
            {
                coverage.bound_pruned_pair_attempts += 1;
                continue;
            }
            if pairs.len() >= max_pairs {
                coverage.pair_limit_hit = true;
                return false;
            }
            pairs.insert(pair);
        }
    }
    true
}

fn candidate_pairs(
    profiles: &[AbstractionProfile],
    max_pairs: usize,
    max_bucket: usize,
    exhaustive: bool,
    minimum: f64,
) -> (BTreeSet<(usize, usize)>, PairCoverage) {
    let eligible: Vec<usize> = profiles
        .iter()
        .enumerate()
        .filter_map(|(index, profile)| profile.eligible.then_some(index))
        .collect();
    let mut coverage = PairCoverage::default();
    let (buckets, signatures) = if exhaustive {
        (
            BTreeMap::from([("exhaustive".to_owned(), eligible)]),
            Vec::new(),
        )
    } else {
        abstraction_buckets(profiles)
    };
    coverage.bucket_count = buckets.len();
    let mut pairs = BTreeSet::new();
    for (position, members) in buckets.values().enumerate() {
        if !exhaustive && members.len() > max_bucket {
            coverage.oversized_buckets += 1;
            coverage.oversized_pair_upper_bound += members
                .len()
                .saturating_mul(members.len().saturating_sub(1))
                / 2;
        }
        let leaves = if !exhaustive && members.len() > max_bucket {
            refine_bucket(members, &signatures, max_bucket, &mut coverage)
        } else {
            vec![members.clone()]
        };
        for leaf in leaves {
            if !admit_pairs(
                &leaf,
                profiles,
                &mut pairs,
                max_pairs,
                minimum,
                &mut coverage,
            ) {
                coverage.unprocessed_buckets = buckets.len() - position;
                return (pairs, coverage);
            }
        }
    }
    (pairs, coverage)
}

struct AbstractionEdge {
    left: usize,
    right: usize,
    similarity: Similarity,
    shape: bool,
    differences: Vec<String>,
}

fn score_abstractions(
    profiles: &[AbstractionProfile],
    pairs: &BTreeSet<(usize, usize)>,
    minimum: f64,
    shared: usize,
) -> (Vec<AbstractionEdge>, usize) {
    let mut edges = Vec::new();
    let mut normalized = 0;
    for &(left, right) in pairs {
        let a = &profiles[left];
        let b = &profiles[right];
        if a.normalized == b.normalized && a.semantic == b.semantic {
            normalized += 1;
            continue;
        }
        let similarity = compare(&a.features, &b.features);
        if similarity.score < minimum || similarity.shared_features < shared {
            continue;
        }
        let keys: BTreeSet<&String> = a.semantic.keys().chain(b.semantic.keys()).collect();
        let mut differences = if a.normalized == b.normalized {
            Vec::new()
        } else {
            vec!["normalized_ast".to_owned()]
        };
        differences.extend(
            keys.into_iter()
                .filter(|key| a.semantic.get(*key) != b.semantic.get(*key))
                .cloned(),
        );
        edges.push(AbstractionEdge {
            left,
            right,
            similarity,
            shape: a.shape == b.shape,
            differences,
        });
    }
    edges.sort_by(|a, b| {
        b.similarity
            .score
            .total_cmp(&a.similarity.score)
            .then_with(|| (a.left, a.right).cmp(&(b.left, b.right)))
    });
    (edges, normalized)
}

fn edge_to_dict<'py>(py: Python<'py>, edge: &AbstractionEdge) -> PyResult<Bound<'py, PyDict>> {
    let output = PyDict::new(py);
    output.set_item("left", edge.left)?;
    output.set_item("right", edge.right)?;
    output.set_item("score", edge.similarity.score)?;
    output.set_item("containment", edge.similarity.containment)?;
    output.set_item("shared_features", edge.similarity.shared_features)?;
    let components = PyDict::new(py);
    for (key, value) in &edge.similarity.components {
        components.set_item(key, value)?;
    }
    output.set_item("components", components)?;
    output.set_item(
        "evidence_kind",
        if edge.shape { "shape" } else { "structural" },
    )?;
    output.set_item("differences", &edge.differences)?;
    output.set_item("review_only", true)?;
    Ok(output)
}

#[pyfunction]
#[pyo3(signature = (profiles, min_similarity=0.80, min_shared_features=8, max_pairs=2_000_000, max_bucket_size=80, exhaustive=false))]
pub(crate) fn audit_abstraction_pairs(
    py: Python<'_>,
    profiles: &Bound<'_, PyList>,
    min_similarity: f64,
    min_shared_features: usize,
    max_pairs: usize,
    max_bucket_size: usize,
    exhaustive: bool,
) -> PyResult<Py<PyDict>> {
    if !min_similarity.is_finite()
        || !(0.0..=1.0).contains(&min_similarity)
        || min_shared_features == 0
        || max_pairs == 0
        || max_bucket_size < 2
    {
        return Err(PyValueError::new_err(
            "require similarity in [0,1], positive shared features/pair limit, bucket limit >=2",
        ));
    }
    let profiles = profiles
        .iter()
        .map(|value| abstraction_profile(&value))
        .collect::<PyResult<Vec<_>>>()?;
    let count = profiles.len();
    let eligible = profiles.iter().filter(|profile| profile.eligible).count();
    let (edges, normalized, pair_count, stats) = py.detach(move || {
        let (pairs, stats) = candidate_pairs(
            &profiles,
            max_pairs,
            max_bucket_size,
            exhaustive,
            min_similarity,
        );
        let (edges, normalized) =
            score_abstractions(&profiles, &pairs, min_similarity, min_shared_features);
        (edges, normalized, pairs.len(), stats)
    });
    let output = PyDict::new(py);
    let rows = PyList::empty(py);
    for edge in &edges {
        rows.append(edge_to_dict(py, edge)?)?;
    }
    output.set_item("edges", rows)?;
    let coverage = PyDict::new(py);
    stats.write_counts(&coverage)?;
    for (key, value) in [
        ("input_profiles", count),
        ("eligible_profiles", eligible),
        ("ineligible_profiles", count - eligible),
        (
            "possible_pairs",
            eligible.saturating_mul(eligible.saturating_sub(1)) / 2,
        ),
        ("candidate_pairs", pair_count),
        ("scored_pairs", pair_count - normalized),
        ("normalized_clone_pairs_excluded", normalized),
        ("reported_edges", edges.len()),
        ("max_pairs", max_pairs),
        ("max_bucket_size", max_bucket_size),
        ("min_shared_features", min_shared_features),
        ("permutations", 48),
        ("band_size", 2),
    ] {
        coverage.set_item(key, value)?;
    }
    coverage.set_item("min_similarity", min_similarity)?;
    coverage.set_item("pair_limit_hit", stats.pair_limit_hit)?;
    coverage.set_item(
        "candidate_generation_complete",
        !stats.pair_limit_hit && stats.unresolved_oversized_buckets == 0,
    )?;
    coverage.set_item("complete", exhaustive && !stats.pair_limit_hit)?;
    coverage.set_item("exhaustive", exhaustive)?;
    coverage.set_item(
        "recall",
        if exhaustive && !stats.pair_limit_hit {
            "exhaustive_for_eligible_profiles"
        } else {
            "unknown"
        },
    )?;
    coverage.set_item("review_only", true)?;
    coverage.set_item("retrieval", "coarse shapes, call/schema anchors and 24 two-row MinHash bands; crowded buckets subdivide by extra bands; rounded safe upper bound prunes before pair admission")?;
    coverage.set_item(
        "eligibility",
        "at least two body statements and two operation/control/call AST nodes",
    )?;
    output.set_item("coverage", coverage)?;
    Ok(output.unbind())
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
