//! Check mutation targets against the invoking lane's changed files before any
//! engine or snapshot starts. A changed test admits only its named source, and
//! `--only` narrows the owned diff; neither can authorize another lane's work.

use crate::mutation_campaign::{py_list, py_repr, Campaign};
use crate::mutation_scope::changed_sources_narrow;
use conductor_native::candidate_policy::glob_match;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Component, Path, PathBuf};

const MULL_SCOPE_FILE: &str = ".mutation-mull-scope.yml";

/// Resolve a literal file inside the checkout, refusing globs and escapes.
fn exact_file(root: &Path, relative: &str) -> Result<String, String> {
    let path = Path::new(relative);
    let escapes = path.is_absolute() || path.components().any(|c| c == Component::ParentDir);
    if escapes || relative.contains(['*', '?', '[']) {
        return Err(format!(
            "mutation scope requires exact repository files: {}",
            py_repr(relative)
        ));
    }
    let target = root.join(path);
    let inside = match (target.canonicalize(), root.canonicalize()) {
        (Ok(resolved), Ok(checkout)) => resolved.starts_with(checkout),
        _ => false,
    };
    if !target.is_file() || !inside {
        return Err(format!(
            "mutation scope file is absent or outside the checkout: {}",
            py_repr(relative)
        ));
    }
    Ok(normalised(path))
}

/// `Path.as_posix`: redundant separators and `.` segments do not name a new file.
fn normalised(path: &Path) -> String {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// Expand archival Mull globs into pinned targets for its engine allowlist.
fn mull_sources(campaign: &Campaign, root: &Path) -> Result<Vec<String>, String> {
    let mut selected = BTreeSet::new();
    for pattern in &campaign.source {
        let matches: Vec<&String> = campaign
            .source_sha256
            .keys()
            .filter(|source| glob_match(source, pattern))
            .collect();
        if matches.is_empty() {
            return Err(format!(
                "Mull source has no pinned target: {}",
                py_repr(pattern)
            ));
        }
        for source in matches {
            selected.insert(exact_file(root, source)?);
        }
    }
    if selected.is_empty() {
        return Err("Mull requires at least one exact source file".into());
    }
    Ok(selected.into_iter().collect())
}

pub(crate) fn targets_of(
    campaign: &Campaign,
    repo_root: &Path,
) -> Result<BTreeSet<String>, String> {
    if campaign.mutation_engine == "mull" {
        return Ok(mull_sources(campaign, repo_root)?.into_iter().collect());
    }
    let prefix = if campaign.mutation_engine == "cargo-mutants" {
        campaign.option_text("package_root").unwrap_or_default()
    } else {
        String::new()
    };
    campaign
        .source
        .iter()
        .map(|source| {
            let joined = Path::new(&prefix).join(source);
            exact_file(repo_root, &joined.to_string_lossy())
        })
        .collect()
}

/// The sibling test paths `source` is named by.
fn paired_test_paths(source: &str) -> Vec<PathBuf> {
    let path = Path::new(source);
    let parent = path.parent().unwrap_or(Path::new(""));
    let name = format!(
        "test_{}",
        path.file_name().unwrap_or_default().to_string_lossy()
    );
    let mut directories = vec![parent.to_path_buf(), parent.join("tests")];
    if matches!(
        parent.file_name().and_then(|part| part.to_str()),
        Some("src" | "tools")
    ) {
        directories.push(parent.parent().unwrap_or(Path::new("")).join("tests"));
    }
    directories
        .into_iter()
        .map(|directory| directory.join(&name))
        .collect()
}

/// Sources a changed test admits: exactly its one named partner, or none.
fn admitted_by_test<'a>(test: &str, targets: &'a BTreeSet<String>) -> Option<&'a String> {
    let mut paired = targets.iter().filter(|source| {
        source.ends_with(".py") && paired_test_paths(source).contains(&PathBuf::from(test))
    });
    let first = paired.next()?;
    paired.next().is_none().then_some(first)
}

/// Refuse broad or unchanged targets; returns the sorted targets on success.
pub fn validate_run_scope(
    campaign: &Campaign,
    repo_root: &Path,
    owner: &str,
    base: &str,
    only: &[String],
) -> Result<Vec<String>, String> {
    let targets = targets_of(campaign, repo_root)?;
    let canonical = |path: &str| -> Result<PathBuf, String> {
        let path = exact_file(repo_root, path)?;
        repo_root
            .join(path)
            .canonicalize()
            .map_err(|error| error.to_string())
    };
    let selected = targets
        .iter()
        .map(|path| canonical(path))
        .collect::<Result<BTreeSet<_>, _>>()?;
    let pins = campaign
        .source_sha256
        .keys()
        .map(|path| canonical(path))
        .collect::<Result<BTreeSet<_>, _>>()?;
    if targets.is_empty() || !selected.is_subset(&pins) {
        return Err("every mutation target must be pinned in source_sha256".into());
    }
    if pins != selected {
        return Err("extra source_sha256 supporting pins are unsupported: every source pin must be a selected mutation target".into());
    }
    let selected_only: BTreeSet<String> = only
        .iter()
        .map(|source| exact_file(repo_root, source))
        .collect::<Result<_, _>>()?;
    let narrowed = (!only.is_empty()).then_some(&selected_only);
    let mut changed = changed_sources_narrow(base, repo_root, owner, narrowed)?;
    if !only.is_empty() {
        let foreign: Vec<&String> = selected_only.difference(&changed).collect();
        if !foreign.is_empty() {
            return Err(format!(
                "--only names files outside this agent's changes: {}",
                py_list(foreign)
            ));
        }
        changed = changed.intersection(&selected_only).cloned().collect();
    }
    let mut allowed = changed.clone();
    for test in campaign.test_sha256.keys().filter(|t| changed.contains(*t)) {
        if let Some(source) = admitted_by_test(test, &targets) {
            allowed.insert(source.clone());
        }
    }
    let outside: Vec<&String> = targets.difference(&allowed).collect();
    if !outside.is_empty() {
        return Err(format!(
            "campaign mutates files outside this agent's changes: {}; generate a \
             campaign for the exact changed files with the correct --owner",
            py_list(outside)
        ));
    }
    Ok(targets.into_iter().collect())
}

/// `json.dumps(text)`: the ASCII-escaped scalar `python_pretty` emits.
fn json_text(text: &str) -> Result<String, String> {
    crate::json_canon::python_pretty(serde_json::Value::from(text))
        .map(|line| line.trim_end().to_string())
        .map_err(|error| error.to_string())
}

/// Give Mull's compiler plugin and runner the same exact path allowlist.
pub fn mull_scope_config(campaign: &Campaign, worktree: &Path) -> Result<PathBuf, String> {
    let mut lines = vec!["includePaths:".to_string()];
    for source in mull_sources(campaign, worktree)? {
        let resolved = worktree
            .join(&source)
            .canonicalize()
            .map_err(|error| format!("cannot resolve {source}: {error}"))?;
        let pattern = format!("^{}$", python_escape(&resolved.to_string_lossy()));
        lines.push(format!("  - {}", json_text(&pattern)?));
    }
    if !campaign.operators.is_empty() {
        lines.push("mutators:".into());
        for operator in &campaign.operators {
            lines.push(format!("  - {}", json_text(operator)?));
        }
    }
    let config = worktree.join(MULL_SCOPE_FILE);
    fs::write(&config, lines.join("\n"))
        .map_err(|error| format!("cannot write {}: {error}", config.display()))?;
    Ok(config)
}

/// `re.escape`: every character Python's regex syntax gives meaning to.
fn python_escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for ch in text.chars() {
        if "()[]{}?*+-|^$\\.&~# \t\n\r\x0b\x0c".contains(ch) {
            escaped.push('\\');
        }
        escaped.push(ch);
    }
    escaped
}

#[cfg(test)]
#[path = "mutation_run_scope_tests.rs"]
mod tests;
