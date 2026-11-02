use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Workload {
    pub revision: String,
    pub files: BTreeMap<String, String>,
    pub queries: Vec<Intent>,
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Intent {
    pub id: String,
    pub query: String,
    pub relevant: Vec<Label>,
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Label {
    pub path: String,
    pub symbol: String,
    pub line_start: usize,
    pub line_end: usize,
    pub reason: String,
}

impl Workload {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.queries.len() == 24,
            "all 24 frozen queries are required"
        );
        let mut ids = BTreeSet::new();
        for (n, query) in self.queries.iter().enumerate() {
            ensure!(
                query.id == format!("q{:02}", n + 1),
                "query order/identity changed"
            );
            ensure!(
                ids.insert(&query.id) && !query.query.trim().is_empty(),
                "invalid query"
            );
            ensure!(
                !query.relevant.is_empty(),
                "missing labels for {}",
                query.id
            );
            for label in &query.relevant {
                ensure!(self.files.contains_key(&label.path), "unbound label path");
                ensure!(
                    label.line_start > 0 && label.line_end >= label.line_start,
                    "invalid labeled line range"
                );
            }
        }
        Ok(())
    }
}

/// Convert inclusive one-based source lines to a half-open byte range.
pub fn line_bytes(source: &str, start: usize, end: usize) -> Result<(usize, usize)> {
    ensure!(start > 0 && end >= start, "invalid line interval");
    let offsets: Vec<_> = std::iter::once(0)
        .chain(source.match_indices('\n').map(|(i, _)| i + 1))
        .filter(|&i| i < source.len())
        .collect();
    let low = *offsets
        .get(start - 1)
        .context("label start exceeds source")?;
    ensure!(end <= offsets.len(), "label end exceeds source");
    Ok((low, offsets.get(end).copied().unwrap_or(source.len())))
}

pub fn overlaps(hit: (&str, usize, usize), label: (&str, usize, usize)) -> bool {
    hit.0 == label.0 && hit.1 < label.2 && label.1 < hit.2
}

/// Distinct approximate rows recovered from the nonempty exact top-k reference.
pub fn recall(exact: &[u32], approximate: &[u32]) -> Result<f64> {
    ensure!(!exact.is_empty(), "empty exact reference");
    let reference: BTreeSet<_> = exact.iter().copied().collect();
    ensure!(reference.len() == exact.len(), "duplicate exact row");
    let recovered: BTreeSet<_> = approximate
        .iter()
        .copied()
        .filter(|row| reference.contains(row))
        .collect();
    Ok(recovered.len() as f64 / reference.len() as f64)
}

/// Missing/failed rows stay in the requested denominator; latency uses successes.
pub fn summarize(rows: &[&serde_json::Value], requested: usize) -> serde_json::Value {
    let valid_success = |r: &&serde_json::Value| {
        r["status"] == "completed"
            && r["observation_success"] == true
            && r["latency_us"]
                .as_f64()
                .is_some_and(|t| t.is_finite() && t >= 0.0)
    };
    let successes: Vec<_> = rows.iter().copied().filter(valid_success).collect();
    let failed = rows
        .iter()
        .copied()
        .filter(|r| r["status"] == "failed" || (r["status"] == "completed" && !valid_success(r)))
        .count();
    let censored = rows.iter().filter(|r| r["status"] == "censored").count();
    let count = |field: &str| {
        successes
            .iter()
            .filter(|r| r[field].as_bool() == Some(true))
            .count()
    };
    let mut times: Vec<_> = successes
        .iter()
        .filter_map(|r| r["latency_us"].as_f64())
        .collect();
    times.sort_by(f64::total_cmp);
    let quantile = |p: f64| -> Option<f64> {
        (!times.is_empty()).then(|| times[((times.len() - 1) as f64 * p).ceil() as usize])
    };
    serde_json::json!({"requested":requested,"completed":successes.len(),
        "failed":failed,"censored":censored,
        "unstarted":requested.saturating_sub(successes.len()+failed+censored),
        "path_hit_at_10":count("path_hit_at_10") as f64/requested as f64,
        "span_hit_at_10":count("span_hit_at_10") as f64/requested as f64,
        "all_label_spans_hit_at_10":count("all_label_spans_hit_at_10") as f64/requested as f64,
        "latency_us_p50_within_budget_successes_only":quantile(0.5),"latency_us_p95_within_budget_successes_only":quantile(0.95)})
}

/// Retrieval accounting partitions all requests; censoring preserves observation success separately.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Completed,
    Failed,
    Unstarted,
    Censored,
}
impl Outcome {
    pub fn classify(admitted: bool, success: Option<bool>, within_budget: bool) -> Result<Self> {
        ensure!(
            admitted == success.is_some(),
            "admission/observation mismatch"
        );
        Ok(if !admitted {
            Self::Unstarted
        } else if !within_budget {
            Self::Censored
        } else if success == Some(true) {
            Self::Completed
        } else {
            Self::Failed
        })
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Unstarted => "unstarted",
            Self::Censored => "censored",
        }
    }
}

pub fn validated_index_path(
    source: &std::path::Path,
    index: &std::path::Path,
) -> Result<std::path::PathBuf> {
    use std::path::{Component, PathBuf};
    ensure!(
        index.is_absolute()
            && index
                .components()
                .all(|c| matches!(c, Component::RootDir | Component::Normal(_))),
        "index requires normalized absolute components"
    );
    let normalized: PathBuf = index.components().collect();
    ensure!(
        normalized.as_os_str() == index.as_os_str(),
        "index must be lexically normalized"
    );
    let source = source.canonicalize()?;
    let parent = index
        .parent()
        .context("index requires parent")?
        .canonicalize()?;
    let checked = parent.join(index.file_name().context("index requires directory name")?);
    ensure!(
        !checked.starts_with(&source),
        "index resolves inside source"
    );
    match std::fs::symlink_metadata(&checked) {
        Ok(m) => ensure!(
            m.is_dir() && !m.file_type().is_symlink(),
            "index must be a real directory"
        ),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    Ok(checked)
}
