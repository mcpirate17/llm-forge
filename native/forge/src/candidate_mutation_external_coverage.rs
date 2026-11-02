//! Confined coverage.json projection for one owned invariant phase.
use anyhow::{ensure, Context, Result};
use serde_json::Value;
use std::{collections::BTreeSet, path::Path};
type Lines = BTreeSet<(String, u32)>;
fn infrastructure(path: &str) -> bool {
    let p = Path::new(path);
    p.components()
        .any(|c| matches!(c,std::path::Component::Normal(v) if v=="tests"))
        || p.file_name()
            .and_then(|v| v.to_str())
            .is_some_and(|name| name == "conftest.py" || name.starts_with("test_"))
}
fn relative<'a>(path: &'a str, snapshot: &Path, source: &Path) -> Result<Option<(&'a str, bool)>> {
    ensure!(
        path.len() <= 4096 && !path.chars().any(char::is_control),
        "invalid invariant coverage path"
    );
    let value = Path::new(path);
    if value.is_absolute() {
        if let Ok(rest) = value.strip_prefix(snapshot) {
            return Ok(Some((
                rest.to_str().context("snapshot coverage path not UTF-8")?,
                false,
            )));
        }
        if let Ok(rest) = value.strip_prefix(source) {
            return Ok(Some((
                rest.to_str().context("source coverage path not UTF-8")?,
                true,
            )));
        }
        return Ok(None);
    }
    Ok(Some((path, false)))
}
pub(super) fn executed(
    payload: &Value,
    snapshot: &Path,
    source: &Path,
    tracked: &BTreeSet<String>,
) -> Result<Lines> {
    let files = payload
        .get("files")
        .and_then(Value::as_object)
        .context("invariant coverage files missing")?;
    ensure!(
        files.len() <= 4096,
        "invariant coverage file count exceeds bound"
    );
    let mut lines = Lines::new();
    let mut seen = BTreeSet::new();
    let mut total = 0usize;
    for (name, record) in files {
        let Some((relative, live_source)) = relative(name, snapshot, source)? else {
            continue;
        };
        crate::candidate_source::safe_path(relative.as_bytes())?;
        ensure!(
            seen.insert((relative.to_owned(), live_source)),
            "duplicate invariant coverage path"
        );
        let excluded = record
            .get("excluded_lines")
            .and_then(Value::as_array)
            .context("invariant excluded lines missing")?;
        ensure!(
            excluded.is_empty(),
            "invariant coverage contains excluded lines"
        );
        let executed = record
            .get("executed_lines")
            .and_then(Value::as_array)
            .context("invariant executed lines missing")?;
        total = total
            .checked_add(executed.len())
            .context("invariant line overflow")?;
        ensure!(total <= 200_000, "invariant line count exceeds limit");
        if infrastructure(relative) || executed.is_empty() {
            continue;
        }
        ensure!(
            !live_source,
            "invariant executed original live repository source"
        );
        ensure!(
            tracked.contains(relative),
            "invariant executed untracked production source"
        );
        for value in executed {
            let line = value
                .as_u64()
                .and_then(|v| u32::try_from(v).ok())
                .filter(|v| *v > 0)
                .context("invalid invariant line number")?;
            ensure!(
                lines.insert((relative.to_owned(), line)),
                "duplicate invariant executed line"
            );
        }
    }
    Ok(lines)
}
pub(super) fn natives(value: &Value) -> Result<Vec<String>> {
    let rows = value
        .as_array()
        .context("native invariant report missing")?;
    ensure!(rows.len() <= 4096, "native invariant report exceeds limit");
    let mut out = BTreeSet::new();
    let mut bytes = 0usize;
    for row in rows {
        let name = row.as_str().context("native invariant path invalid")?;
        ensure!(
            !name.is_empty() && name.len() <= 4096 && !name.chars().any(char::is_control),
            "native invariant path invalid"
        );
        bytes = bytes
            .checked_add(name.len())
            .context("native invariant path overflow")?;
        ensure!(bytes <= 1024 * 1024, "native invariant paths exceed limit");
        ensure!(
            out.insert(name.to_owned()),
            "duplicate native invariant report entry"
        );
    }
    Ok(out.into_iter().collect())
}
#[cfg(test)]
#[path = "candidate_mutation_external_coverage_tests.rs"]
mod tests;
