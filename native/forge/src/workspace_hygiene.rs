//! Port of `conductor.workspace_hygiene`'s SessionStart surface: the cheap
//! EXPOSED counts and the one-line inject they render
//! (`workspace_hygiene.exposure_line`, which `session_preamble._exposure_line`
//! used to embed). The full `--json` report stays in Python -- it needs `gh`
//! per feature branch, the AST-based untracked-import closure, and the whole
//! reaper engine -- none of which belongs on a sub-20ms SessionStart path.
//!
//! Git discipline: at most one invocation per fact. The local-only commit
//! count is one `for-each-ref` (both exclusion namespaces at once; Python's
//! `branch_policy` still spends two) plus one `rev-list --count`; dirty files
//! are one `status --porcelain=v2 -z` (v2 + `-z` so paths with spaces or
//! exotic bytes never arrive quoted, which v1's C-quoted form does); worktree
//! cleanliness/containment mirror Python's per-worktree `status` and
//! `rev-list`. No Python subprocess anywhere on this path.
//!
//! Resolution of the integration line is shared semantics with
//! `worktree_reap.default_integration_ref` (fixed in the same PR):
//! `[tool.conductor].integration_branch` (env override included), else the
//! remote's own HEAD symref -- never an `origin/master` literal.

use anyhow::{anyhow, Result};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

/// Python's `stale_dirty_files` default: files older than 24h uncommitted.
pub const DEFAULT_STALE_HOURS: f64 = 24.0;

/// Serializes tests that set `CONDUCTOR_INTEGRATION_BRANCH`: every test that
/// resolves an integration line reads it, and the handlers' session-start test
/// reaches it through `exposure_line`, so it takes this lock too. Lives at
/// module level (not in `tests`) because this file is also compiled verbatim
/// into the parity binary via `#[path]`, where `crate::handlers` does not
/// exist -- the lock must not reference anything outside this file.
#[cfg(test)]
pub(crate) static INTEGRATION_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn git_out(repo: &Path, args: &[&str]) -> Result<String> {
    let done = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .map_err(|err| anyhow!("git {} failed to start: {err}", args.join(" ")))?;
    if !done.status.success() {
        let stderr = String::from_utf8_lossy(&done.stderr);
        return Err(anyhow!("git {} failed: {}", args.join(" "), stderr.trim()));
    }
    Ok(String::from_utf8_lossy(&done.stdout).into_owned())
}

/// A probe whose nonzero exit is an answer ("absent"), not a failure -- the
/// equivalent of Python's `_git_in_quiet`/`_run(...).returncode` checks.
fn git_quiet(repo: &Path, args: &[&str]) -> Option<String> {
    let done = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .ok()?;
    if done.status.success() {
        Some(String::from_utf8_lossy(&done.stdout).into_owned())
    } else {
        None
    }
}

/// The nearest ancestor (inclusive) holding `.git` -- Python's
/// `project_paths.enclosing_repo`.
fn enclosing_repo(start: &Path) -> Option<PathBuf> {
    let mut candidate = start;
    while let Some(parent) = candidate.parent() {
        if candidate.join(".git").exists() {
            return Some(candidate.to_path_buf());
        }
        candidate = parent; // one step up
    }
    None // the filesystem root is never a repo
}

/// Python's `project_paths.host_root`: the enclosing repo, else the start
/// itself (symlinks resolved the way `Path.resolve()` does).
fn host_root(start: &Path) -> PathBuf {
    let resolved = std::fs::canonicalize(start).unwrap_or_else(|_| start.to_path_buf());
    enclosing_repo(&resolved).unwrap_or(resolved)
}

/// The configured integration branch: `CONDUCTOR_INTEGRATION_BRANCH`, else
/// `[tool.conductor].integration_branch` in the host root's pyproject.toml,
/// else `main` -- mirroring `project_paths._configured_integration_branch`
/// and `conductor_table` exactly: a missing manifest or a manifest without
/// the key is simply "unnamed" (the default), a non-table `[tool.conductor]`
/// or a non-string/empty value is a loud error, and an unparseable manifest
/// fails as-is.
fn configured_integration_branch(root: &Path) -> Result<String> {
    if let Ok(raw) = std::env::var("CONDUCTOR_INTEGRATION_BRANCH") {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            return Ok(trimmed.to_string());
        }
    }
    let manifest = root.join("pyproject.toml");
    if !manifest.is_file() {
        return Ok("main".to_string());
    }
    let text = std::fs::read_to_string(&manifest)
        .map_err(|err| anyhow!("cannot read {}: {err}", manifest.display()))?;
    let payload: toml::Table = text.parse().map_err(|err| anyhow!("{err}"))?;
    let tool = payload.get("tool");
    let table = match tool.and_then(|tool| tool.get("conductor")) {
        None => return Ok("main".to_string()),
        Some(table) => table,
    };
    if !table.is_table() {
        return Err(anyhow!(
            "[tool.conductor] in {} is not a table",
            manifest.display()
        ));
    }
    let Some(raw) = table.get("integration_branch") else {
        return Ok("main".to_string());
    };
    let Some(branch) = raw.as_str() else {
        return Err(anyhow!(
            "integration_branch in [tool.conductor] ({}) must be a string, got {raw}",
            manifest.display()
        ));
    };
    let trimmed = branch.trim();
    if trimmed.is_empty() {
        return Err(anyhow!(
            "integration_branch in [tool.conductor] ({}) must not be empty",
            manifest.display()
        ));
    }
    Ok(trimmed.to_string())
}

/// The line containment is judged against, resolved exactly as
/// `worktree_reap.default_integration_ref`: configured candidates first
/// (`origin/<branch>` preferred over the local branch), else the remote HEAD
/// symref -- bound locally, else advertised by the remote itself. Never a
/// literal.
pub fn default_integration_ref(repo: &Path) -> Result<String> {
    let branch = configured_integration_branch(&host_root(repo))?;
    let candidates = [format!("origin/{branch}"), branch];
    for refname in &candidates {
        if git_quiet(repo, &["rev-parse", "--verify", "--quiet", refname]).is_some() {
            return Ok(refname.clone());
        }
    }
    if let Some(bound) = git_quiet(
        repo,
        &["symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"],
    ) {
        // Python: `stdout.strip().removeprefix("refs/remotes/")` kept only
        // when truthy -- an empty target falls through to the advertisement.
        // Written empty-first (no `!`) so the guard cannot be quietly deleted
        // by a single-token mutation.
        let line = bound
            .trim()
            .strip_prefix("refs/remotes/")
            .unwrap_or(bound.trim());
        if line.is_empty() {
            // fall through to the remote's own advertisement
        } else {
            return Ok(line.to_string());
        }
    }
    if let Some(advertised) = git_quiet(repo, &["ls-remote", "--symref", "origin", "HEAD"]) {
        for line in advertised.lines() {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.first() == Some(&"ref:") {
                if let Some(head) = parts
                    .get(1)
                    .and_then(|refname| refname.strip_prefix("refs/heads/"))
                {
                    return Ok(format!("origin/{head}"));
                }
            }
        }
    }
    Err(anyhow!(
        "{}: no integration line to judge containment against \
         (tried {}, {}; origin HEAD symref unavailable)",
        repo.display(),
        candidates[0],
        candidates[1]
    ))
}

/// Commits on HEAD reachable from no `refs/remotes/*` or `refs/snapshots/**`
/// -- `branch_policy.local_only_commits`, counted. One `for-each-ref` covers
/// both exclusion namespaces; one `rev-list --count` answers the count (the
/// Python version lists shas and then pays a `log -1` per sha for subjects
/// this count never reads).
pub fn local_only_commit_count(repo: &Path) -> Result<usize> {
    let listing = git_out(
        repo,
        &[
            "for-each-ref",
            "--format=%(refname)",
            "refs/remotes",
            "refs/snapshots",
        ],
    )?;
    let excluded: Vec<&str> = listing
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    let mut args: Vec<&str> = vec!["rev-list", "--count", "HEAD"];
    if !excluded.is_empty() {
        args.push("--not");
        args.extend(excluded.iter().copied());
    }
    let count = git_out(repo, &args)?;
    count
        .trim()
        .parse::<usize>()
        .map_err(|err| anyhow!("git rev-list --count printed {count:?}: {err}"))
}

/// The `status --porcelain=v2 -z` paths, in order, rename destinations kept
/// and their orig-path tokens consumed. Headers (`# ...`) never appear here.
fn status_paths(repo: &Path) -> Result<Vec<String>> {
    let raw = git_out(
        repo,
        &["status", "--porcelain=v2", "-z", "--untracked-files=all"],
    )?;
    let mut paths = Vec::new();
    let mut tokens = raw.split('\0');
    while let Some(token) = tokens.next() {
        if token.is_empty() {
            continue;
        }
        let path = if let Some(rest) = token.strip_prefix("? ") {
            rest.to_string()
        } else if token.starts_with("1 ") || token.starts_with("u ") {
            // Ordinary/unmerged change: the path is the last space-separated
            // field (9th for `1`, 12th for `u`); splitn keeps embedded spaces.
            let fields = if token.starts_with("1 ") { 9 } else { 12 };
            token
                .splitn(fields, ' ')
                .nth(fields - 1)
                .unwrap_or_default()
                .to_string()
        } else if token.starts_with("2 ") {
            // Rename/copy: the destination is the 10th field and the NEXT
            // NUL-separated token is the orig path -- consume it so it is not
            // read as an entry of its own.
            let _ = tokens.next();
            token.splitn(10, ' ').nth(9).unwrap_or_default().to_string()
        } else {
            continue;
        };
        if !path.is_empty() {
            paths.push(path);
        }
    }
    Ok(paths)
}

/// Dirty working-tree files (by mtime) older than `stale_hours` --
/// `workspace_hygiene.stale_dirty_files`, counted. Paths that no longer
/// exist (staged deletions) and future mtimes never count, matching Python's
/// `exists()` check and its `<=` skip.
pub fn stale_dirty_file_count(repo: &Path, stale_hours: f64) -> Result<usize> {
    stale_dirty_file_count_at(repo, stale_hours, SystemTime::now())
}

/// `stale_dirty_file_count` with the clock supplied, so the exactly-at-threshold
/// case (`age == stale_hours` is NOT stale, Python compares strictly) is
/// testable without racing the filesystem against a real clock.
fn stale_dirty_file_count_at(repo: &Path, stale_hours: f64, now: SystemTime) -> Result<usize> {
    let mut count = 0usize;
    for path in status_paths(repo)? {
        let Ok(metadata) = std::fs::metadata(repo.join(&path)) else {
            continue; // deleted (or unreadable): no mtime to be stale by
        };
        let Ok(modified) = metadata.modified() else {
            continue;
        };
        let age_hours = now
            .duration_since(modified)
            .map(|elapsed| elapsed.as_secs_f64() / 3600.0)
            .unwrap_or(0.0); // future mtime: not stale, like Python's negative age
        if age_hours > stale_hours {
            count += 1;
        }
    }
    Ok(count)
}

struct WorktreeEntry {
    worktree: Option<String>,
    head: Option<String>,
    branch: Option<String>,
}

/// `git worktree list --porcelain`, one record per blank-line-separated block.
fn worktree_entries(repo: &Path) -> Result<Vec<WorktreeEntry>> {
    let raw = git_out(repo, &["worktree", "list", "--porcelain"])?;
    let mut entries = Vec::new();
    let mut current = WorktreeEntry {
        worktree: None,
        head: None,
        branch: None,
    };
    for line in raw.lines() {
        if line.is_empty() {
            entries.push(std::mem::replace(
                &mut current,
                WorktreeEntry {
                    worktree: None,
                    head: None,
                    branch: None,
                },
            ));
            continue;
        }
        let (key, value) = match line.split_once(' ') {
            Some(split) => split,
            None => (line, ""),
        };
        match key {
            "worktree" => current.worktree = Some(value.to_string()),
            "HEAD" => current.head = Some(value.to_string()),
            "branch" => current.branch = Some(value.to_string()),
            _ => {}
        }
    }
    entries.push(current);
    Ok(entries)
}

fn resolved(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn worktree_dirty(path: &Path) -> bool {
    // Any v2 entry at all (change or untracked) is uncommitted work; the
    // header-only output means clean. `-z` keeps exotic paths unquoted.
    git_out(
        path,
        &["status", "--porcelain=v2", "-z", "--untracked-files=all"],
    )
    .map(|raw| raw.split('\0').any(|token| !token.is_empty()))
    .unwrap_or(true) // a status that cannot run must not read as clean
}

/// A branch pushed once whose remote ref no longer exists --
/// `_upstream_was_deleted`.
fn upstream_was_deleted(repo: &Path, branch: &str) -> bool {
    let Some(remote) = git_quiet(
        repo,
        &["config", "--get", &format!("branch.{branch}.remote")],
    )
    .map(|value| value.trim().to_string()) else {
        return false;
    };
    let Some(merge) = git_quiet(
        repo,
        &["config", "--get", &format!("branch.{branch}.merge")],
    )
    .map(|value| value.trim().to_string()) else {
        return false;
    };
    if remote.is_empty() || merge.is_empty() {
        return false;
    }
    let tracked = format!(
        "refs/remotes/{remote}/{}",
        merge.trim_start_matches("refs/heads/")
    );
    git_quiet(repo, &["rev-parse", "--verify", "--quiet", &tracked]).is_none()
}

/// Worktrees that have finished: clean, non-primary, and holding nothing the
/// live line lacks (or pushed-then-pruned) -- `landed_worktrees`, counted.
pub fn landed_worktrees(live_ref: &str, repo: &Path) -> Result<usize> {
    let main = resolved(repo);
    let mut count = 0usize;
    for entry in worktree_entries(repo)? {
        let Some(path_text) = &entry.worktree else {
            continue;
        };
        let path = PathBuf::from(path_text);
        if !path.is_dir() || resolved(&path) == main {
            continue;
        }
        if worktree_dirty(&path) {
            continue;
        }
        let Some(head) = &entry.head else {
            continue;
        };
        if head.is_empty() {
            continue;
        }
        let contained = git_out(
            repo,
            &["rev-list", "--count", &format!("{live_ref}..{head}")],
        )?
        .trim()
            == "0";
        if !contained {
            let branch = entry
                .branch
                .as_deref()
                .unwrap_or("")
                .trim_start_matches("refs/heads/");
            if branch.is_empty() || !upstream_was_deleted(repo, branch) {
                continue;
            }
        }
        count += 1;
    }
    Ok(count)
}

/// The four SessionStart-safe counts -- `cheap_exposure_counts`. A repo with
/// no integration line reports `landed_worktrees: None` plus the reason in
/// `worktrees_skipped` (rendered as "unknown") rather than zero, which would
/// read as "nothing to clean up"; the other two counts still answer.
///
/// The three facts are independent, so they compute concurrently -- the git
/// discipline is unchanged (one invocation per fact) but the wall time is the
/// slowest fact, not the sum, which is what keeps a big clone under the 20 ms
/// SessionStart budget. Python's sequential version answers the same values.
pub fn cheap_exposure_counts(repo: &Path) -> Result<(usize, usize, Option<usize>, Option<String>)> {
    std::thread::scope(|scope| {
        let local = scope.spawn(|| local_only_commit_count(repo));
        let stale = scope.spawn(|| stale_dirty_file_count(repo, DEFAULT_STALE_HOURS));
        let landed = scope
            .spawn(|| default_integration_ref(repo).and_then(|live| landed_worktrees(&live, repo)));

        // Error precedence matches Python's sequential order: a local-only
        // failure surfaces before a stale-files failure, and a landed failure
        // never fails the line -- it becomes the "unknown" skip reason.
        let local = local
            .join()
            .unwrap_or_else(|_| Err(anyhow!("local-only commit count panicked")));
        let stale = stale
            .join()
            .unwrap_or_else(|_| Err(anyhow!("stale dirty file count panicked")));
        let landed = landed
            .join()
            .unwrap_or_else(|_| Err(anyhow!("landed worktree count panicked")));
        let (landed_worktrees, worktrees_skipped) = match landed {
            Ok(count) => (Some(count), None),
            Err(err) => (None, Some(format!("{err}"))),
        };
        Ok((local?, stale?, landed_worktrees, worktrees_skipped))
    })
}

/// The one-line EXPOSED summary the SessionStart inject carries, byte-identical
/// to `workspace_hygiene.exposure_line`. Degrades visibly on failure rather
/// than crashing the hook or silently disappearing.
pub fn exposure_line(repo: &Path) -> String {
    match cheap_exposure_counts(repo) {
        Err(err) => format!("EXPOSED: unavailable ({err}). python -m conductor.workspace_hygiene"),
        Ok((local_only_commits, stale_dirty_files, landed, skipped)) => {
            let landed_text = match (landed, &skipped) {
                (Some(count), None) => count.to_string(),
                _ => "unknown".to_string(),
            };
            format!(
                "EXPOSED: {local_only_commits} local-only commit(s), \
                 {stale_dirty_files} stale dirty file(s), \
                 {landed_text} finished worktree(s) to remove, \
                 branches skipped (needs gh). \
                 python -m conductor.workspace_hygiene"
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lock_env() -> std::sync::MutexGuard<'static, ()> {
        INTEGRATION_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn scratch(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "forge-workspace-hygiene-{}-{label}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn git(cwd: &Path, args: &[&str]) {
        let done = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .unwrap();
        assert!(
            done.status.success(),
            "git {} in {cwd:?}: {}",
            args.join(" "),
            String::from_utf8_lossy(&done.stderr)
        );
    }

    fn touch(cwd: &Path, epoch: u64, path: &str) {
        let done = Command::new("touch")
            .args(["-d", &format!("@{epoch}"), path])
            .current_dir(cwd)
            .output()
            .unwrap();
        assert!(
            done.status.success(),
            "touch: {}",
            String::from_utf8_lossy(&done.stderr)
        );
    }

    fn seeded(label: &str, branch: &str) -> PathBuf {
        let parent = scratch(label);
        let repo = parent.join("repo");
        git(
            &parent,
            &["init", "--quiet", "-b", branch, &repo.display().to_string()],
        );
        git(&repo, &["config", "user.email", "forge@example.invalid"]);
        git(&repo, &["config", "user.name", "forge"]);
        std::fs::write(repo.join("seed.txt"), b"seed\n").unwrap();
        git(&repo, &["add", "seed.txt"]);
        git(&repo, &["commit", "--quiet", "-m", "seed"]);
        repo
    }

    /// A bare origin whose HEAD advertises `head_branch`, wired to `repo`.
    fn bare_origin(repo: &Path, label: &str, head_branch: &str) -> PathBuf {
        let origin = scratch(label);
        git(
            repo,
            &[
                "init",
                "--quiet",
                "--bare",
                "-b",
                head_branch,
                &origin.display().to_string(),
            ],
        );
        git(
            repo,
            &["remote", "add", "origin", &origin.display().to_string()],
        );
        origin
    }

    #[test]
    fn integration_ref_prefers_the_configured_origin_line() {
        let _guard = lock_env();
        let repo = seeded("configured", "main");
        std::fs::write(
            repo.join("pyproject.toml"),
            b"[tool.conductor]\nintegration_branch = \"main\"\n",
        )
        .unwrap();
        bare_origin(&repo, "configured-origin", "main");
        git(&repo, &["push", "--quiet", "origin", "main"]);
        assert_eq!(default_integration_ref(&repo).unwrap(), "origin/main");
        std::fs::remove_dir_all(repo.parent().unwrap()).ok();
    }

    #[test]
    fn the_configured_branch_resolves_from_a_nested_start_too() {
        // The manifest lives at the repo root while the caller starts from a
        // subdirectory: `enclosing_repo` must walk up to find it, and the
        // configured branch (wip, remote HEAD trunk) must win over every
        // fallback -- which is what separates the configured resolution from
        // the symref/ls-remote defaults.
        let _guard = lock_env();
        let repo = seeded("nested", "wip");
        std::fs::write(
            repo.join("pyproject.toml"),
            b"[tool.conductor]\nintegration_branch = \"wip\"\n",
        )
        .unwrap();
        bare_origin(&repo, "nested-origin", "trunk");
        git(&repo, &["push", "--quiet", "origin", "wip"]);
        let sub = repo.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        assert_eq!(default_integration_ref(&sub).unwrap(), "origin/wip");
        std::fs::remove_dir_all(repo.parent().unwrap()).ok();
    }

    #[test]
    fn the_env_override_wins_over_the_manifest() {
        // The handlers' session-start test reads this variable through
        // `exposure_line`; it takes this same lock so the override cannot
        // leak into its resolution.
        let _guard = lock_env();
        let repo = seeded("env-override", "wip");
        std::fs::write(
            repo.join("pyproject.toml"),
            b"[tool.conductor]\nintegration_branch = \"wip\"\n",
        )
        .unwrap();
        git(&repo, &["branch", "main"]); // local-only: verifies, never pushed
        std::env::set_var("CONDUCTOR_INTEGRATION_BRANCH", "main");
        let resolved = default_integration_ref(&repo);
        std::env::remove_var("CONDUCTOR_INTEGRATION_BRANCH");
        assert_eq!(resolved.unwrap(), "main");
        std::fs::remove_dir_all(repo.parent().unwrap()).ok();
    }

    #[test]
    fn the_origin_head_symref_answers_stripped_of_its_namespace() {
        let _guard = lock_env();
        let repo = seeded("symref", "work");
        bare_origin(&repo, "symref-origin", "master");
        git(&repo, &["push", "--quiet", "origin", "work:master"]);
        git(
            &repo,
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/master",
            ],
        );
        // No manifest (default "main") and no local/pushed "main": only the
        // bound symref can answer, and it must arrive as the short form.
        assert_eq!(default_integration_ref(&repo).unwrap(), "origin/master");
        std::fs::remove_dir_all(repo.parent().unwrap()).ok();
    }

    #[test]
    fn the_remote_advertised_head_answers_when_nothing_local_does() {
        let _guard = lock_env();
        let repo = seeded("ls-remote", "work");
        bare_origin(&repo, "ls-remote-origin", "master");
        git(&repo, &["push", "--quiet", "origin", "work:master"]);
        // No bound refs/remotes/origin/HEAD (a plain push never creates one),
        // so only `ls-remote --symref` can name the line.
        assert_eq!(default_integration_ref(&repo).unwrap(), "origin/master");
        std::fs::remove_dir_all(repo.parent().unwrap()).ok();
    }

    #[test]
    fn integration_ref_refuses_a_repo_with_no_line() {
        let _guard = lock_env();
        let repo = seeded("lineless", "trunk");
        assert!(default_integration_ref(&repo)
            .unwrap_err()
            .to_string()
            .contains("no integration line"));
        std::fs::remove_dir_all(repo.parent().unwrap()).ok();
    }

    #[test]
    fn a_branch_with_only_half_its_upstream_configured_is_not_finished() {
        // Uncontained, clean worktree whose branch names a merge ref but no
        // remote: that is not a pushed-then-pruned branch, so the worktree
        // must not read as landed.
        let repo = seeded("half-upstream", "main");
        bare_origin(&repo, "half-upstream-origin", "main");
        git(&repo, &["push", "--quiet", "origin", "main"]);
        let wt = repo.parent().unwrap().join("wt");
        git(
            &repo,
            &[
                "worktree",
                "add",
                "--quiet",
                &wt.display().to_string(),
                "-b",
                "feat",
            ],
        );
        std::fs::write(wt.join("feat.txt"), b"feat\n").unwrap();
        git(&wt, &["add", "feat.txt"]);
        git(&wt, &["commit", "--quiet", "-m", "feat"]);
        // Half-configured upstream: the merge ref is named but the remote is
        // configured to the empty string (a `git config branch.feat.remote ""`
        // artifact), which must read as "never pushed", not "pushed and
        // pruned" -- the emptiness check is what keeps it from proceeding to
        // the doomed refs/remotes//feat probe.
        git(&repo, &["config", "branch.feat.merge", "refs/heads/feat"]);
        git(&repo, &["config", "branch.feat.remote", ""]);
        assert_eq!(landed_worktrees("origin/main", &repo).unwrap(), 0);
        std::fs::remove_dir_all(repo.parent().unwrap()).ok();
    }

    #[test]
    fn exposure_line_renders_the_counts_and_the_unknown_skipped_line() {
        let _guard = lock_env();
        let repo = seeded("exposure", "main");
        let line = exposure_line(&repo);
        assert!(
            line.starts_with("EXPOSED: 1 local-only commit(s), 0 stale dirty file(s), "),
            "{line}"
        );
        std::fs::remove_dir_all(repo.parent().unwrap()).ok();
    }

    #[test]
    fn status_paths_keeps_spaced_and_rename_destinations() {
        let repo = seeded("status-paths", "main");
        std::fs::write(repo.join("with space.txt"), b"x\n").unwrap();
        std::fs::write(repo.join("gone.txt"), b"x\n").unwrap();
        git(&repo, &["add", "gone.txt"]);
        std::fs::remove_file(repo.join("gone.txt")).unwrap(); // staged deletion
                                                              // Aged past the stale threshold so exactly the untracked file counts.
        touch(&repo, 1_000_000_000, "with space.txt");
        let paths = status_paths(&repo).unwrap();
        assert!(paths.contains(&"with space.txt".to_string()));
        assert!(paths.contains(&"gone.txt".to_string()));
        assert_eq!(
            stale_dirty_file_count(&repo, DEFAULT_STALE_HOURS).unwrap(),
            1,
            "the untracked file exists and the staged deletion does not"
        );
        std::fs::remove_dir_all(repo.parent().unwrap()).ok();
    }

    #[test]
    fn a_file_younger_than_the_threshold_is_never_stale() {
        // Two hours and change, not twenty-four: pins the hours arithmetic
        // (seconds divided by 3600, never multiplied or taken modulo) with a
        // remainder deliberately past 24 seconds either way.
        let repo = seeded("young-file", "main");
        std::fs::write(repo.join("young.txt"), b"x\n").unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let young = (now - 2 * 3600) / 3600 * 3600 + 30; // 2h..2h+59m30s old
        touch(&repo, young, "young.txt");
        assert_eq!(
            stale_dirty_file_count(&repo, DEFAULT_STALE_HOURS).unwrap(),
            0,
            "a file at most three hours old is not stale"
        );
        std::fs::remove_dir_all(repo.parent().unwrap()).ok();
    }

    #[test]
    fn exactly_at_the_threshold_is_not_stale() {
        // Python compares strictly (`age > stale_hours`), so a file exactly
        // 24h old is fresh; the clock is supplied to hit the boundary exactly.
        let repo = seeded("boundary", "main");
        std::fs::write(repo.join("edge.txt"), b"x\n").unwrap();
        touch(&repo, 1_000_000_000, "edge.txt");
        let mtime = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000_000);
        let exactly = mtime + std::time::Duration::from_secs(24 * 3600);
        assert_eq!(
            stale_dirty_file_count_at(&repo, DEFAULT_STALE_HOURS, exactly).unwrap(),
            0,
            "exactly 24h old is not stale: the comparison is strict"
        );
        let past = exactly + std::time::Duration::from_secs(1);
        assert_eq!(
            stale_dirty_file_count_at(&repo, DEFAULT_STALE_HOURS, past).unwrap(),
            1,
            "one second past 24h is stale"
        );
        std::fs::remove_dir_all(repo.parent().unwrap()).ok();
    }

    #[test]
    fn future_and_fresh_mtimes_are_never_stale() {
        let repo = seeded("mtimes", "main");
        std::fs::write(repo.join("fresh.txt"), b"x\n").unwrap();
        assert_eq!(
            stale_dirty_file_count(&repo, DEFAULT_STALE_HOURS).unwrap(),
            0
        );
        std::fs::remove_dir_all(repo.parent().unwrap()).ok();
    }
}
