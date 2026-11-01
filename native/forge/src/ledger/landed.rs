//! `forge ledger landed`: design step 4 (`docs/design/cost_ledger.md`
//! section 6 step 4). Scans `git log --first-parent <branch>` on a repo and
//! emits one JSONL row per landed commit, carrying its `Agent:` trailer
//! names and harness session ids for `agent_rollup`'s join using libgit2. The branch defaults to whatever
//! `refs/remotes/origin/HEAD` points at, else `main` (the LLM monorepo
//! integrates on `master`, which is why the default is resolved, not
//! hardcoded).
//!
//! Join keys measured against this repo's real history (design step 4
//! brief, 2026-09-13): every squash commit on `main` carries one or more
//! `Agent: <name>` trailers; a Claude-authored one also carries a
//! `Claude-Session: https://claude.ai/code/session_<id>` trailer, a
//! GLM-authored one carries `Agent:` lines but no session URL.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{bail, Context, Result};
use clap::Args;
use regex::Regex;
use serde::Serialize;

use super::session_ids::find_session_ids;

#[derive(Args)]
pub struct LandedArgs {
    /// Path to the git repository to scan.
    #[arg(long)]
    pub repo: PathBuf,

    /// ISO date (using current UTC time of day), RFC3339 timestamp, or a
    /// relative interval such as `7 days ago`.
    /// Mutually exclusive with `--last`.
    #[arg(long)]
    pub since: Option<String>,

    /// Only the most recent N commits on the first-parent history of the
    /// integration branch. Mutually exclusive with `--since`.
    #[arg(long)]
    pub last: Option<u64>,

    /// Integration branch to scan (`git log --first-parent <branch>`).
    /// Default: the ref `refs/remotes/origin/HEAD` points at, falling back
    /// to `main` when a repo has no origin/HEAD at all (the LLM monorepo
    /// integrates on `master`, and hardcoding `main` there fails with
    /// "ambiguous argument 'main'").
    #[arg(long)]
    pub branch: Option<String>,
}

/// One landed commit on `main`'s first-parent history (design section 2,
/// step 4). Field order matches the design's name order.
#[derive(Debug, Clone, Serialize)]
pub struct LandedCommitRow {
    pub sha: String,
    /// Committer date, UTC, `YYYY-MM-DDTHH:MM:SSZ`.
    pub merged_at: String,
    /// From the subject's trailing `(#N)`; `None` when the subject has no
    /// such suffix (a commit landed some other way than a numbered PR).
    pub pr_number: Option<u64>,
    /// Every `Agent: <name>` trailer value in the body, sorted and
    /// deduplicated (a squash commit can carry the same name more than
    /// once, one per squashed sub-commit).
    pub agent_names: Vec<String>,
    pub harness_session_ids: Vec<String>,
    /// The subject's digest under the SAME normalization and hash the
    /// reader side uses (`subject.rs`): the squash suffix `(#N)` is
    /// stripped before hashing, so a `gh pr create --title` digest meets
    /// its landed subject here. Empty string when the normalized subject is
    /// shorter than `subject::MIN_SUBJECT_CHARS` -- refused on both sides,
    /// never matched.
    pub subject_digest: String,
    pub files_changed: u64,
    pub insertions: u64,
    pub deletions: u64,
}

pub fn run(args: LandedArgs) -> Result<i32> {
    if args.since.is_some() && args.last.is_some() {
        bail!("forge ledger landed: --since and --last are mutually exclusive");
    }
    let rows = scan(
        &args.repo,
        args.since.as_deref(),
        args.last,
        args.branch.as_deref(),
    )?;
    for row in &rows {
        println!("{}", serde_json::to_string(row)?);
    }
    Ok(0)
}

/// The ref `refs/remotes/origin/HEAD` points at (e.g.
/// `refs/remotes/origin/master`), or `main` when the repo has no origin/HEAD
/// (git prints that ref only when a remote has been fetched or configured;
/// a fresh `git init` has none). An absent ref selects `main`; malformed or
/// unreadable references remain errors.
fn default_branch(repo: &git2::Repository) -> Result<String> {
    match repo.find_reference("refs/remotes/origin/HEAD") {
        Ok(reference) => Ok(reference
            .symbolic_target()?
            .filter(|name| !name.is_empty())
            .unwrap_or("main")
            .to_owned()),
        Err(error) if error.code() == git2::ErrorCode::NotFound => Ok("main".into()),
        Err(error) => Err(error.into()),
    }
}

fn since_cutoff(raw: &str) -> Result<i64> {
    let value = raw.trim();
    if value.len() == 10
        && value.as_bytes().get(4) == Some(&b'-')
        && value.as_bytes().get(7) == Some(&b'-')
    {
        let now = forge::instant::isoformat_utc(forge::instant::now());
        let date = format!("{value}{}", &now[10..19]);
        let parsed = forge::instant::parse(&format!("{date}+00:00"))
            .with_context(|| format!("invalid --since date {raw:?}"))?;
        if &forge::instant::isoformat_utc(parsed)[..10] != value {
            bail!("invalid --since date {raw:?}");
        }
        return Ok(parsed as i64);
    }
    if let Some(value) = forge::instant::parse(value) {
        return Ok(value as i64);
    }
    let parts: Vec<_> = value.split_whitespace().collect();
    if let [amount, unit, "ago"] = parts.as_slice() {
        let number: i64 = amount.parse().context("invalid --since interval")?;
        let seconds = match *unit {
            "second" | "seconds" => 1,
            "minute" | "minutes" => 60,
            "hour" | "hours" => 3600,
            "day" | "days" => 86400,
            "week" | "weeks" => 604800,
            _ => bail!("unsupported --since interval {raw:?}"),
        };
        let duration = number
            .checked_mul(seconds)
            .context("--since interval overflow")?;
        if number < 0 {
            bail!("--since interval must be nonnegative");
        }
        return (forge::instant::now() as i64)
            .checked_sub(duration)
            .context("--since interval overflow");
    }
    bail!("unsupported --since {raw:?}; use ISO date, RFC3339 timestamp, or an interval such as '7 days ago'")
}

/// Scan first-parent commits and compute native tree-diff statistics.
pub fn scan(
    repo: &Path,
    since: Option<&str>,
    last: Option<u64>,
    branch: Option<&str>,
) -> Result<Vec<LandedCommitRow>> {
    if since.is_some() && last.is_some() {
        bail!("--since and --last are mutually exclusive");
    }
    let repository = git2::Repository::discover(repo)
        .with_context(|| format!("opening repository {}", repo.display()))?;
    let default = default_branch(&repository)?;
    let revision = branch.unwrap_or(&default);
    let mut commit = repository.revparse_single(revision)?.peel_to_commit()?;
    let cutoff = since.map(since_cutoff).transpose()?;
    let mut rows = Vec::new();
    loop {
        if last.is_some_and(|max| rows.len() as u64 >= max) {
            break;
        }
        if cutoff.is_some_and(|cutoff| commit.time().seconds() < cutoff) {
            break;
        }
        rows.push(commit_row(&repository, &commit)?);
        if commit.parent_count() == 0 {
            break;
        }
        commit = commit.parent(0)?;
    }
    Ok(rows)
}

fn commit_row(repo: &git2::Repository, commit: &git2::Commit<'_>) -> Result<LandedCommitRow> {
    std::str::from_utf8(commit.message_bytes()).context("commit message is not UTF-8")?;
    let subject = commit.summary()?.unwrap_or("");
    let body = commit.body()?.unwrap_or("");
    let mut ids = BTreeSet::new();
    find_session_ids(body, &mut ids);
    let tree = commit.tree()?;
    let parent = if commit.parent_count() == 0 {
        None
    } else {
        Some(commit.parent(0)?.tree()?)
    };
    let mut diff = repo.diff_tree_to_tree(parent.as_ref(), Some(&tree), None)?;
    diff.find_similar(None)?;
    let stats = diff.stats()?;
    let stamp = forge::instant::isoformat_utc(commit.time().seconds() as f64);
    let merged_at = stamp.strip_suffix("+00:00").unwrap_or(&stamp).to_owned() + "Z";
    Ok(LandedCommitRow {
        sha: commit.id().to_string(),
        merged_at,
        pr_number: pr_number_from_subject(subject),
        agent_names: agent_names_from_body(body),
        harness_session_ids: ids.into_iter().collect(),
        subject_digest: super::subject::subject_digest(subject),
        files_changed: stats.files_changed() as u64,
        insertions: stats.insertions() as u64,
        deletions: stats.deletions() as u64,
    })
}

fn pr_number_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(r"\(#(\d+)\)\s*$").expect("valid pr number regex"))
}

/// The subject's trailing `(#N)` (a squash-merge's default suffix);
/// `None` for a subject with no such marker.
fn pr_number_from_subject(subject: &str) -> Option<u64> {
    pr_number_pattern()
        .captures(subject.trim_end())
        .and_then(|c| c.get(1))
        .and_then(|m| m.as_str().parse().ok())
}

fn agent_trailer_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    // Anchored per-line (`(?m)`) so a body with several `Agent:` trailers
    // (one squashed sub-commit each) yields one match per line; the value
    // is trimmed of trailing `\r` for a body that came from a CRLF source.
    PATTERN.get_or_init(|| {
        Regex::new(r"(?m)^Agent:[ \t]*(.+?)[ \t\r]*$").expect("valid agent trailer regex")
    })
}

/// Every `Agent: <name>` trailer line in `body`, sorted and deduplicated.
/// Deliberately does not touch `Co-Authored-By` or any other trailer --
/// only the exact `Agent:` key is this repo's ownership record
/// (CLAUDE.md's "Commits" rule).
fn agent_names_from_body(body: &str) -> Vec<String> {
    agent_trailer_pattern()
        .captures_iter(body)
        .filter_map(|c| c.get(1))
        .map(|m| m.as_str().to_string())
        .collect::<BTreeSet<String>>()
        .into_iter()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_scan_selects_origin_head_and_reads_first_parent_stats() {
        let dir = std::env::temp_dir().join(forge::resource_id("landed-native-test").unwrap());
        std::fs::create_dir(&dir).unwrap();
        let repo = git2::Repository::init(&dir).unwrap();
        let blob = repo.blob(b"one\ntwo\n").unwrap();
        let mut builder = repo.treebuilder(None).unwrap();
        builder.insert("file.txt", blob, 0o100644).unwrap();
        let tree = repo.find_tree(builder.write().unwrap()).unwrap();
        drop(builder);
        let signature = git2::Signature::new(
            "agent",
            "a@example.invalid",
            &git2::Time::new(1_700_000_000, 540),
        )
        .unwrap();
        let first = repo
            .commit(
                None,
                &signature,
                &signature,
                "first landed change\n",
                &tree,
                &[],
            )
            .unwrap();
        let first_commit = repo.find_commit(first).unwrap();
        let empty = repo.treebuilder(None).unwrap().write().unwrap();
        let empty_tree = repo.find_tree(empty).unwrap();
        let second = repo
            .commit(
                None,
                &signature,
                &signature,
                "second landed change (#8)\n\nAgent: lane-b\nAgent: lane-b\nClaude-Session: https://claude.ai/code/session_abc123\n",
                &empty_tree,
                &[&first_commit],
            )
            .unwrap();
        repo.reference("refs/remotes/origin/master", second, true, "fixture")
            .unwrap();
        repo.reference_symbolic(
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/master",
            true,
            "fixture",
        )
        .unwrap();
        let rows = scan(&dir, None, None, None).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].merged_at, "2023-11-14T22:13:20Z");
        assert_eq!(rows[0].pr_number, Some(8));
        assert_eq!(rows[0].agent_names, ["lane-b"]);
        assert_eq!(rows[0].harness_session_ids, ["session_abc123"]);
        assert_eq!(
            rows[0].subject_digest,
            super::super::subject::subject_digest("second landed change")
        );
        assert_eq!(rows[1].pr_number, None);
        assert!(rows[1].agent_names.is_empty() && rows[1].harness_session_ids.is_empty());
        assert_eq!((rows[0].files_changed, rows[0].deletions), (1, 2));
        assert_eq!((rows[1].files_changed, rows[1].insertions), (1, 2));
        assert_eq!(scan(&dir, None, Some(1), None).unwrap().len(), 1);
        assert!(scan(&dir, Some("2099-01-01T00:00:00Z"), None, None)
            .unwrap()
            .is_empty());
        assert!(scan(&dir, Some("not a date"), None, None).is_err());
        drop(empty_tree);
        drop(first_commit);
        drop(tree);
        drop(repo);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn trailing_slash_on_a_session_url_does_not_reach_the_match() {
        let mut ids = BTreeSet::new();
        find_session_ids(
            "Claude-Session: https://claude.ai/code/session_abc123/\n",
            &mut ids,
        );
        assert_eq!(
            ids.into_iter().collect::<Vec<_>>(),
            vec!["session_abc123".to_string()]
        );
    }
}
