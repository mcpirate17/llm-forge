//! Bounded evaluation of captured coverage.py JSON. This does not authenticate
//! its producer; the caller must supply bytes captured by the Stage owner after
//! the supervised test/JSON processes have finished and before workspace cleanup.
use anyhow::{bail, ensure, Context, Result};
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeSet;

const MAX_JSON: usize = 16 * 1024 * 1024;
const MAX_PATHS: usize = 4096;
const MAX_LINES: usize = 1_000_000;

#[derive(Clone, Debug, Serialize)]
pub(crate) struct ChangedFile {
    pub path: String,
    pub absolute_path: String,
    pub lines: BTreeSet<u32>,
    pub high_risk: bool,
}
#[derive(Default, Debug, Serialize)]
pub(crate) struct Bucket {
    pub covered: usize,
    pub measurable: usize,
}
#[derive(Debug, Serialize)]
pub(crate) struct Coverage {
    pub high: Bucket,
    pub other: Bucket,
    pub high_pass: bool,
    pub other_pass: bool,
}

/// Union captured shard observations. Missing lines reported by one shard are
/// removed when any other shard reports them executed.
pub(crate) fn evaluate_shards(
    shards: &[(Vec<u8>, String)],
    changed: &[ChangedFile],
    high: f64,
    other: f64,
) -> Result<Coverage> {
    ensure!(
        !shards.is_empty() && shards.len() <= 256,
        "invalid coverage shard count"
    );
    let mut merged = serde_json::Map::new();
    let mut total = 0usize;
    let mut work = 0usize;
    let targets: BTreeSet<_> = changed.iter().map(|v| v.path.as_str()).collect();
    for (shard, snapshot) in shards {
        ensure!(
            snapshot.starts_with('/') && !snapshot.ends_with('/') && snapshot.len() <= 4096,
            "invalid captured shard root"
        );
        total = total
            .checked_add(shard.len())
            .context("coverage shard size overflow")?;
        ensure!(total <= MAX_JSON, "coverage shard bytes exceed limit");
        let value =
            crate::tool_schema::parse_document_with_limit(std::str::from_utf8(shard)?, MAX_JSON)?;
        let files = value
            .get("files")
            .and_then(Value::as_object)
            .context("coverage shard has no files")?;
        ensure!(
            files.len() <= MAX_PATHS,
            "coverage shard has too many files"
        );
        let mut seen = BTreeSet::new();
        for (path, record) in files {
            ensure!(path.len() <= 4096, "coverage path too long");
            let path = if path.starts_with('/') {
                let Some(relative) = path
                    .strip_prefix(snapshot)
                    .and_then(|v| v.strip_prefix('/'))
                else {
                    continue;
                };
                relative
            } else {
                path.as_str()
            };
            if !targets.contains(path) {
                continue;
            }
            ensure!(seen.insert(path), "ambiguous coverage aliases for one file");
            let (executed, missing) = record_lines(record, &mut work)?;
            let entry = merged
                .entry(path.to_owned())
                .or_insert_with(|| serde_json::json!({"executed_lines":[],"missing_lines":[]}));
            let (mut old_executed, mut old_missing) = record_lines(entry, &mut work)?;
            old_executed.extend(executed);
            old_missing.extend(missing);
            old_missing.retain(|line| !old_executed.contains(line));
            *entry =
                serde_json::json!({"executed_lines": old_executed, "missing_lines": old_missing});
            ensure!(merged.len() <= MAX_PATHS, "coverage files exceed limit");
        }
    }
    let payload = serde_json::to_vec(&serde_json::json!({"files":merged}))?;
    evaluate(&payload, changed, high, other)
}

/// Reject missing changed files instead of silently treating unmeasured source
/// as successful. Counts only changed lines reported executable by coverage.py.
pub(crate) fn evaluate(
    bytes: &[u8],
    changed: &[ChangedFile],
    high: f64,
    other: f64,
) -> Result<Coverage> {
    ensure!(bytes.len() <= MAX_JSON, "coverage JSON exceeds 16 MiB");
    ensure!(
        changed.len() <= MAX_PATHS,
        "too many changed coverage files"
    );
    ensure!(
        (0.0..=100.0).contains(&high) && (0.0..=100.0).contains(&other),
        "invalid coverage threshold"
    );
    let text = std::str::from_utf8(bytes).context("coverage JSON is not UTF-8")?;
    let document = crate::tool_schema::parse_document_with_limit(text, MAX_JSON)?;
    let files = document
        .get("files")
        .and_then(Value::as_object)
        .context("coverage JSON missing files object")?;
    ensure!(files.len() <= MAX_PATHS, "too many coverage files");
    let mut result = Coverage {
        high: Bucket::default(),
        other: Bucket::default(),
        high_pass: false,
        other_pass: false,
    };
    let mut seen = BTreeSet::new();
    let mut work = 0usize;
    for file in changed {
        ensure!(seen.insert(&file.path), "duplicate changed coverage path");
        ensure!(
            crate::candidate_source::safe_path(file.path.as_bytes()).is_ok()
                && (file.absolute_path.is_empty() || file.absolute_path.starts_with('/'))
                && file.absolute_path.len() <= 4096,
            "invalid changed coverage path"
        );
        let relative = files.get(&file.path);
        let absolute = files.get(&file.absolute_path);
        ensure!(
            relative.is_none() || absolute.is_none(),
            "ambiguous duplicate coverage path"
        );
        let record = relative
            .or(absolute)
            .with_context(|| format!("coverage missing changed file {}", file.path))?;
        let (executed, missing) = record_lines(record, &mut work)?;
        work = work
            .checked_add(file.lines.len())
            .context("coverage work overflow")?;
        ensure!(work <= MAX_LINES, "coverage line budget exceeded");
        let bucket = if file.high_risk {
            &mut result.high
        } else {
            &mut result.other
        };
        for line in &file.lines {
            if executed.contains(line) {
                bucket.covered += 1;
                bucket.measurable += 1;
            } else if missing.contains(line) {
                bucket.measurable += 1;
            }
        }
    }
    result.high_pass = passes(&result.high, high);
    result.other_pass = passes(&result.other, other);
    Ok(result)
}
fn passes(bucket: &Bucket, threshold: f64) -> bool {
    bucket.measurable == 0
        || (bucket.covered as f64) * 100.0 >= threshold * (bucket.measurable as f64)
}
fn record_lines(record: &Value, work: &mut usize) -> Result<(BTreeSet<u32>, BTreeSet<u32>)> {
    let obj = record
        .as_object()
        .context("coverage file record must be object")?;
    let executed = lines(obj.get("executed_lines"), work)?;
    let missing = lines(obj.get("missing_lines"), work)?;
    if !executed.is_disjoint(&missing) {
        bail!("coverage executed and missing lines overlap");
    }
    Ok((executed, missing))
}
fn lines(value: Option<&Value>, work: &mut usize) -> Result<BTreeSet<u32>> {
    let values = value
        .and_then(Value::as_array)
        .context("coverage line list missing or invalid")?;
    *work = work
        .checked_add(values.len())
        .context("coverage work overflow")?;
    ensure!(*work <= MAX_LINES, "coverage line budget exceeded");
    let mut result = BTreeSet::new();
    for value in values {
        let line = value
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .filter(|n| *n > 0)
            .context("invalid coverage line")?;
        ensure!(result.insert(line), "duplicate coverage line");
    }
    Ok(result)
}
#[cfg(test)]
#[path = "candidate_python_coverage_tests.rs"]
mod tests;
