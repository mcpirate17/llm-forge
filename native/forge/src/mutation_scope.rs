//! Which files one mutation campaign may cover: the branch's changed paths,
//! bounded by this lane's live ownership claims, or an exact `--only` list.
//!
//! Refusing an empty scope is the point. A campaign that plans nothing because
//! the branch changed nothing is a maintenance sweep asked for by accident.

use crate::ownership::{load_claims, paths_overlap};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn git_lines(repo_root: &Path, args: &[&str]) -> Result<Vec<String>, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(args)
        .output()
        .map_err(|error| format!("cannot start git: {error}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let detail = [stderr.trim(), stdout.trim()]
            .into_iter()
            .find(|text| !text.is_empty())
            .unwrap_or("no output");
        return Err(format!(
            "cannot determine mutation scope with git {}: {detail}",
            args.join(" ")
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(str::to_string)
        .collect())
}

fn now_seconds() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs_f64())
        .unwrap_or_default()
}

/// Dirty paths this lane's active claims own. A dirty checkout with no claim is
/// refused: the shared tree's dirt is not this lane's change to measure.
fn owned_dirty(
    repo_root: &Path,
    owner: &str,
    dirty: BTreeSet<String>,
) -> Result<BTreeSet<String>, String> {
    let (claims, _) = load_claims(repo_root)
        .map_err(|error| format!("cannot resolve ownership scope: {error}"))?;
    let now = now_seconds();
    let mut owned = Vec::new();
    for claim in claims.iter().filter(|claim| claim.owner == owner) {
        let active = claim
            .active(now)
            .map_err(|error| format!("cannot resolve ownership scope: {error}"))?;
        if active {
            owned.extend(claim.paths.iter().cloned());
        }
    }
    if owned.is_empty() {
        return Err(format!(
            "dirty mutation scope exists but '{owner}' has no active ownership claim. \
             Create a narrow claim or pass --only for the exact files you changed."
        ));
    }
    Ok(dirty
        .into_iter()
        .filter(|path| owned.iter().any(|claimed| paths_overlap(path, claimed)))
        .collect())
}

/// Repository-relative paths this branch touched, bounded by live lane claims.
/// Untracked files count: a brand-new module has never been mutated.
pub fn changed_sources(
    base: &str,
    repo_root: &Path,
    owner: &str,
) -> Result<BTreeSet<String>, String> {
    changed_sources_narrow(base, repo_root, owner, None)
}

/// Restrict dirty ownership checks to the exact requested paths, while still
/// requiring every selected dirty path to belong to the invoking lane.
pub(crate) fn changed_sources_narrow(
    base: &str,
    repo_root: &Path,
    owner: &str,
    only: Option<&BTreeSet<String>>,
) -> Result<BTreeSet<String>, String> {
    let diff_range = format!("{base}...HEAD");
    let mut paths: BTreeSet<String> = git_lines(repo_root, &["diff", "--name-only", &diff_range])?
        .into_iter()
        .collect();
    let mut dirty: BTreeSet<String> = git_lines(repo_root, &["diff", "--name-only", "HEAD"])?
        .into_iter()
        .collect();
    dirty.extend(git_lines(
        repo_root,
        &["ls-files", "--others", "--exclude-standard"],
    )?);
    if let Some(only) = only {
        dirty.retain(|path| only.contains(path));
    }
    if !dirty.is_empty() {
        let owned = owned_dirty(repo_root, owner, dirty.clone())?;
        if only.is_some() && owned != dirty {
            return Err("selected dirty mutation paths are not owned by this lane".into());
        }
        paths.extend(owned);
    }
    paths.retain(|path| repo_root.join(path).exists());
    Ok(paths)
}

/// The default subject set: what this branch changed, and nothing else.
pub fn branch_scope(base: &str, repo_root: &Path, owner: &str) -> Result<Vec<String>, String> {
    let scope = changed_sources(base, repo_root, owner)?;
    if scope.is_empty() {
        return Err(format!(
            "nothing changed against {base}: mutation campaigns cover the files this \
             branch modified and the tests that exercise them. Pass --all-files only \
             for an explicitly requested whole-tree maintenance sweep."
        ));
    }
    Ok(scope.into_iter().collect())
}

/// Validate an exact subject list without inspecting shared checkout dirt.
pub fn explicit_scope(paths: &[String], repo_root: &Path) -> Result<Vec<String>, String> {
    let mut scope = BTreeSet::new();
    for raw in paths {
        let normalized = raw.replace('\\', "/");
        let escapes = normalized.starts_with('/')
            || normalized
                .split('/')
                .any(|part| part == "." || part == "..");
        if escapes {
            return Err(format!(
                "--only must name a repository-relative file: {raw:?}"
            ));
        }
        if !repo_root.join(&normalized).is_file() {
            return Err(format!("--only source does not exist: {normalized}"));
        }
        scope.insert(normalized);
    }
    if scope.is_empty() {
        return Err("--only needs at least one exact repository-relative file".into());
    }
    Ok(scope.into_iter().collect())
}

fn is_test_path(relative: &str) -> bool {
    let name = relative.rsplit('/').next().unwrap_or(relative);
    name.starts_with("test_") || name == "conftest.py" || relative.contains("/tests/")
}

/// Parse `SOURCE=TEST` into exact repository-relative pairs.
pub fn extra_tests(
    pairs: &[String],
    repo_root: &Path,
) -> Result<BTreeMap<String, Vec<String>>, String> {
    let mut extra: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for raw in pairs {
        let Some((source, test)) = raw.split_once('=') else {
            return Err(format!("--extra-test wants SOURCE=TEST, got {raw:?}"));
        };
        let source = single_path(source, repo_root)?;
        let test = single_path(test, repo_root)?;
        if !is_test_path(&test) {
            return Err(format!("--extra-test target is not a test file: {test}"));
        }
        extra.entry(source).or_default().push(test);
    }
    Ok(extra)
}

fn single_path(raw: &str, repo_root: &Path) -> Result<String, String> {
    let mut scope = explicit_scope(&[raw.to_string()], repo_root)?;
    Ok(scope.remove(0))
}

#[cfg(test)]
#[path = "mutation_scope_tests.rs"]
mod tests;
