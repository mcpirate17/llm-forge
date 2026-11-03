//! Bounded Git facts for the full report. No deletion/ref mutation operations.
use anyhow::{ensure, Context, Result};
use git2::Repository;
use serde_json::{json, Value};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};
const MAX_ROWS: usize = 100_000;
pub(super) use crate::workspace_integration::git;
pub(super) fn worktrees(root: &Path) -> Result<Vec<Value>> {
    // NUL fields preserve spaces/newlines rather than Python's lossy line parser.
    let raw = git(root, &["worktree", "list", "--porcelain", "-z"])?;
    let mut rows = Vec::new();
    let mut row = serde_json::Map::new();
    for field in raw.split('\0') {
        if field.is_empty() {
            if !row.is_empty() {
                rows.push(Value::Object(std::mem::take(&mut row)));
            }
        } else {
            let (key, value) = field.split_once(' ').unwrap_or((field, ""));
            row.insert(key.to_owned(), json!(value));
        }
        ensure!(
            rows.len() <= MAX_ROWS,
            "worktree inventory exceeds100000 entries"
        );
    }
    if !row.is_empty() {
        rows.push(Value::Object(row));
    }
    Ok(rows)
}
fn text<'a>(row: &'a Value, key: &str) -> &'a str {
    row[key].as_str().unwrap_or("")
}
fn changes(path: &str) -> Result<Vec<String>> {
    // This human sample deliberately preserves Git's configured porcelain quoting.
    // Paths for authority decisions elsewhere use NUL/native Git framing.
    let raw = git(
        Path::new(path),
        &["status", "--porcelain", "--untracked-files=all"],
    )?;
    Ok(raw
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(str::to_owned)
        .collect())
}
pub(super) fn worktree_state(trees: &[Value]) -> Result<(Vec<String>, Vec<Value>)> {
    let mut stale = Vec::new();
    let mut dirty = Vec::new();
    for tree in trees {
        let path = text(tree, "worktree");
        if path.is_empty() {
            continue;
        }
        if !Path::new(path).is_dir() {
            stale.push(path.to_owned());
            continue;
        }
        let changes = changes(path)?;
        if !changes.is_empty() {
            dirty.push(json!({"worktree":path,
            "branch":tree.get("branch").cloned().unwrap_or(json!("(detached)")),
            "entries":changes.len(),"sample":changes.into_iter().take(5).collect::<Vec<_>>()}));
        }
    }
    Ok((stale, dirty))
}
pub(super) fn default_branch(root: &Path) -> Result<String> {
    let repo = Repository::discover(root)?;
    if let Ok(reference) = repo.find_reference("refs/remotes/origin/HEAD") {
        if let Some(target) = reference.symbolic_target()? {
            return Ok(target
                .rsplit('/')
                .next()
                .context("empty origin HEAD")?
                .to_owned());
        }
    }
    conductor_native::project_paths::integration_branch(root).map_err(anyhow::Error::msg)
}
pub(super) fn redundant(
    root: &Path,
    live: &str,
    protected: &str,
    dirty: &[Value],
) -> Result<Vec<Value>> {
    let repo = Repository::discover(root)?;
    let blocked: BTreeSet<_> = dirty
        .iter()
        .filter_map(|row| row["branch"].as_str())
        .map(|branch| branch.strip_prefix("refs/heads/").unwrap_or(branch))
        .collect();
    let base = repo.revparse_single(live)?.peel_to_commit()?.id();
    let mut rows = Vec::new();
    for branch in repo.branches(Some(git2::BranchType::Local))? {
        let (branch, _) = branch?;
        let name = branch.name()?.context("branch name is not UTF-8")?;
        if name == live || name == protected {
            continue;
        }
        let oid = branch.get().peel_to_commit()?.id();
        if oid != base && !repo.graph_descendant_of(base, oid)? {
            continue;
        }
        let mut row = json!({"branch":name,"sha":oid.to_string()});
        if blocked.contains(name) {
            row["blocked_by"] = json!("worktree holds uncommitted work");
        }
        rows.push(row);
        ensure!(
            rows.len() <= MAX_ROWS,
            "branch report exceeds100000 entries"
        );
    }
    rows.sort_by(|a, b| a["branch"].as_str().cmp(&b["branch"].as_str()));
    Ok(rows)
}
fn upstream_gone(root: &Path, branch: &str) -> Result<bool> {
    if branch.is_empty() {
        return Ok(false);
    }
    let repo = Repository::discover(root)?;
    let config = repo.config()?;
    let remote = match config.get_string(&format!("branch.{branch}.remote")) {
        Ok(value) => value,
        Err(error) if error.code() == git2::ErrorCode::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    let merge = match config.get_string(&format!("branch.{branch}.merge")) {
        Ok(value) => value,
        Err(error) if error.code() == git2::ErrorCode::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    if remote.is_empty() || merge.is_empty() {
        return Ok(false);
    }
    let reference = format!(
        "refs/remotes/{remote}/{}",
        merge.strip_prefix("refs/heads/").unwrap_or(&merge)
    );
    let result = match repo.find_reference(&reference) {
        Ok(_) => Ok(false),
        Err(error) if error.code() == git2::ErrorCode::NotFound => Ok(true),
        Err(error) => Err(error.into()),
    };
    result
}
pub(super) fn landed(root: &Path, live: &str, trees: &[Value]) -> Result<Vec<Value>> {
    let repo = Repository::discover(root)?;
    let base = repo.revparse_single(live)?.peel_to_commit()?.id();
    let root = root.canonicalize()?;
    let mut rows = Vec::new();
    for tree in trees {
        let path = text(tree, "worktree");
        let full = PathBuf::from(path);
        if path.is_empty()
            || !full.is_dir()
            || !full.join(".git").is_file()
            || full.canonicalize()? == root
            || !changes(path)?.is_empty()
        {
            continue;
        }
        let head = text(tree, "HEAD");
        if head.is_empty() {
            continue;
        }
        let oid = git2::Oid::from_str(head)?;
        let branch = text(tree, "branch")
            .strip_prefix("refs/heads/")
            .unwrap_or(text(tree, "branch"));
        let reason = if oid == base || repo.graph_descendant_of(base, oid)? {
            format!("contained in {live}")
        } else if upstream_gone(&root, branch)? {
            "remote branch is gone; merge is unverified".to_owned()
        } else {
            continue;
        };
        let requires_review = reason == "remote branch is gone; merge is unverified";
        rows.push(
            json!({"worktree":path,"branch":if branch.is_empty(){"(detached)"}else{branch},
            "head":head.chars().take(8).collect::<String>(),"reason":reason,"requires_review":requires_review}),
        );
    }
    Ok(rows)
}
pub(super) fn unpushed(root: &Path) -> Result<Vec<Value>> {
    let repo = Repository::discover(root)?;
    let mut rows = Vec::new();
    for branch in repo.branches(Some(git2::BranchType::Local))? {
        let (branch, _) = branch?;
        let name = branch.name()?.context("branch name is not UTF-8")?;
        let upstream = match branch.upstream() {
            Ok(value) => Some(value.get().shorthand()?.to_owned()),
            Err(error) if error.code() == git2::ErrorCode::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        let remote = upstream.unwrap_or_else(|| format!("origin/{name}"));
        let oid = branch.get().peel_to_commit()?.id();
        let (count, label) = match repo.revparse_single(&remote) {
            Ok(base) => (
                repo.graph_ahead_behind(oid, base.peel_to_commit()?.id())?.0,
                remote,
            ),
            Err(error)
                if error.code() == git2::ErrorCode::NotFound
                    || error.code() == git2::ErrorCode::InvalidSpec =>
            {
                let mut walk = repo.revwalk()?;
                walk.push(oid)?;
                let mut count = 0usize;
                for value in walk {
                    value?;
                    count += 1;
                    ensure!(
                        count <= MAX_ROWS,
                        "untracked branch history exceeds100000 commits"
                    );
                }
                (count, "(NO REMOTE -- whole branch is unpushed)".to_owned())
            }
            Err(error) => return Err(error.into()),
        };
        if count > 0 {
            rows.push(json!({"branch":name,"ahead":count.to_string(),"remote":label}));
        }
        ensure!(
            rows.len() <= MAX_ROWS,
            "unpushed inventory exceeds100000 branches"
        );
    }
    rows.sort_by(|a, b| {
        b["ahead"]
            .as_str()
            .unwrap()
            .parse::<usize>()
            .unwrap()
            .cmp(&a["ahead"].as_str().unwrap().parse::<usize>().unwrap())
    });
    Ok(rows)
}
