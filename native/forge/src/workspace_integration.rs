//! Shared integration-line resolution and bounded workspace Git probes.
//! Explicit reports may allow remote HEAD advertisement; session paths stay offline.
use anyhow::{anyhow, Result};
use std::{
    path::{Path, PathBuf},
    process::{Command, Output},
};

pub fn probe(repo: &Path, args: &[&str]) -> Result<Output> {
    let mut command = Command::new("git");
    command.current_dir(repo);
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    command
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null");
    // Report probes must not activate repository diff/textconv/fsmonitor hooks
    // or credential/SSH-command overrides. Git/SSH themselves remain operator tools.
    for setting in [
        "core.fsmonitor=false",
        "core.hooksPath=/dev/null",
        "credential.helper=",
        "credential.interactive=false",
        "core.sshCommand=ssh",
        "remote.origin.uploadpack=git-upload-pack",
        "protocol.allow=never",
        "protocol.file.allow=always",
        "protocol.https.allow=always",
        "protocol.http.allow=always",
        "protocol.ssh.allow=always",
    ] {
        command.args(["-c", setting]);
    }
    if let Some((first, rest)) = args.split_first() {
        command.arg(first);
        if matches!(*first, "diff" | "show") {
            command.args(["--no-ext-diff", "--no-textconv"]);
        }
        if *first == "ls-remote" {
            command.arg("--upload-pack=git-upload-pack");
        }
        command.args(rest);
    }
    crate::process_probe::run(
        &mut command,
        std::time::Duration::from_secs(15),
        1024 * 1024,
    )
}

pub fn git(repo: &Path, args: &[&str]) -> Result<String> {
    let done = probe(repo, args)?;
    if !done.status.success() {
        return Err(anyhow!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&done.stderr).trim()
        ));
    }
    String::from_utf8(done.stdout).map_err(|error| anyhow!("Git output is not UTF-8: {error}"))
}

fn git_query(repo: &Path, args: &[&str]) -> Result<Option<String>> {
    let done = probe(repo, args)?;
    Ok(done
        .status
        .success()
        .then(|| String::from_utf8_lossy(&done.stdout).into_owned()))
}

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

fn host_root(start: &Path) -> PathBuf {
    let resolved = std::fs::canonicalize(start).unwrap_or_else(|_| start.to_path_buf());
    enclosing_repo(&resolved).unwrap_or(resolved)
}

fn configured_integration_branch(root: &Path) -> Result<String> {
    conductor_native::project_paths::integration_branch(root).map_err(anyhow::Error::msg)
}

pub fn default_integration_ref(repo: &Path, allow_network: bool) -> Result<String> {
    let branch = configured_integration_branch(&host_root(repo))?;
    let candidates = [format!("origin/{branch}"), branch];
    for refname in &candidates {
        if git_query(repo, &["rev-parse", "--verify", "--quiet", refname])?.is_some() {
            return Ok(refname.clone());
        }
    }
    if let Some(bound) = git_query(
        repo,
        &["symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"],
    )? {
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
    // Offline: a repo whose line is conventional (master or main) and whose
    // fetch already created the remote-tracking ref needs no network to name
    // it. Exactly one answers; both present is ambiguous and falls through --
    // guessing between them could judge containment against the wrong line.
    let conventional = conventional_remote_refs(repo)?;
    if conventional.len() == 1 {
        return Ok(conventional[0].clone());
    }
    if !allow_network {
        return Err(anyhow!(
            "{}: no integration line to judge containment against \
             (tried {}, {}; origin HEAD symref unavailable; \
             network advertisement skipped)",
            repo.display(),
            candidates[0],
            candidates[1]
        ));
    }
    if let Some(advertised) = git_query(repo, &["ls-remote", "--symref", "origin", "HEAD"])? {
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

fn conventional_remote_refs(repo: &Path) -> Result<Vec<String>> {
    Ok(git_query(
        repo,
        &[
            "for-each-ref",
            "--format=%(refname:short)",
            "refs/remotes/origin/master",
            "refs/remotes/origin/main",
        ],
    )?
    .map(|listed| listed.split_whitespace().map(str::to_owned).collect())
    .unwrap_or_default())
}
