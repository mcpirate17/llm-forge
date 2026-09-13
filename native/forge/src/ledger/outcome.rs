//! The outcome join (Phase 3 step 3, item 3; `docs/design/cost_ledger.md`
//! section 5): per `task_dispatch` row, did the dispatch's session actually
//! land, did it need a higher-tier follow-up commit before it did, and did
//! its PR's first push go red on CI. `gh` is not available to Rust, so this
//! reads a cached JSON `ledger/ci_history/<owner>_<repo>.json` (schema
//! below, also documented in `docs/ledger.md`) rather than shelling out --
//! the fetcher that populates that file is a separate, GLM-owned slice
//! (`docs/ledger.md` "ci_history fetcher, not yet built").
//!
//! **Absent file -> all three fields `null`, never `false`**: this module
//! either has the full picture (the cache exists) or it says so honestly by
//! leaving every row alone. It never derives `landed` from `agent_rollup`'s
//! own join data alone while leaving the other two fields unset -- the
//! three fields are one atomic unit gated on the same cache.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

use super::agent::CommitJoin;
use super::rollup::TaskDispatchRow;

/// One PR's cached CI/commit history. `commits` is the *pre-squash* list of
/// commits on the PR branch (in the order `gh api
/// repos/{owner}/{repo}/pulls/{n}/commits` returns them) -- `git log
/// --first-parent main` only ever sees the single post-squash commit, which
/// is why this needs its own cache at all (`docs/ledger.md`'s schema
/// section).
#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)] // `branch`/`first_push_sha` are part of the documented
                    // cache schema (`docs/ledger.md`) even though today's `apply()` only reads
                    // `first_push_ci`/`commits` -- a future debug/audit path (or a fetcher
                    // correctness check) reads them, and the schema must round-trip in full.
pub struct CiHistoryPr {
    pub branch: String,
    pub first_push_sha: String,
    pub first_push_ci: CiStatus,
    pub commits: Vec<CiHistoryCommit>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CiStatus {
    Green,
    Red,
    Unknown,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)] // `sha`/`subject` round-trip the documented schema; only
                    // `trailers` feeds `required_rework` today.
pub struct CiHistoryCommit {
    pub sha: String,
    pub subject: String,
    #[serde(default)]
    pub trailers: CiHistoryTrailers,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[allow(dead_code)] // `claude_session` documents the schema `docs/ledger.md`
                    // promises; only `agent` feeds `required_rework`'s tier lookup today.
pub struct CiHistoryTrailers {
    #[serde(rename = "Agent")]
    pub agent: Option<String>,
    #[serde(rename = "Claude-Session")]
    pub claude_session: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)] // `fetched_utc` documents cache freshness for a human or a
                    // future staleness check; `apply()` itself does not gate on it.
pub struct CiHistoryFile {
    pub fetched_utc: String,
    pub prs: BTreeMap<String, CiHistoryPr>,
}

/// Loads `path`. `Ok(None)` when the file does not exist at all -- the
/// documented, honest "no data yet" case. Any other failure (unreadable,
/// malformed JSON) is a real error: the file existing but being wrong is
/// not the same as it never having been fetched.
pub fn load(path: &Path) -> Result<Option<CiHistoryFile>> {
    if !path.is_file() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading ci_history cache {}", path.display()))?;
    let parsed: CiHistoryFile = serde_json::from_str(&text)
        .with_context(|| format!("parsing ci_history cache {}", path.display()))?;
    Ok(Some(parsed))
}

/// `<repo>/ledger/ci_history/<owner>_<name>.json` -- parallel to
/// `ledger/routing_policy.toml`'s own repo-root `ledger/` directory, except
/// this one is read from disk at run time (it is refreshed periodically by
/// the GLM fetcher, not baked into the binary via `include_str!`).
pub fn ci_history_path(repo: &Path) -> Result<std::path::PathBuf> {
    let slug = owner_repo_slug(repo)?;
    Ok(repo
        .join("ledger")
        .join("ci_history")
        .join(format!("{slug}.json")))
}

/// Parses `owner_name` out of `origin`'s remote URL (`git@github.com:o/n.git`
/// or `https://github.com/o/n`), lower-cased and `.git`-stripped.
fn owner_repo_slug(repo: &Path) -> Result<String> {
    let output = std::process::Command::new("git")
        .args(["-C"])
        .arg(repo)
        .args(["remote", "get-url", "origin"])
        .output()
        .context("running git remote get-url origin")?;
    if !output.status.success() {
        anyhow::bail!(
            "git remote get-url origin exited {}: {}",
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let url = String::from_utf8_lossy(&output.stdout).trim().to_string();
    parse_owner_repo(&url)
        .with_context(|| format!("could not parse an owner/repo out of remote URL {url:?}"))
}

fn parse_owner_repo(url: &str) -> Option<String> {
    let stripped = url.trim_end_matches(".git").trim_end_matches('/');
    let tail = stripped
        .split("github.com")
        .nth(1)?
        .trim_start_matches([':', '/']);
    let (owner, name) = tail.rsplit_once('/')?;
    if owner.is_empty() || name.is_empty() {
        return None;
    }
    Some(format!("{owner}_{name}"))
}

/// The routing policy's own tier order (`ledger/routing_policy.toml`),
/// re-embedded here rather than importing `route::Policy` for one lookup --
/// `route.rs` already asserts `tier_order` is non-empty at parse time, so a
/// hardcoded fallback here would only ever be exercised by a policy file
/// this repo has never shipped.
const TIER_ORDER: [&str; 4] = ["haiku", "sonnet", "opus", "fable"];

fn tier_rank(tier: &str) -> usize {
    TIER_ORDER
        .iter()
        .position(|t| t.eq_ignore_ascii_case(tier))
        .unwrap_or(TIER_ORDER.len())
}

/// A two-tier heuristic mapping an `Agent:` trailer name to the model tier
/// it is presumed to run at, for `required_rework`'s tier comparison: this
/// repo's own convention (`AGENTS.md`, `docs/roadmap.md`'s Ownership rule)
/// is that any `glm`-named agent is the cheap tier and every other agent
/// name (a Claude coordinator session) is not. This is a documented
/// approximation, not a measured fact per commit -- a future
/// `Agent:`-to-tier registry would replace it; until then it is the only
/// signal a bare trailer name gives.
fn agent_tier(agent_name: &str) -> &'static str {
    if agent_name.to_ascii_lowercase().contains("glm") {
        "haiku"
    } else {
        "sonnet"
    }
}

/// `required_rework`'s approximation: among a PR's commits *after the
/// first*, is there one whose `Agent:` trailer maps to a tier strictly
/// above `row_tier`. The first commit is treated as the dispatch's own
/// work (the ci_history cache carries no other way to say which commit was
/// whose); anything after it that escalates tier is read as a rework signal.
/// `None` when the PR's commit list has no `Agent:` trailer information at
/// all worth comparing.
fn required_rework(commits: &[CiHistoryCommit], row_tier: &str) -> Option<bool> {
    if commits.len() < 2 {
        return Some(false);
    }
    let row_rank = tier_rank(row_tier);
    let escalated = commits[1..].iter().any(|commit| {
        commit
            .trailers
            .agent
            .as_deref()
            .is_some_and(|agent| tier_rank(agent_tier(agent)) > row_rank)
    });
    Some(escalated)
}

/// Finds the PR number a `session_id` landed, via the same `CommitJoin`s
/// `agent_rollup` already computed (`session_id` -> commit -> PR). Takes
/// the first match; a session credited to more than one landed commit is
/// already reported as `ambiguous` upstream in `rollup.rs`'s own stderr
/// summary.
fn pr_for_session(joins: &[CommitJoin], session_id: &str) -> Option<u64> {
    joins
        .iter()
        .find(|join| join.session_ids.iter().any(|s| s == session_id))
        .and_then(|join| join.pr_number)
}

/// Fills `landed`/`required_rework`/`ci_red_on_first_push` on every row in
/// `rows` whose `parent_session_id` is set, gated entirely on
/// `<repo>`'s ci_history cache existing (`load`'s `None` leaves every row
/// untouched -- they start `None`). Called from `rollup::run` right after
/// the `--repo` join, so `joins` is the same `Vec<CommitJoin>`
/// `agent::build_agent_rollup` just produced.
pub fn apply(
    rows: &mut [(String, TaskDispatchRow)],
    joins: &[CommitJoin],
    repo: &Path,
) -> Result<()> {
    // A `repo` with no git metadata or no `origin` remote (a scratch dir in
    // a test, or a checkout mid-setup) can't even be named as an
    // owner/repo slug -- that is the same "no data yet" case as the cache
    // file itself being absent, not a hard error: every row's three fields
    // stay honestly `None` rather than failing the whole rollup.
    let Ok(path) = ci_history_path(repo) else {
        return Ok(());
    };
    let Some(history) = load(&path)? else {
        return Ok(());
    };
    for (_, row) in rows.iter_mut() {
        let Some(session_id) = row.parent_session_id.as_deref() else {
            continue;
        };
        let Some(pr_number) = pr_for_session(joins, session_id) else {
            // The cache exists and covers every landed PR this rollup knows
            // about; a session with no PR join in it is a real, honest
            // negative -- it has not landed, per current knowledge.
            row.landed = Some(false);
            continue;
        };
        row.landed = Some(true);
        let Some(entry) = history.prs.get(&pr_number.to_string()) else {
            // Landed, but this PR is not in the cache yet (fetcher lag) --
            // the other two fields stay honestly unknown.
            continue;
        };
        row.ci_red_on_first_push = match entry.first_push_ci {
            CiStatus::Green => Some(false),
            CiStatus::Red => Some(true),
            CiStatus::Unknown => None,
        };
        let row_tier = row.tier.as_deref().unwrap_or("sonnet");
        row.required_rework = required_rework(&entry.commits, row_tier);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The frozen fixture the brief asks for: one PR that needed a
    /// higher-tier follow-up (glm -> llm-b0) and went red on first push,
    /// one clean single-commit PR, and one PR the cache does not cover yet.
    const FIXTURE_JSON: &str = r#"{
        "fetched_utc": "2026-09-13T00:00:00Z",
        "prs": {
            "50": {
                "branch": "forge/routing-policy",
                "first_push_sha": "aaa111",
                "first_push_ci": "red",
                "commits": [
                    {"sha": "aaa111", "subject": "feat: first cut", "trailers": {"Agent": "glm"}},
                    {"sha": "bbb222", "subject": "fix: CI", "trailers": {"Agent": "llm-b0", "Claude-Session": "https://claude.ai/code/session_01X"}}
                ]
            },
            "48": {
                "branch": "forge/ledger-join",
                "first_push_sha": "ccc333",
                "first_push_ci": "green",
                "commits": [
                    {"sha": "ccc333", "subject": "feat: clean landing", "trailers": {"Agent": "llm-b0"}}
                ]
            }
        }
    }"#;

    fn history() -> CiHistoryFile {
        serde_json::from_str(FIXTURE_JSON).unwrap()
    }

    #[test]
    fn parses_the_frozen_fixture() {
        let h = history();
        assert_eq!(h.prs.len(), 2);
        assert_eq!(h.prs["50"].first_push_ci, CiStatus::Red);
        assert_eq!(h.prs["48"].commits.len(), 1);
    }

    #[test]
    fn a_haiku_row_flags_rework_when_a_sonnet_commit_follows() {
        let h = history();
        let result = required_rework(&h.prs["50"].commits, "haiku");
        assert_eq!(result, Some(true));
    }

    #[test]
    fn a_single_commit_pr_never_needed_rework() {
        let h = history();
        let result = required_rework(&h.prs["48"].commits, "sonnet");
        assert_eq!(result, Some(false));
    }

    #[test]
    fn a_row_at_the_same_or_higher_tier_than_the_follow_up_is_not_flagged() {
        let h = history();
        // The follow-up commit is `llm-b0` (sonnet); a row already at
        // opus/fable ranks above it, so no escalation happened.
        assert_eq!(required_rework(&h.prs["50"].commits, "opus"), Some(false));
    }

    #[test]
    fn ci_red_on_first_push_reads_the_pr_verbatim() {
        let h = history();
        assert_eq!(h.prs["50"].first_push_ci, CiStatus::Red);
        assert_eq!(h.prs["48"].first_push_ci, CiStatus::Green);
    }

    #[test]
    fn parse_owner_repo_handles_both_url_forms() {
        assert_eq!(
            parse_owner_repo("git@github.com:mcpirate17/llm-forge.git"),
            Some("mcpirate17_llm-forge".to_string())
        );
        assert_eq!(
            parse_owner_repo("https://github.com/mcpirate17/llm-forge"),
            Some("mcpirate17_llm-forge".to_string())
        );
        assert_eq!(parse_owner_repo("https://example.com/x/y"), None);
    }

    #[test]
    fn missing_cache_file_leaves_every_field_null_not_false() {
        let dir =
            std::env::temp_dir().join(format!("forge-outcome-test-missing-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut rows: Vec<(String, TaskDispatchRow)> =
            vec![("2026-09-13".to_string(), blank_row(Some("s1".to_string())))];
        apply(&mut rows, &[], &dir).unwrap();
        assert_eq!(rows[0].1.landed, None);
        assert_eq!(rows[0].1.required_rework, None);
        assert_eq!(rows[0].1.ci_red_on_first_push, None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn blank_row(parent_session_id: Option<String>) -> TaskDispatchRow {
        TaskDispatchRow {
            parent_session_id,
            dispatch_ts: None,
            tool_use_id: "t1".to_string(),
            agent_id: None,
            subagent_type: None,
            description: None,
            model_requested: None,
            model_used: None,
            tier: Some("haiku".to_string()),
            n_turns: None,
            billed_tokens: None,
            total_cache_read: None,
            over_cap: None,
            first_ts: None,
            last_ts: None,
            landed: None,
            required_rework: None,
            ci_red_on_first_push: None,
            decision: None,
            mode: None,
            applied: None,
        }
    }
}
