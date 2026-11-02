//! Exact changed-file territory calculation over already captured campaign scope.
use anyhow::{ensure, Result};
use serde::Serialize;
use std::{
    collections::BTreeSet,
    path::{Component, Path},
};

#[derive(Debug)]
pub struct CampaignScope {
    pub language: String,
    pub source_paths: Vec<String>,
    pub test_paths: Vec<String>,
}
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct Uncovered {
    pub file: String,
    pub reason: &'static str,
    pub detail: &'static str,
}
fn validate(path: &str) -> Result<()> {
    ensure!(
        !path.is_empty()
            && path.len() <= 4096
            && path
                .split('/')
                .all(|part| !part.is_empty() && part != "." && part != "..")
            && Path::new(path)
                .components()
                .all(|part| matches!(part, Component::Normal(_))),
        "invalid corpus path"
    );
    Ok(())
}
fn parent(path: &str) -> &str {
    path.rsplit_once('/').map_or(".", |(parent, _)| parent)
}
fn suffix(path: &str) -> &str {
    let name = path.rsplit('/').next().unwrap_or(path);
    match name.rfind('.') {
        Some(index) if index > 0 => &name[index..],
        _ => "",
    }
}
fn suffixes(language: &str) -> Option<&'static [&'static str]> {
    match language {
        "python" => Some(&[".py"]),
        "rust" => Some(&[".rs"]),
        "python-rust" => Some(&[".py", ".rs"]),
        "c" => Some(&[".c", ".h"]),
        "cpp" => Some(&[".cc", ".cpp", ".cxx", ".h", ".hh", ".hpp"]),
        _ => None,
    }
}
fn inside(path: &str, directory: &str, suffixes: Option<&[&str]>) -> bool {
    let parent = parent(path);
    match suffixes {
        None => parent == directory,
        Some(extensions) => {
            extensions.contains(&suffix(path))
                && (parent == directory
                    || parent
                        .strip_prefix(directory)
                        .is_some_and(|rest| rest.starts_with('/')))
        }
    }
}
/// Determine changed files inside campaign territory with no exact source/test pin.
/// `present` must be captured from the candidate tree; this does not perform an audit.
pub fn uncovered(
    changed: &[String],
    present: &BTreeSet<String>,
    campaigns: &[CampaignScope],
) -> Result<Vec<Uncovered>> {
    ensure!(
        changed.len() <= 100_000 && present.len() <= 100_000 && campaigns.len() <= 100_000,
        "corpus territory input count exceeds limit"
    );
    let mut bytes = 0usize;
    for path in changed.iter().chain(present) {
        validate(path)?;
        bytes = bytes
            .checked_add(path.len())
            .ok_or_else(|| anyhow::anyhow!("corpus path budget overflow"))?;
        ensure!(
            bytes <= 64 * 1024 * 1024,
            "corpus path byte budget exceeded"
        );
    }
    let mut scope_count = 0usize;
    let mut pinned = BTreeSet::new();
    let mut territories = BTreeSet::new();
    for campaign in campaigns {
        ensure!(
            campaign.language.len() <= 128,
            "corpus language exceeds limit"
        );
        for path in campaign.source_paths.iter().chain(&campaign.test_paths) {
            validate(path)?;
            scope_count = scope_count
                .checked_add(1)
                .ok_or_else(|| anyhow::anyhow!("corpus scope count overflow"))?;
            bytes = bytes
                .checked_add(path.len())
                .ok_or_else(|| anyhow::anyhow!("corpus path budget overflow"))?;
            ensure!(
                scope_count <= 1_000_000 && bytes <= 64 * 1024 * 1024,
                "corpus scope budget exceeded"
            );
            pinned.insert(path.clone());
            territories.insert((parent(path).to_owned(), campaign.language.clone()));
            ensure!(
                pinned.len() <= 1_000_000 && territories.len() <= 1_000_000,
                "corpus scope exceeds limit"
            );
        }
    }
    let changed: BTreeSet<_> = changed.iter().collect();
    let mut result = Vec::new();
    let mut work = 0usize;
    for path in changed {
        validate(path)?;
        if pinned.contains(path) || !present.contains(path) {
            continue;
        }
        for (directory, language) in &territories {
            work += 1;
            ensure!(work <= 10_000_000, "corpus territory work limit exceeded");
            if inside(path, directory, suffixes(language)) {
                result.push(Uncovered { file: path.clone(), reason: "NO_PINNING_CAMPAIGN",
                    detail: "changed file sits beside or inside files registered campaigns pin, but no campaign pins it; plan one" });
                break;
            }
        }
    }
    Ok(result)
}
#[cfg(test)]
#[path = "mutation_corpus_territory_tests.rs"]
mod tests;
