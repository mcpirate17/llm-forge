//! `forge ledger landed`: design step 4 (`docs/design/cost_ledger.md`
//! section 6 step 4). Scans `git log --first-parent <branch>` on a repo and
//! emits one JSONL row per landed commit, carrying its `Agent:` trailer
//! names and harness session ids for `agent_rollup`'s join -- shells out to
//! `git`, no libgit2, matching this crate's other native tools
//! (`bash_guard`, `crg_refresh`) that already shell out to `git` the same
//! way rather than link a git library. The branch defaults to whatever
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
use std::process::Command;
use std::sync::OnceLock;

use anyhow::{bail, Context, Result};
use clap::Args;
use regex::Regex;
use serde::Serialize;

use super::session_ids::find_session_ids;

/// Record separator placed before every commit's fields (`git log
/// --format`); chosen because it cannot occur in a commit subject/body.
const RECORD_SEP: char = '\u{1e}';
/// Field separator between sha/date/subject/body within one record.
const FIELD_SEP: char = '\u{1f}';
/// Marks the end of the formatted fields, right before whatever
/// `--shortstat` appends (a blank line then one stat line, or nothing for
/// a commit with no first-parent diff).
const STAT_SEP: char = '\u{2}';

#[derive(Args)]
pub struct LandedArgs {
    /// Path to the git repository to scan.
    #[arg(long)]
    pub repo: PathBuf,

    /// Only commits at or after this date (any form `git log --since`
    /// accepts, e.g. `2026-08-01`). Mutually exclusive with `--last`.
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
/// a fresh `git init` has none). Never an error: a repo whose integration
/// branch really is `main` and has no origin/HEAD gets `main`, which is
/// what it would have resolved to anyway.
fn default_branch(repo: &Path) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["symbolic-ref", "refs/remotes/origin/HEAD"])
        .output();
    match output {
        Ok(output)
            if output.status.success()
                && !String::from_utf8_lossy(&output.stdout).trim().is_empty() =>
        {
            String::from_utf8_lossy(&output.stdout).trim().to_string()
        }
        _ => "main".to_string(),
    }
}

/// Scans `repo`'s `git log --first-parent <branch>` into one
/// `LandedCommitRow` per commit, newest first (git log's own default order).
/// `branch: None` means the default resolution (`default_branch`).
pub fn scan(
    repo: &Path,
    since: Option<&str>,
    last: Option<u64>,
    branch: Option<&str>,
) -> Result<Vec<LandedCommitRow>> {
    let rev = branch.unwrap_or(&default_branch(repo)).to_string();
    let format = format!("{RECORD_SEP}%H{FIELD_SEP}%cd{FIELD_SEP}%s{FIELD_SEP}%b{STAT_SEP}");
    let mut cmd = Command::new("git");
    cmd.env("TZ", "UTC")
        .arg("-C")
        .arg(repo)
        .args(["log", "--first-parent", &rev, "--shortstat"])
        .arg("--date=format-local:%Y-%m-%dT%H:%M:%SZ")
        .arg(format!("--format={format}"));
    if let Some(since) = since {
        cmd.arg(format!("--since={since}"));
    }
    if let Some(last) = last {
        cmd.arg(format!("-n{last}"));
    }
    let output = cmd
        .output()
        .with_context(|| format!("running git log in {}", repo.display()))?;
    if !output.status.success() {
        bail!(
            "git log in {} exited {:?}: {}",
            repo.display(),
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let stdout = String::from_utf8(output.stdout).context("git log output is not valid UTF-8")?;
    Ok(parse_log(&stdout))
}

fn parse_log(stdout: &str) -> Vec<LandedCommitRow> {
    stdout
        .split(RECORD_SEP)
        .filter(|chunk| !chunk.trim().is_empty())
        .map(parse_commit_chunk)
        .collect()
}

fn parse_commit_chunk(chunk: &str) -> LandedCommitRow {
    let (fields, stat) = chunk.split_once(STAT_SEP).unwrap_or((chunk, ""));
    let mut parts = fields.splitn(4, FIELD_SEP);
    let sha = parts.next().unwrap_or("").to_string();
    let merged_at = parts.next().unwrap_or("").to_string();
    let subject = parts.next().unwrap_or("");
    let body = parts.next().unwrap_or("");

    let mut harness_session_ids = BTreeSet::new();
    find_session_ids(body, &mut harness_session_ids);
    let (files_changed, insertions, deletions) = parse_shortstat(stat);

    LandedCommitRow {
        sha,
        merged_at,
        pr_number: pr_number_from_subject(subject),
        agent_names: agent_names_from_body(body),
        harness_session_ids: harness_session_ids.into_iter().collect(),
        subject_digest: super::subject::subject_digest(subject),
        files_changed,
        insertions,
        deletions,
    }
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

fn shortstat_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(
            r"(\d+) files? changed(?:, (\d+) insertions?\(\+\))?(?:, (\d+) deletions?\(-\))?",
        )
        .expect("valid shortstat regex")
    })
}

/// Parses a `git --shortstat` block (" N files changed, M insertions(+),
/// K deletions(-)", any of the two counts optionally absent). All zero for
/// a commit `--shortstat` printed nothing for (no first-parent diff --
/// rare, but not an error: an empty merge is still a landed commit).
fn parse_shortstat(stat: &str) -> (u64, u64, u64) {
    let Some(caps) = shortstat_pattern().captures(stat) else {
        return (0, 0, 0);
    };
    let num = |i: usize| -> u64 {
        caps.get(i)
            .and_then(|m| m.as_str().parse().ok())
            .unwrap_or(0)
    };
    (num(1), num(2), num(3))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fixture built from three real commit messages copied from this
    /// repo's own `git log` (commit messages are public repo content --
    /// `brief_ledger_agent_rollup.md` item 2 says copying them is fine).
    /// Shapes: one Claude commit (sha, PR number, one `Agent:`, one
    /// `Claude-Session` URL), one GLM commit (`ab153e2`-shaped: several
    /// `Agent: glm` trailers, zero URLs), one commit with no `(#N)` suffix.
    const FIXTURE_LOG: &str = "\u{1e}d90ffdfd9a1b4db0d58794da574b20e7f66d6414\u{1f}2026-09-13T08:29:48Z\u{1f}feat(forge): ledger rollup writes turn, session and hook tables with compaction and resend detection (#37)\u{1f}Design step 2 of the cost ledger.\n\nAgent: llm-b0\n\nClaude-Session: https://claude.ai/code/session_01PoLjRxqVQGqy41fMDG26vX\n\nCo-authored-by: llm-b0 <llm-b0@users.noreply.github.com>\u{2}\n\n 21 files changed, 980 insertions(+), 31 deletions(-)\u{1e}ab153e246e234caaa4f1daa51c9fda7e83f51bdf\u{1f}2026-09-11T12:00:00Z\u{1f}feat(forge): workspace_hygiene reads the configured integration branch, runs native at SessionStart (#32)\u{1f}Squashed body.\n\nAgent: glm\n\nAgent: glm\n\nAgent: glm\u{2}\n\n 41 files changed, 64292 insertions(+), 132 deletions(-)\u{1e}0000000000000000000000000000000000000000\u{1f}2026-09-01T00:00:00Z\u{1f}chore: direct push, no PR\u{1f}No trailers at all.\u{2}\n\n 1 file changed, 1 insertion(+)\u{1e}";

    #[test]
    fn claude_commit_has_pr_number_agent_and_session_id() {
        let rows = parse_log(FIXTURE_LOG);
        let row = &rows[0];
        assert_eq!(row.sha, "d90ffdfd9a1b4db0d58794da574b20e7f66d6414");
        assert_eq!(row.merged_at, "2026-09-13T08:29:48Z");
        assert_eq!(row.pr_number, Some(37));
        assert_eq!(row.agent_names, vec!["llm-b0".to_string()]);
        assert_eq!(
            row.harness_session_ids,
            vec!["session_01PoLjRxqVQGqy41fMDG26vX".to_string()]
        );
        assert_eq!(row.files_changed, 21);
        assert_eq!(row.insertions, 980);
        assert_eq!(row.deletions, 31);
    }

    #[test]
    fn glm_commit_has_no_session_id_and_dedupes_agent_lines() {
        let rows = parse_log(FIXTURE_LOG);
        let row = &rows[1];
        assert_eq!(row.pr_number, Some(32));
        assert_eq!(row.agent_names, vec!["glm".to_string()]);
        assert!(row.harness_session_ids.is_empty());
        assert_eq!(row.files_changed, 41);
        assert_eq!(row.insertions, 64292);
        assert_eq!(row.deletions, 132);
    }

    #[test]
    fn commit_with_no_pr_suffix_and_no_trailers_is_null_not_an_error() {
        let rows = parse_log(FIXTURE_LOG);
        let row = &rows[2];
        assert_eq!(row.pr_number, None);
        assert!(row.agent_names.is_empty());
        assert!(row.harness_session_ids.is_empty());
        assert_eq!(row.files_changed, 1);
        assert_eq!(row.insertions, 1);
        assert_eq!(row.deletions, 0);
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

    #[test]
    fn the_subject_digest_strips_the_squash_suffix() {
        // The landed subject carries ` (#N)`; the session that typed it ran
        // `gh pr create --title "<title>"` with no suffix. The strip is
        // what makes the two digests meet.
        let rows = parse_log(FIXTURE_LOG);
        assert_eq!(
            rows[0].subject_digest,
            super::super::subject::subject_digest(
                "feat(forge): ledger rollup writes turn, session and hook tables with compaction and resend detection"
            )
        );
        assert_eq!(
            rows[1].subject_digest,
            super::super::subject::subject_digest(
                "feat(forge): workspace_hygiene reads the configured integration branch, runs native at SessionStart"
            )
        );
        // No suffix on this one: the digest hashes the subject verbatim.
        assert_eq!(
            rows[2].subject_digest,
            super::super::subject::subject_digest("chore: direct push, no PR")
        );
    }
}
