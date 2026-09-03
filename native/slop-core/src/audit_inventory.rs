//! Deterministic construction of audit-only structural candidates.
//!
//! Python retains repository scanning and every policy-bearing decision. This module
//! receives pre-measured records and performs the allocation-heavy formatting,
//! stable identity hashing, and bounded ranking used by the audit inventory.

use sha2::{Digest, Sha256};
use std::cmp::Ordering;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use pyo3::exceptions::{PyKeyError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};

#[derive(Debug)]
pub struct Candidate {
    pub id: String,
    pub category: &'static str,
    pub severity: &'static str,
    pub confidence: f64,
    pub value: i64,
    pub files: Vec<String>,
    pub location: String,
    pub evidence: String,
    pub family_shape: Option<FamilyShape>,
}

#[derive(Debug)]
pub struct FamilyShape {
    pub before_loc: i64,
    pub after_loc: i64,
    pub target_shape: String,
}

#[derive(Debug)]
pub struct CloneSite {
    pub file: String,
    pub line_start: i64,
    pub line_end: i64,
    pub name: String,
}

#[derive(Debug)]
pub struct CloneCluster {
    pub kind: String,
    pub n_sites: i64,
    pub est_bytes: i64,
    pub confidence: f64,
    pub value_score: i64,
    pub disposition: String,
    pub rationale: String,
    pub sites: Vec<CloneSite>,
}

#[derive(Debug)]
pub struct FileFamily {
    pub files: Vec<String>,
    pub similarity_min_display: String,
    pub similarity_avg_display: String,
    pub containment_min_display: String,
    pub band: String,
    pub recommended_abstraction: String,
    pub suggested_home: Option<String>,
    pub estimated_net_deleted_loc: i64,
    pub confidence: f64,
    pub before_loc: i64,
    pub after_loc: i64,
    pub target_shape: String,
    pub shared_methods_repr: String,
    pub variable_methods_repr: String,
}

#[derive(Debug)]
pub struct RuffFinding {
    pub filename: String,
    pub row: String,
    pub code: String,
    pub message: String,
}

#[derive(Debug, Default)]
pub struct StructuralCandidates {
    pub dead_code: Vec<Candidate>,
    pub god_files: Vec<Candidate>,
    pub god_functions: Vec<Candidate>,
    pub duplication: Vec<Candidate>,
    pub file_families: Vec<Candidate>,
    pub imports_deps: Vec<Candidate>,
}

#[allow(clippy::too_many_arguments)]
fn candidate(
    id: String,
    category: &'static str,
    severity: &'static str,
    confidence: f64,
    value: i64,
    files: Vec<String>,
    location: String,
    evidence: String,
) -> Candidate {
    Candidate {
        id,
        category,
        severity,
        confidence,
        value,
        files,
        location,
        evidence,
        family_shape: None,
    }
}

fn stable_id(parts: impl IntoIterator<Item = String>) -> String {
    let mut sorted: Vec<String> = parts.into_iter().collect();
    sorted.sort();
    let mut hash = Sha256::new();
    for (index, part) in sorted.iter().enumerate() {
        if index != 0 {
            hash.update([0]);
        }
        hash.update(part.as_bytes());
    }
    let digest = hash.finalize();
    let mut out = String::with_capacity(20);
    for byte in &digest[..10] {
        use std::fmt::Write as _;
        write!(&mut out, "{byte:02x}").expect("writing to a String cannot fail");
    }
    out
}

fn relative_path(path_text: &str, repo: &Path) -> Result<String, String> {
    let path = PathBuf::from(path_text);
    let relative = if path.is_absolute() {
        path.strip_prefix(repo).map_err(|_| {
            format!(
                "audit candidate path {path_text:?} is outside repository {:?}",
                repo.display()
            )
        })?
    } else {
        path.as_path()
    };
    let normalized: PathBuf = relative
        .components()
        .filter(|component| !matches!(component, std::path::Component::CurDir))
        .collect();
    Ok(normalized.to_string_lossy().replace('\\', "/"))
}

fn god_file(item: &str, repo: &Path, god_file_lines: i64) -> Result<Candidate, String> {
    let (path_text, lines_text) = item
        .rsplit_once(" (")
        .ok_or_else(|| format!("malformed god-file measurement: {item:?}"))?;
    let lines = lines_text
        .strip_suffix(')')
        .ok_or_else(|| format!("malformed god-file line count: {item:?}"))?
        .parse::<i64>()
        .map_err(|error| format!("invalid god-file line count in {item:?}: {error}"))?;
    let rel = relative_path(path_text, repo)?;
    Ok(candidate(
        format!("god-file:{rel}"),
        "god_files",
        "high",
        1.0,
        lines - god_file_lines,
        vec![rel.clone()],
        rel,
        format!("{lines} lines; threshold is {god_file_lines}"),
    ))
}

fn god_function(item: &str, repo: &Path, god_func_lines: i64) -> Result<Option<Candidate>, String> {
    let Some((body, span_text)) = item.rsplit_once(" (") else {
        return Ok(None);
    };
    let Some(span_text) = span_text.strip_suffix(')') else {
        return Ok(None);
    };
    let Some((separator, line, name)) = body.match_indices(':').find_map(|(separator, _)| {
        let suffix = &body[separator + 1..];
        let (line, name) = suffix.split_once(' ')?;
        line.parse::<i64>().ok().map(|_| (separator, line, name))
    }) else {
        return Ok(None);
    };
    let path_text = &body[..separator];
    let Ok(span) = span_text.parse::<i64>() else {
        return Ok(None);
    };
    let rel = relative_path(path_text, repo)?;
    Ok(Some(candidate(
        format!("god-function:{rel}:{line}:{name}"),
        "god_functions",
        "high",
        1.0,
        span - god_func_lines,
        vec![rel.clone()],
        format!("{rel}:{line}"),
        format!("{name} spans {span_text} lines; threshold is {god_func_lines}"),
    )))
}

fn clone_candidate(cluster: CloneCluster) -> Option<Candidate> {
    if cluster.disposition == "ignore" {
        return None;
    }
    let identity = cluster.sites.iter().map(|site| {
        format!(
            "{}:{}-{}:{}",
            site.file, site.line_start, site.line_end, site.name
        )
    });
    let id = stable_id(identity);
    let files = cluster
        .sites
        .iter()
        .map(|site| site.file.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let location = cluster
        .sites
        .iter()
        .take(8)
        .map(|site| format!("{}:{}", site.file, site.line_start))
        .collect::<Vec<_>>()
        .join(", ");
    Some(candidate(
        format!("reuse:{id}"),
        "duplication",
        "medium",
        cluster.confidence,
        cluster.value_score,
        files,
        location,
        format!(
            "{} clone; {} sites; {} redundant units; {}; disposition={}",
            cluster.kind,
            cluster.n_sites,
            cluster.est_bytes,
            cluster.rationale,
            cluster.disposition
        ),
    ))
}

fn family_candidate(family: FileFamily) -> Candidate {
    let id = stable_id(family.files.iter().cloned());
    let severity = if family.estimated_net_deleted_loc >= 200 {
        "high"
    } else {
        "medium"
    };
    let target_shape = if family.target_shape.is_empty() {
        "worker-required"
    } else {
        &family.target_shape
    };
    let suggested_home = family
        .suggested_home
        .as_deref()
        .unwrap_or("leader-selected");
    let mut out = candidate(
        format!("family:{id}"),
        "file_families",
        severity,
        family.confidence,
        family.estimated_net_deleted_loc,
        family.files.clone(),
        family.files.join(", "),
        format!(
            "{} family; min/avg similarity {}/{}; containment>={}; \
             recommend={}; before_loc={}; after_loc={}; target_shape={}; \
             net_deleted_loc={}; shared_methods={}; variable_methods={}; \
             suggested_home={}",
            family.band,
            family.similarity_min_display,
            family.similarity_avg_display,
            family.containment_min_display,
            family.recommended_abstraction,
            family.before_loc,
            family.after_loc,
            target_shape,
            family.estimated_net_deleted_loc,
            family.shared_methods_repr,
            family.variable_methods_repr,
            suggested_home
        ),
    );
    out.family_shape = Some(FamilyShape {
        before_loc: family.before_loc,
        after_loc: family.after_loc,
        target_shape: family.target_shape,
    });
    out
}

fn dead_code(line: &str, repo: &Path) -> Result<Option<Candidate>, String> {
    let Some((body, confidence_text)) = line.rsplit_once(" (") else {
        return Ok(None);
    };
    let Some(confidence_text) = confidence_text.strip_suffix("% confidence)") else {
        return Ok(None);
    };
    let Some((location, message)) = body.split_once(": ") else {
        return Ok(None);
    };
    let Some((path_text, line_number)) = location.rsplit_once(':') else {
        return Ok(None);
    };
    if line_number.parse::<i64>().is_err() {
        return Ok(None);
    }
    let Ok(confidence_percent) = confidence_text.parse::<i64>() else {
        return Ok(None);
    };
    let rel = relative_path(path_text, repo)?;
    let confidence = confidence_percent as f64 / 100.0;
    Ok(Some(candidate(
        format!("dead:{rel}:{line_number}"),
        "dead_code",
        "medium",
        confidence,
        (2 * confidence_percent + 2) / 5,
        vec![rel.clone()],
        format!("{rel}:{line_number}"),
        message.to_owned(),
    )))
}

fn ruff_candidate(finding: RuffFinding, repo: &Path) -> Result<Option<Candidate>, String> {
    if finding.filename.is_empty() {
        return Ok(None);
    }
    let rel = relative_path(&finding.filename, repo)?;
    Ok(Some(candidate(
        format!("ruff:{}:{rel}:{}", finding.code, finding.row),
        "imports_deps",
        "low",
        1.0,
        10,
        vec![rel.clone()],
        format!("{rel}:{}", finding.row),
        format!("{}: {}", finding.code, finding.message),
    )))
}

#[allow(clippy::too_many_arguments)]
pub fn build(
    repo: &Path,
    god_file_lines: i64,
    god_func_lines: i64,
    god_files: Vec<String>,
    god_functions: Vec<String>,
    clusters: Vec<CloneCluster>,
    families: Vec<FileFamily>,
    vulture: Vec<String>,
    ruff: Vec<RuffFinding>,
) -> Result<StructuralCandidates, String> {
    Ok(StructuralCandidates {
        god_files: god_files
            .iter()
            .map(|item| god_file(item, repo, god_file_lines))
            .collect::<Result<_, _>>()?,
        god_functions: god_functions
            .iter()
            .filter_map(|item| god_function(item, repo, god_func_lines).transpose())
            .collect::<Result<_, _>>()?,
        duplication: clusters.into_iter().filter_map(clone_candidate).collect(),
        file_families: families.into_iter().map(family_candidate).collect(),
        dead_code: vulture
            .iter()
            .filter_map(|line| dead_code(line, repo).transpose())
            .collect::<Result<_, _>>()?,
        imports_deps: ruff
            .into_iter()
            .filter_map(|finding| ruff_candidate(finding, repo).transpose())
            .collect::<Result<_, _>>()?,
    })
}

pub fn rank_key(left: (i64, f64), right: (i64, f64)) -> Ordering {
    right
        .0
        .cmp(&left.0)
        .then_with(|| right.1.partial_cmp(&left.1).unwrap_or(Ordering::Equal))
}

fn string_attr(obj: &Bound<'_, PyAny>, name: &str) -> PyResult<String> {
    Ok(obj.getattr(name)?.str()?.to_string_lossy().into_owned())
}

fn optional_truthy_string_attr(obj: &Bound<'_, PyAny>, name: &str) -> PyResult<Option<String>> {
    let value = obj.getattr(name)?;
    if value.is_truthy()? {
        Ok(Some(value.str()?.to_string_lossy().into_owned()))
    } else {
        Ok(None)
    }
}

fn percentage_attr(obj: &Bound<'_, PyAny>, name: &str) -> PyResult<String> {
    obj.getattr(name)?
        .call_method1("__format__", (".1%",))?
        .extract()
}

fn clone_clusters(raw: &Bound<'_, PyList>) -> PyResult<Vec<CloneCluster>> {
    raw.iter()
        .map(|obj| {
            let sites = obj.getattr("sites")?;
            let sites = sites.cast::<PyList>()?;
            let sites = sites
                .iter()
                .map(|site| {
                    Ok(CloneSite {
                        file: string_attr(&site, "file")?,
                        line_start: site.getattr("line_start")?.extract()?,
                        line_end: site.getattr("line_end")?.extract()?,
                        name: string_attr(&site, "name")?,
                    })
                })
                .collect::<PyResult<_>>()?;
            Ok(CloneCluster {
                kind: string_attr(&obj, "kind")?,
                n_sites: obj.getattr("n_sites")?.extract()?,
                est_bytes: obj.getattr("est_bytes")?.extract()?,
                confidence: obj.getattr("confidence")?.extract()?,
                value_score: obj.getattr("value_score")?.extract()?,
                disposition: string_attr(&obj, "disposition")?,
                rationale: string_attr(&obj, "rationale")?,
                sites,
            })
        })
        .collect()
}

fn file_families(raw: &Bound<'_, PyList>) -> PyResult<Vec<FileFamily>> {
    raw.iter()
        .map(|obj| {
            let shared_methods = obj.getattr("shared_methods")?;
            let variable_methods = obj.getattr("variable_methods")?;
            Ok(FileFamily {
                files: obj.getattr("files")?.extract()?,
                similarity_min_display: percentage_attr(&obj, "similarity_min")?,
                similarity_avg_display: percentage_attr(&obj, "similarity_avg")?,
                containment_min_display: percentage_attr(&obj, "containment_min")?,
                band: string_attr(&obj, "band")?,
                recommended_abstraction: string_attr(&obj, "recommended_abstraction")?,
                suggested_home: optional_truthy_string_attr(&obj, "suggested_home")?,
                estimated_net_deleted_loc: obj.getattr("estimated_net_deleted_loc")?.extract()?,
                confidence: obj.getattr("confidence")?.extract()?,
                before_loc: obj.getattr("before_loc")?.extract()?,
                after_loc: obj.getattr("after_loc")?.extract()?,
                target_shape: optional_truthy_string_attr(&obj, "target_shape")?
                    .unwrap_or_default(),
                shared_methods_repr: shared_methods.repr()?.to_string_lossy().into_owned(),
                variable_methods_repr: variable_methods.repr()?.to_string_lossy().into_owned(),
            })
        })
        .collect()
}

fn ruff_findings(py: Python<'_>, raw: &Bound<'_, PyList>) -> PyResult<Vec<RuffFinding>> {
    raw.iter()
        .map(|obj| {
            let get = |key: &str, fallback: &str| -> PyResult<String> {
                Ok(obj
                    .call_method1("get", (key, fallback))?
                    .str()?
                    .to_string_lossy()
                    .into_owned())
            };
            let empty = PyDict::new(py);
            let location = obj.call_method1("get", ("location", empty))?;
            let row = location
                .call_method1("get", ("row", 1))?
                .str()?
                .to_string_lossy()
                .into_owned();
            Ok(RuffFinding {
                filename: get("filename", "")?,
                row,
                code: get("code", "F")?,
                message: get("message", "")?,
            })
        })
        .collect()
}

fn candidate_dict<'py>(py: Python<'py>, candidate: Candidate) -> PyResult<Bound<'py, PyDict>> {
    let out = PyDict::new(py);
    out.set_item("id", candidate.id)?;
    out.set_item("category", candidate.category)?;
    out.set_item("severity", candidate.severity)?;
    out.set_item("confidence", candidate.confidence)?;
    out.set_item("value", candidate.value)?;
    out.set_item("files", candidate.files)?;
    out.set_item("location", candidate.location)?;
    out.set_item("evidence", candidate.evidence)?;
    if let Some(shape) = candidate.family_shape {
        out.set_item("before_loc", shape.before_loc)?;
        out.set_item("after_loc", shape.after_loc)?;
        out.set_item("target_shape", shape.target_shape)?;
    }
    Ok(out)
}

fn candidate_objects(py: Python<'_>, candidates: Vec<Candidate>) -> PyResult<Vec<Py<PyAny>>> {
    candidates
        .into_iter()
        .map(|candidate| Ok(candidate_dict(py, candidate)?.unbind().into_any()))
        .collect()
}

fn list_objects(raw: &Bound<'_, PyList>) -> Vec<Py<PyAny>> {
    raw.iter().map(Bound::unbind).collect()
}

struct Ranked {
    value: i64,
    confidence: f64,
    object: Py<PyAny>,
}

fn rank(py: Python<'_>, objects: Vec<Py<PyAny>>) -> PyResult<Vec<Py<PyAny>>> {
    let mut ranked = objects
        .into_iter()
        .map(|object| {
            let candidate = object.bind(py).cast::<PyDict>()?;
            let value = candidate
                .get_item("value")?
                .ok_or_else(|| PyKeyError::new_err("value"))?
                .extract()?;
            let confidence = candidate
                .get_item("confidence")?
                .ok_or_else(|| PyKeyError::new_err("confidence"))?
                .extract()?;
            Ok(Ranked {
                value,
                confidence,
                object,
            })
        })
        .collect::<PyResult<Vec<_>>>()?;
    ranked.sort_by(|left, right| {
        rank_key(
            (left.value, left.confidence),
            (right.value, right.confidence),
        )
    });
    Ok(ranked.into_iter().map(|item| item.object).collect())
}

fn slice_end(len: usize, limit: isize) -> usize {
    if limit >= 0 {
        len.min(limit as usize)
    } else {
        len.saturating_sub(limit.unsigned_abs())
    }
}

fn as_list<'py>(
    py: Python<'py>,
    objects: &[Py<PyAny>],
    limit: isize,
) -> PyResult<Bound<'py, PyList>> {
    let end = slice_end(objects.len(), limit);
    PyList::new(py, objects[..end].iter().map(|item| item.bind(py)))
}

fn replace_list(py: Python<'_>, target: &Bound<'_, PyList>, objects: &[Py<PyAny>]) -> PyResult<()> {
    let replacement = PyList::new(py, objects.iter().map(|item| item.bind(py)))?;
    target.set_slice(0, target.len(), replacement.as_any())
}

#[pyfunction]
#[pyo3(signature = (*, repo, god_file_lines, god_func_lines, god_files, god_functions, clusters, families, vulture, ruff, fallbacks, token_clones, native_reuse, dependencies, limit))]
#[allow(clippy::too_many_arguments)]
pub fn audit_inventory_candidates(
    py: Python<'_>,
    repo: String,
    god_file_lines: i64,
    god_func_lines: i64,
    god_files: Vec<String>,
    god_functions: Vec<String>,
    clusters: &Bound<'_, PyList>,
    families: &Bound<'_, PyList>,
    vulture: Vec<String>,
    ruff: &Bound<'_, PyList>,
    fallbacks: &Bound<'_, PyList>,
    token_clones: &Bound<'_, PyList>,
    native_reuse: &Bound<'_, PyList>,
    dependencies: &Bound<'_, PyList>,
    limit: isize,
) -> PyResult<Py<PyDict>> {
    let clusters = clone_clusters(clusters)?;
    let families = file_families(families)?;
    let ruff = ruff_findings(py, ruff)?;
    let repo_path = PathBuf::from(repo);
    let built = py
        .detach(move || {
            build(
                &repo_path,
                god_file_lines,
                god_func_lines,
                god_files,
                god_functions,
                clusters,
                families,
                vulture,
                ruff,
            )
        })
        .map_err(PyValueError::new_err)?;

    let dead_code = rank(py, candidate_objects(py, built.dead_code)?)?;
    let god_files = rank(py, candidate_objects(py, built.god_files)?)?;
    let god_functions = rank(py, candidate_objects(py, built.god_functions)?)?;

    let mut duplication = candidate_objects(py, built.duplication)?;
    duplication.extend(list_objects(token_clones));
    let duplication = rank(py, duplication)?;

    let file_families = rank(py, candidate_objects(py, built.file_families)?)?;

    let fallback_objects = list_objects(fallbacks);
    let fallbacks_ranked = rank(py, fallback_objects)?;
    replace_list(py, fallbacks, &fallbacks_ranked)?;

    let native_objects = list_objects(native_reuse);
    let perf_objects = native_objects
        .iter()
        .filter_map(|object| {
            let candidate = object.bind(py).cast::<PyDict>().ok()?;
            let complete = candidate.get_item("evidence_complete").ok()??;
            complete
                .is_truthy()
                .ok()
                .filter(|truthy| *truthy)
                .map(|_| object.clone_ref(py))
        })
        .collect();
    let perf_hotspots = rank(py, perf_objects)?;
    let native_ranked = rank(py, native_objects)?;
    replace_list(py, native_reuse, &native_ranked)?;

    let mut imports_deps = candidate_objects(py, built.imports_deps)?;
    imports_deps.extend(list_objects(dependencies));
    let imports_deps = rank(py, imports_deps)?;

    let out = PyDict::new(py);
    out.set_item("dead_code", as_list(py, &dead_code, limit)?)?;
    out.set_item("god_files", as_list(py, &god_files, limit)?)?;
    out.set_item("god_functions", as_list(py, &god_functions, limit)?)?;
    out.set_item("duplication", as_list(py, &duplication, limit)?)?;
    out.set_item("file_families", as_list(py, &file_families, limit)?)?;
    out.set_item("silent_fallbacks", as_list(py, &fallbacks_ranked, limit)?)?;
    out.set_item("perf_hotspots", as_list(py, &perf_hotspots, limit)?)?;
    out.set_item("imports_deps", as_list(py, &imports_deps, limit)?)?;
    out.set_item("native_reuse", as_list(py, &native_ranked, limit)?)?;
    Ok(out.unbind())
}
