//! Read-only parity for worktree_reap.decide's exposure preview. No apply APIs.
use super::{gh, git, probe, prs};
use anyhow::{Context, Result};
use git2::Repository;
use serde_json::{json, Value};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
#[derive(Default)]
struct Tree {
    path: PathBuf,
    head: String,
    branch: String,
    locked: bool,
    prunable: bool,
}
fn inventory(root: &Path) -> Result<Vec<Tree>> {
    let raw = git(root, &["worktree", "list", "--porcelain", "-z"])?;
    let mut rows = Vec::new();
    let mut row = Tree::default();
    for token in raw.split('\0') {
        if token.is_empty() {
            if !row.path.as_os_str().is_empty() {
                rows.push(row);
                row = Tree::default();
            }
            continue;
        }
        let (key, value) = token.split_once(' ').unwrap_or((token, ""));
        match key {
            "worktree" => row.path = PathBuf::from(value),
            "HEAD" => row.head = value.into(),
            "branch" => row.branch = value.trim_start_matches("refs/heads/").into(),
            "locked" => row.locked = true,
            "prunable" => row.prunable = true,
            _ => (),
        }
        anyhow::ensure!(
            rows.len() < 10000,
            "worktree inventory exceeds10000 entries"
        );
    }
    if !row.path.as_os_str().is_empty() {
        rows.push(row);
    }
    anyhow::ensure!(!rows.is_empty(), "git reported no worktrees");
    Ok(rows)
}
fn integration(root: &Path) -> Result<String> {
    let repository = Repository::discover(root)?;
    let branch =
        conductor_native::project_paths::integration_branch(root).map_err(anyhow::Error::msg)?;
    let candidates = [format!("origin/{branch}"), branch];
    for name in &candidates {
        if repository.revparse_single(name).is_ok() {
            return Ok(name.clone());
        }
    }
    if let Ok(reference) = repository.find_reference("refs/remotes/origin/HEAD") {
        if let Some(target) = reference.symbolic_target()? {
            return Ok(target.trim_start_matches("refs/remotes/").into());
        }
    }
    let known = ["origin/master", "origin/main"]
        .into_iter()
        .filter(|name| {
            repository
                .find_reference(&format!("refs/remotes/{name}"))
                .is_ok()
        })
        .collect::<Vec<_>>();
    if known.len() == 1 {
        return Ok(known[0].into());
    }
    if let Ok(listed) = git(root, &["ls-remote", "--symref", "origin", "HEAD"]) {
        for line in listed.lines() {
            let parts = line.split_whitespace().collect::<Vec<_>>();
            if parts.len() >= 2 && parts[0] == "ref:" && parts[1].starts_with("refs/heads/") {
                return Ok(format!(
                    "origin/{}",
                    parts[1].trim_start_matches("refs/heads/")
                ));
            }
        }
    }
    anyhow::bail!("{}: no integration line to judge containment against (tried {}; origin HEAD symref unavailable)",root.display(),candidates.join(", "))
}
fn process_cwds() -> (Vec<PathBuf>, usize, bool) {
    let mut paths = Vec::new();
    let mut unreadable = 0;
    let Ok(entries) = fs::read_dir("/proc") else {
        return (paths, 0, true);
    };
    for (count, entry) in entries.enumerate() {
        if count >= 100000 {
            return (paths, unreadable, true);
        }
        let Ok(entry) = entry else {
            return (paths, unreadable, true);
        };
        if !entry
            .file_name()
            .to_string_lossy()
            .bytes()
            .all(|byte| byte.is_ascii_digit())
        {
            continue;
        }
        match fs::read_link(entry.path().join("cwd")) {
            Ok(path) => paths.push(path),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => unreadable += 1,
            Err(_) => return (paths, unreadable, true),
        }
    }
    (paths, unreadable, false)
}
fn lease(path: &Path, now: f64) -> Result<Option<(bool, String)>> {
    let target = path.join(".worktree-lease.json");
    let metadata = match fs::symlink_metadata(&target) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    anyhow::ensure!(
        metadata.is_file() && metadata.len() <= 1024 * 1024,
        "lease must be a bounded regular file"
    );
    let mut bytes = Vec::new();
    fs::File::open(&target)?
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    anyhow::ensure!(bytes.len() <= 1024 * 1024, "lease exceeds1MiB");
    let record: Value = serde_json::from_slice(&bytes)?;
    anyhow::ensure!(
        record["schema"] == "worktree-lease.v1",
        "{} is not a worktree-lease.v1 lease",
        target.display()
    );
    for key in ["owner", "purpose", "expires_at"] {
        anyhow::ensure!(
            record[key].as_str().is_some_and(|value| !value.is_empty()),
            "lease has no {key}"
        );
    }
    let raw = record["expires_at"].as_str().unwrap();
    let (stamp, aware) = conductor_native::branch_policy::stamp_epoch_seconds(raw)
        .map_err(anyhow::Error::msg)
        .context("untrusted lease deadline")?;
    let active = stamp > now;
    let expires = if raw.ends_with('Z') {
        format!("{}+00:00", raw.trim_end_matches('Z'))
    } else if !aware {
        format!("{raw}+00:00")
    } else {
        raw.into()
    };
    let owner = record["owner"].as_str().unwrap();
    let reason = if active {
        format!("active lease held by {owner} until {expires}")
    } else {
        format!("lease from {owner} expired at {expires}")
    };
    Ok(Some((active, reason)))
}
fn idle(root: &Path, path: &Path, now: f64) -> Result<bool> {
    let path = path.to_str().context("worktree path must be UTF-8")?;
    let cutoff = format!("@{:.0}", now - 6.0 * 3600.0);
    let output = probe(
        root,
        Path::new("find"),
        &[
            path,
            "-xdev",
            "(",
            "-name",
            ".git",
            "-o",
            "-name",
            ".venv",
            "-o",
            "-name",
            "node_modules",
            "-o",
            "-name",
            "__pycache__",
            ")",
            "-prune",
            "-o",
            "-type",
            "f",
            "-newermt",
            &cutoff,
            "-print",
            "-quit",
        ],
        120,
    )?;
    Ok(output.status.success() && output.stdout.is_empty())
}
fn gone(root: &Path, repository: &Repository, branch: &str) -> bool {
    let pushed = repository
        .find_reference(&format!("refs/remotes/origin/{branch}"))
        .is_ok()
        || repository
            .config()
            .and_then(|config| config.get_string(&format!("branch.{branch}.merge")))
            .is_ok_and(|value| value == format!("refs/heads/{branch}"));
    pushed
        && git(root, &["ls-remote", "--heads", "origin", branch])
            .is_ok_and(|value| value.trim().is_empty())
}
fn decide(root: &Path) -> Result<Vec<Value>> {
    let line = integration(root)?;
    let repository = Repository::discover(root)?;
    let rows = inventory(root)?;
    let primary = rows[0].path.canonicalize()?;
    let current = std::env::current_dir()?.canonicalize()?;
    let (cwds, unreadable, unknown) = process_cwds();
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs_f64();
    let program = gh();
    let mut result = Vec::new();
    for row in rows {
        let path = row.path.canonicalize().unwrap_or_else(|_| row.path.clone());
        if path == primary
            || current.starts_with(&path)
            || row.locked
            || unknown
            || cwds.iter().any(|cwd| cwd.starts_with(&path))
        {
            continue;
        }
        if row.path.is_dir() && !row.path.join(".git").is_file() {
            continue;
        }
        let lease = match lease(&row.path, now) {
            Ok(value) => value,
            Err(_) => continue,
        };
        if lease.as_ref().is_some_and(|(active, _)| *active) {
            continue;
        }
        let mut reasons = Vec::new();
        if row.prunable || !row.path.is_dir() {
            reasons.push("stale registration: worktree directory is absent".into());
        } else {
            if !row.head.is_empty() {
                if let (Ok(head), Ok(base)) = (
                    repository.revparse_single(&row.head),
                    repository.revparse_single(&line),
                ) {
                    if head.id() == base.id()
                        || repository.graph_descendant_of(base.id(), head.id())?
                    {
                        reasons.push(format!("HEAD is contained in {line}"));
                    }
                }
            }
            if !row.branch.is_empty() {
                if let Some(program) = &program {
                    if prs(root, &row.branch, true, program)
                        .is_ok_and(|states| states.iter().any(|state| state["state"] == "MERGED"))
                    {
                        reasons.push(format!("a merged PR exists for {}", row.branch));
                    }
                }
                if gone(root, &repository, &row.branch) {
                    reasons.push(format!("{} was pushed and is gone from origin", row.branch));
                }
            }
            if let Some((false, reason)) = lease {
                reasons.push(reason);
            }
            if idle(root, &row.path, now).unwrap_or(false) {
                reasons.push("idle: nothing modified in 6.0h".into());
            }
            if !reasons.is_empty() && unreadable > 0 {
                reasons.push(format!(
                    "note: {unreadable} processes could not be inspected"
                ));
            }
        }
        if !reasons.is_empty() {
            result.push(json!({"worktree":row.path,"branch":row.branch,"reason":reasons.into_iter().take(3).collect::<Vec<_>>().join("; ")}));
        }
    }
    Ok(result)
}
pub(super) fn preview(root: &Path) -> Result<Vec<Value>> {
    match decide(root) {
        Ok(rows) => Ok(rows),
        Err(error) => Ok(vec![
            json!({"worktree":root,"branch":"","reason":format!("reap unavailable: {error:#}")}),
        ]),
    }
}
