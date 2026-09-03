use std::collections::{BTreeMap, BTreeSet};

use pyo3::prelude::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const COMMON_NAMES: &[&str] = &[
    "main", "run", "forward", "backward", "build", "get", "set", "load", "save", "parse", "init",
];
const NOISE: &[&str] = &[
    "native", "compiled", "kernel", "cpu", "cuda", "gpu", "torch", "py", "f16", "f32", "f64",
];

#[derive(Deserialize)]
struct SymbolRow {
    name: String,
    file: String,
    line_start: i64,
    language: String,
    params: String,
    caller_count: usize,
}

#[derive(Serialize)]
struct Candidate {
    id: String,
    category: &'static str,
    severity: &'static str,
    confidence: f64,
    value: usize,
    files: Vec<String>,
    symbols: Vec<String>,
    native_targets: Vec<String>,
    tests: Vec<String>,
    location: String,
    evidence: String,
    evidence_complete: bool,
    disposition: &'static str,
}

fn normalized_name(name: &str) -> String {
    let mut snake = String::with_capacity(name.len() + 4);
    let mut previous: Option<char> = None;
    for character in name.chars() {
        if character.is_uppercase()
            && previous.is_some_and(|value| value.is_lowercase() || value.is_ascii_digit())
        {
            snake.push('_');
        }
        for lower in character.to_lowercase() {
            snake.push(lower);
        }
        previous = Some(character);
    }
    snake
        .split('_')
        .filter(|part| !part.is_empty() && !NOISE.contains(part))
        .collect::<Vec<_>>()
        .join("_")
}

fn stable_id(row: &SymbolRow) -> String {
    let signature = if row.params.trim().is_empty() {
        "()"
    } else {
        row.params.trim()
    };
    format!("{}:{}::{}{}", row.language, row.file, row.name, signature)
}

fn fingerprint(identities: &mut [String]) -> String {
    identities.sort_unstable();
    let mut digest = Sha256::new();
    digest.update(b"native-reuse\0");
    digest.update(identities.join("\0").as_bytes());
    format!("{:x}", digest.finalize())[..20].to_owned()
}

fn candidates(rows: Vec<SymbolRow>, limit: usize) -> Vec<Candidate> {
    let mut python: BTreeMap<String, Vec<SymbolRow>> = BTreeMap::new();
    let mut native: BTreeMap<String, Vec<SymbolRow>> = BTreeMap::new();
    for row in rows {
        let key = normalized_name(&row.name);
        if key.len() < 5 || COMMON_NAMES.contains(&key.as_str()) {
            continue;
        }
        if row.language == "python" {
            python.entry(key).or_default().push(row);
        } else {
            native.entry(key).or_default().push(row);
        }
    }

    let mut output = Vec::new();
    for (name, python_sites) in python {
        let Some(native_sites) = native.get(&name) else {
            continue;
        };
        for python_site in python_sites {
            let python_id = stable_id(&python_site);
            let native_ids = native_sites.iter().map(stable_id).collect::<Vec<_>>();
            let mut identities = Vec::with_capacity(native_ids.len() + 1);
            identities.push(python_id.clone());
            identities.extend(native_ids.iter().cloned());
            let files = std::iter::once(python_site.file.clone())
                .chain(native_sites.iter().map(|site| site.file.clone()))
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
            let has_callers = python_site.caller_count > 0;
            output.push(Candidate {
                id: format!("native-reuse:{}", fingerprint(&mut identities)),
                category: "native_reuse",
                severity: if has_callers { "high" } else { "medium" },
                confidence: if has_callers { 0.82 } else { 0.68 },
                value: (35 + 5 * python_site.caller_count).min(200),
                files,
                symbols: vec![python_id],
                native_targets: native_ids.into_iter().take(8).collect(),
                tests: Vec::new(),
                location: format!("{}:{}", python_site.file, python_site.line_start),
                evidence: format!(
                    "normalized symbol family '{name}'; {} native target(s); {} direct caller(s); semantic reuse candidate, profiling required before performance-priority substitution",
                    native_sites.len(), python_site.caller_count
                ),
                evidence_complete: has_callers,
                disposition: "validate",
            });
        }
    }
    output.sort_by(|left, right| {
        right
            .value
            .cmp(&left.value)
            .then_with(|| right.confidence.total_cmp(&left.confidence))
    });
    output.truncate(limit);
    output
}

#[pyfunction]
fn native_reuse_candidates_native(
    py: Python<'_>,
    rows_json: &str,
    limit: usize,
) -> PyResult<String> {
    let rows: Vec<SymbolRow> = serde_json::from_str(rows_json)
        .map_err(|error| pyo3::exceptions::PyValueError::new_err(error.to_string()))?;
    let output = py.detach(|| candidates(rows, limit));
    serde_json::to_string(&output)
        .map_err(|error| pyo3::exceptions::PyRuntimeError::new_err(error.to_string()))
}

pub(crate) fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(native_reuse_candidates_native, module)?)?;
    Ok(())
}
