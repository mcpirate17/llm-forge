//! Bounded LLVM lcov line evaluation against captured candidate sources.
//! The caller must supply bytes owner-captured from the successful instrumented
//! Cargo/LLVM Stage; this parser alone does not authenticate the producer.
use crate::candidate_python_coverage::{self, ChangedFile, Coverage};
use anyhow::{ensure, Context, Result};
use serde_json::{json, Map};
use std::collections::{BTreeMap, BTreeSet};
const MAX_BYTES: usize = 16 * 1024 * 1024;
const MAX_LINES: usize = 1_000_000;
const MAX_SOURCES: usize = 4096;

pub(crate) fn evaluate(
    bytes: &[u8],
    snapshot: &str,
    changed: &[ChangedFile],
    high: f64,
    other: f64,
) -> Result<Coverage> {
    ensure!(
        bytes.len() <= MAX_BYTES && !bytes.contains(&0),
        "LLVM coverage text exceeds bounds or contains NUL"
    );
    ensure!(
        snapshot.starts_with('/') && !snapshot.ends_with('/') && snapshot.len() <= 4096,
        "invalid captured snapshot path"
    );
    ensure!(changed.len() <= MAX_SOURCES, "too many changed Rust files");
    let targets: BTreeSet<_> = changed.iter().map(|f| f.path.as_str()).collect();
    ensure!(
        targets.len() == changed.len(),
        "duplicate changed Rust source"
    );
    let mut rows: BTreeMap<String, BTreeMap<u32, bool>> = BTreeMap::new();
    let mut active: Option<Option<String>> = None;
    let text = std::str::from_utf8(bytes).context("LLVM coverage is not UTF-8")?;
    let mut count = 0usize;
    for line in text.lines() {
        count = count
            .checked_add(1)
            .context("LLVM coverage line overflow")?;
        ensure!(count <= MAX_LINES, "LLVM coverage line count exceeds limit");
        if let Some(path) = line.strip_prefix("SF:") {
            ensure!(
                active.is_none(),
                "LLVM coverage source started before previous ended"
            );
            let relative = relative(path, snapshot)?;
            active = Some(relative.filter(|v| targets.contains(v.as_str())));
            if let Some(Some(path)) = &active {
                rows.entry(path.clone()).or_default();
            }
        } else if line == "end_of_record" {
            ensure!(
                active.take().is_some(),
                "LLVM coverage has unpaired end marker"
            );
        } else if let Some(data) = line.strip_prefix("DA:") {
            let selected = active
                .as_ref()
                .context("LLVM coverage line outside source record")?;
            let (line, hits) = parse_da(data)?;
            if let Some(path) = selected {
                *rows
                    .get_mut(path)
                    .context("LLVM source state missing")?
                    .entry(line)
                    .or_default() |= hits;
            }
        } else {
            validate_aux(line, active.is_some())?;
        }
    }
    ensure!(active.is_none(), "LLVM coverage source record did not end");
    let mut files = Map::new();
    for (path, lines) in rows {
        let mut executed = Vec::new();
        let mut missing = Vec::new();
        for (line, hit) in lines {
            if hit {
                executed.push(line);
            } else {
                missing.push(line);
            }
        }
        files.insert(
            path,
            json!({"executed_lines":executed,"missing_lines":missing}),
        );
    }
    let payload = serde_json::to_vec(&json!({"files":files}))?;
    candidate_python_coverage::evaluate(&payload, changed, high, other)
}
fn relative(path: &str, snapshot: &str) -> Result<Option<String>> {
    ensure!(
        !path.is_empty() && path.len() <= 4096 && !path.chars().any(char::is_control),
        "invalid LLVM source path"
    );
    let candidate = if path.starts_with('/') {
        let Some(v) = path
            .strip_prefix(snapshot)
            .and_then(|v| v.strip_prefix('/'))
        else {
            return Ok(None);
        };
        v
    } else {
        path
    };
    crate::candidate_source::safe_path(candidate.as_bytes())?;
    Ok(Some(candidate.into()))
}
fn parse_da(value: &str) -> Result<(u32, bool)> {
    let mut parts = value.split(',');
    let line = parts
        .next()
        .context("LLVM coverage line missing")?
        .parse::<u32>()?;
    let hits = parts
        .next()
        .context("LLVM coverage hit count missing")?
        .parse::<u64>()?;
    ensure!(line > 0, "LLVM coverage line must be positive");
    if let Some(checksum) = parts.next() {
        ensure!(
            !checksum.is_empty()
                && checksum.len() <= 128
                && checksum
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"+/=".contains(&b)),
            "invalid LLVM line checksum"
        );
    }
    ensure!(parts.next().is_none(), "invalid LLVM line record");
    Ok((line, hits > 0))
}
fn validate_aux(line: &str, inside: bool) -> Result<()> {
    if let Some(name) = line.strip_prefix("TN:") {
        ensure!(!inside && name.len() <= 4096, "invalid LLVM test name");
        return Ok(());
    }
    ensure!(
        inside
            && ["FN:", "FNDA:", "FNF:", "FNH:", "BRDA:", "BRF:", "BRH:", "LF:", "LH:"]
                .iter()
                .any(|tag| line.starts_with(tag)),
        "unsupported LLVM coverage record"
    );
    Ok(())
}
#[cfg(test)]
#[path = "candidate_rust_coverage_tests.rs"]
mod tests;
