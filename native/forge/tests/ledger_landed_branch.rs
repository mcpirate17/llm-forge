//! `forge ledger landed --branch` tests (PR #45): the default branch is
//! resolved from `refs/remotes/origin/HEAD`, falling back to `main`. The
//! LLM monorepo integrates on `master` and has no `main` at all -- before
//! this flag, `forge ledger rollup --repo /home/tim/Projects/LLM` died with
//! "ambiguous argument 'main'" -- so the resolution and the fallback both
//! get a real (synthetic) git repository each, not a parse-level fixture.
//!
//! Same `#[path]`-inclusion pattern as the other ledger integration tests:
//! this binary crate has no lib target.

#[path = "../src/json_canon.rs"]
#[allow(dead_code)]
mod json_canon;
#[path = "../src/ledger/mod.rs"]
#[allow(dead_code)]
mod ledger;

use std::fs;
use std::path::{Path, PathBuf};

fn git(dir: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "forge-test")
        .env("GIT_AUTHOR_EMAIL", "forge-test@example.invalid")
        .env("GIT_COMMITTER_NAME", "forge-test")
        .env("GIT_COMMITTER_EMAIL", "forge-test@example.invalid")
        .output()
        .expect("git runs");
    assert!(
        output.status.success(),
        "git {:?} in {} failed: {}",
        args,
        dir.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// A one-commit repo on `branch`; `set_origin_head` additionally points
/// `refs/remotes/origin/HEAD` at `refs/remotes/origin/<branch>` (the state
/// a real clone reaches after a fetch).
fn fixture_repo(name: &str, branch: &str, set_origin_head: bool) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("forge-ledger-landed-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "-q", "-b", branch]);
    fs::write(dir.join("f.txt"), "one\n").unwrap();
    git(&dir, &["add", "."]);
    git(
        &dir,
        &["commit", "-q", "-m", "first landed (#1)\n\nAgent: glm"],
    );
    if set_origin_head {
        let sha = git(&dir, &["rev-parse", "HEAD"]);
        git(
            &dir,
            &["update-ref", &format!("refs/remotes/origin/{branch}"), &sha],
        );
        git(
            &dir,
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                &format!("refs/remotes/origin/{branch}"),
            ],
        );
    }
    dir
}

/// The LLM-monorepo shape: default branch `master`, origin/HEAD set. With no
/// `--branch`, `scan` must resolve `refs/remotes/origin/HEAD` and find the
/// commit -- the exact case that failed with "ambiguous argument 'main'"
/// before the resolution existed.
#[test]
fn default_branch_resolves_origin_head_on_a_master_repo() {
    let dir = fixture_repo("master-origin", "master", true);
    let rows = ledger::landed::scan(&dir, None, None, None).expect("scan resolves origin/HEAD");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].pr_number, Some(1));
    assert_eq!(rows[0].agent_names, vec!["glm".to_string()]);
    let _ = fs::remove_dir_all(&dir);
}

/// A repo with no origin/HEAD at all still scans when the caller names the
/// branch explicitly.
#[test]
fn explicit_branch_scans_without_origin_head() {
    let dir = fixture_repo("explicit-trunk", "trunk", false);
    let rows =
        ledger::landed::scan(&dir, None, None, Some("trunk")).expect("explicit branch scans");
    assert_eq!(rows.len(), 1);
    let _ = fs::remove_dir_all(&dir);
}

/// A plain `main` repo with no origin/HEAD (a fresh `git init -b main`):
/// the fallback resolves `main` and the scan works.
#[test]
fn fallback_resolves_main_when_no_origin_head() {
    let dir = fixture_repo("main-fallback", "main", false);
    let rows = ledger::landed::scan(&dir, None, None, None).expect("fallback main scans");
    assert_eq!(rows.len(), 1);
    let _ = fs::remove_dir_all(&dir);
}

/// A `master`-default repo with no origin/HEAD and no `--branch`: the
/// fallback names `main`, which does not exist there, so `git log` fails
/// and `scan` fails loud rather than silently scanning nothing -- the same
/// failure the LLM monorepo produced before `--branch`, now reachable only
/// by a repo that genuinely has neither signal.
#[test]
fn master_repo_without_origin_head_or_branch_fails_loud() {
    let dir = fixture_repo("master-nohead", "master", false);
    assert!(
        ledger::landed::scan(&dir, None, None, None).is_err(),
        "scan must fail loud when neither --branch nor origin/HEAD can name the branch"
    );
    let _ = fs::remove_dir_all(&dir);
}
