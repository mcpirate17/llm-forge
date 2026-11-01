//! Captured pyproject configuration for native hot-loop scan selection.
use anyhow::{bail, ensure, Context, Result};
use std::collections::BTreeSet;
use toml::Value;

const MAX_DOCUMENT: usize = 1024 * 1024;
const MAX_PATH: usize = 4096;
const MAX_PATHS: usize = 64;

fn child<'a>(value: &'a Value, name: &str) -> Result<Option<&'a Value>> {
    let table = value
        .as_table()
        .context("hot-path configuration ancestor must be a table")?;
    Ok(table.get(name))
}

fn normalize(value: &str) -> Result<String> {
    ensure!(
        !value.is_empty()
            && value.len() <= MAX_PATH
            && !value.starts_with('/')
            && !value.contains('\\')
            && !value.chars().any(char::is_control),
        "invalid hot-path directory"
    );
    let result = value.strip_suffix('/').unwrap_or(value);
    ensure!(
        !result.is_empty()
            && result
                .split('/')
                .all(|part| !part.is_empty() && part != "." && part != ".."),
        "hot-path directory contains ambiguous or unsafe components"
    );
    Ok(result.to_owned())
}

/// Read only the captured pyproject bytes. None and an absent table represent
/// an explicitly empty hot-path scope; malformed present settings are errors.
pub fn hot_paths(raw: Option<&[u8]>) -> Result<Vec<String>> {
    let Some(raw) = raw else {
        return Ok(Vec::new());
    };
    ensure!(raw.len() <= MAX_DOCUMENT, "pyproject exceeds 1 MiB");
    let text = std::str::from_utf8(raw).context("pyproject must be UTF-8")?;
    let parsed: Value = toml::from_str(text).context("invalid pyproject TOML")?;
    let Some(tool) = child(&parsed, "tool")? else {
        return Ok(Vec::new());
    };
    let Some(forge) = child(tool, "forge")? else {
        return Ok(Vec::new());
    };
    let Some(hot) = child(forge, "hot_paths")? else {
        return Ok(Vec::new());
    };
    let table = hot.as_table().context("hot_paths must be a table")?;
    ensure!(
        table.len() == 1 && table.contains_key("paths"),
        "hot_paths accepts exactly the paths key"
    );
    let paths = table["paths"]
        .as_array()
        .context("hot_paths.paths must be an array")?;
    ensure!(
        !paths.is_empty() && paths.len() <= MAX_PATHS,
        "hot_paths.paths requires 1..64 entries"
    );
    let mut seen = BTreeSet::new();
    let mut result = Vec::with_capacity(paths.len());
    for entry in paths {
        let Some(value) = entry.as_str() else {
            bail!("hot_paths.paths entries must be strings");
        };
        let path = normalize(value)?;
        ensure!(seen.insert(path.clone()), "duplicate hot-path directory");
        result.push(path);
    }
    Ok(result)
}

#[cfg(test)]
#[path = "candidate_scan_config_tests.rs"]
mod tests;
