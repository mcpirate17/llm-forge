//! Differential parity test for the SessionStart EXPOSED line ported to
//! `native/forge/src/workspace_hygiene.rs` (`workspace_exposure_session`).
//!
//! Unlike a live differential test, this file spawns no Python interpreter at
//! `cargo test` time: `tests/fixtures/workspace_exposure_corpus.json` and
//! `workspace_exposure_expected.json` are a frozen corpus, generated once by
//! running the REAL, unmodified `conductor.workspace_hygiene.exposure_line`
//! over repositories rebuilt from the corpus recipes under deterministic
//! conditions (every mtime the line can see pinned to a fixed epoch, left at
//! "now", or pushed into the future; the line embeds no paths) -- this file
//! loads that frozen ground truth via `include_str!`, rebuilds the same
//! repository states from the same recipes, and compares Rust's own
//! computation against it directly.
//!
//! `src/tooling/hooks/claude/test_workspace_exposure_parity_corpus.py` is the
//! Python-side twin: it loads the SAME two fixtures, independently rebuilds
//! the states via its own builder, and asserts Python still matches the same
//! frozen lines. Together the two tests pin both implementations to one
//! shared ground truth instead of comparing them to each other at test time,
//! following the same shape as `tool_quiet_parity.rs`/its Python twin.
//!
//! This crate has no lib target: `workspace_hygiene.rs` is pulled in via
//! `#[path]`, the same way every other parity test in this crate includes its
//! module under test.

#[path = "../src/workspace_hygiene.rs"]
mod workspace_hygiene;

use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;

/// `workspace_hygiene::configured_integration_branch` reads the
/// process-global `CONDUCTOR_INTEGRATION_BRANCH`; the corpus cases must
/// resolve their line from the recipe alone, so it stays unset throughout
/// (held under a lock like every other env-mutating test in this crate).
static ENV_LOCK: Mutex<()> = Mutex::new(());

static COUNTER: AtomicU32 = AtomicU32::new(0);

struct ScratchDir(PathBuf);

impl ScratchDir {
    fn new(label: &str) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "forge-workspace-exposure-parity-{}-{label}-{n}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).unwrap();
        ScratchDir(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
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

fn write(path: &Path, text: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, text).unwrap();
}

/// Pin a file's mtime exactly as the Python twin does: a fixed epoch via
/// `touch -d`, "now" left alone (the file was just written).
fn apply_mtime(repo: &Path, relative: &str, mtime: &Value) {
    if mtime.as_str() == Some("now") {
        return;
    }
    let epoch = mtime
        .as_i64()
        .unwrap_or_else(|| panic!("mtime must be an epoch or \"now\", got {mtime:?}"));
    let done = Command::new("touch")
        .args(["-d", &format!("@{epoch}"), relative])
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(
        done.status.success(),
        "touch -d @{epoch} {relative}: {}",
        String::from_utf8_lossy(&done.stderr)
    );
}

/// Rebuild one corpus case's repository state from its recipe -- the same
/// semantics as the Python twin's `build_case`, implemented independently so
/// agreement is evidence about the implementations, not the builders.
fn build_case(scratch: &Path, case: &Value) -> PathBuf {
    let branch = case["branch"].as_str().unwrap();
    let id = case["id"].as_str().unwrap();
    let repo = scratch.join(format!("{id}-repo"));
    if case["origin"].as_bool().unwrap() {
        git(
            scratch,
            &[
                "init",
                "--quiet",
                "--bare",
                "-b",
                branch,
                &scratch
                    .join(format!("{id}-origin.git"))
                    .display()
                    .to_string(),
            ],
        );
    }
    git(
        scratch,
        &["init", "--quiet", "-b", branch, &repo.display().to_string()],
    );
    git(&repo, &["config", "user.email", "parity@example.invalid"]);
    git(&repo, &["config", "user.name", "parity"]);
    write(&repo.join("seed.txt"), "seed\n");
    if let Some(integration) = case["integration"].as_str() {
        write(
            &repo.join("pyproject.toml"),
            &format!("[tool.conductor]\nintegration_branch = \"{integration}\"\n"),
        );
        git(&repo, &["add", "pyproject.toml"]);
    }
    git(&repo, &["add", "seed.txt"]);
    git(&repo, &["commit", "--quiet", "-m", "seed"]);
    if case["origin"].as_bool().unwrap() {
        git(
            &repo,
            &[
                "remote",
                "add",
                "origin",
                &scratch
                    .join(format!("{id}-origin.git"))
                    .display()
                    .to_string(),
            ],
        );
        git(&repo, &["push", "--quiet", "origin", branch]);
    }
    if let Some(head) = case["origin_head"].as_str() {
        git(
            &repo,
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                &format!("refs/remotes/origin/{head}"),
            ],
        );
    }
    for commit_name in case["commits"].as_array().unwrap() {
        let name = commit_name.as_str().unwrap();
        write(&repo.join(name), &format!("{name}\n"));
        git(&repo, &["add", name]);
        git(&repo, &["commit", "--quiet", "-m", name]);
    }
    for entry in case["files"].as_array().unwrap() {
        let relative = entry["path"].as_str().unwrap();
        let target = repo.join(relative);
        if entry["kind"].as_str() == Some("modify_tracked") {
            write(&target, "modified\n");
        } else {
            write(&target, "untracked\n");
        }
        apply_mtime(&repo, relative, &entry["mtime"]);
    }
    for (index, worktree) in case["worktrees"].as_array().unwrap().iter().enumerate() {
        let wt_branch = worktree["branch"].as_str().unwrap();
        let wt_path = scratch.join(format!("{id}-wt{index}"));
        // "pushed" registers the worktree at the line that was already pushed
        // (a merged feature tree while main moved on); the default is HEAD.
        let start = if worktree["at"].as_str() == Some("pushed") {
            format!("origin/{branch}")
        } else {
            "HEAD".to_string()
        };
        git(
            &repo,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                wt_branch,
                &wt_path.display().to_string(),
                &start,
            ],
        );
        if let Some(commit_name) = worktree["commit"].as_str() {
            write(&wt_path.join(commit_name), &format!("{commit_name}\n"));
            git(&wt_path, &["add", commit_name]);
            git(&wt_path, &["commit", "--quiet", "-m", commit_name]);
        }
        if worktree["pruned_upstream"].as_bool().unwrap() {
            git(&wt_path, &["push", "--quiet", "-u", "origin", wt_branch]);
            git(
                &wt_path,
                &["push", "--quiet", "origin", "--delete", wt_branch],
            );
        }
        if worktree["dirty"].as_bool().unwrap() {
            write(&wt_path.join("leftover.txt"), "leftover\n");
        }
    }
    repo
}

#[test]
fn workspace_exposure_native_line_matches_the_frozen_corpus() {
    let _guard = ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    std::env::remove_var("CONDUCTOR_INTEGRATION_BRANCH");

    let corpus: Vec<Value> =
        serde_json::from_str(include_str!("fixtures/workspace_exposure_corpus.json"))
            .expect("workspace_exposure_corpus.json must be valid JSON");
    assert!(
        corpus.len() >= 12,
        "expected 12+ corpus cases, got {}",
        corpus.len()
    );
    let expected: BTreeMap<String, String> =
        serde_json::from_str(include_str!("fixtures/workspace_exposure_expected.json"))
            .expect("workspace_exposure_expected.json must be valid JSON");
    assert_eq!(
        expected.len(),
        corpus.len(),
        "every corpus case needs exactly one frozen expected line"
    );

    let mut failures = Vec::new();
    for case in &corpus {
        let id = case["id"].as_str().unwrap();
        let scratch = ScratchDir::new(id);
        let repo = build_case(scratch.path(), case);
        let actual = workspace_hygiene::exposure_line(&repo);
        let expected_line = expected
            .get(id)
            .unwrap_or_else(|| panic!("no frozen expected line for case {id:?}"));
        if &actual != expected_line {
            failures.push(format!(
                "case {id:?}:\n  rust=    {actual}\n  expected={expected_line}"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} parity cases disagreed:\n{}",
        failures.len(),
        corpus.len(),
        failures.join("\n\n")
    );
}
