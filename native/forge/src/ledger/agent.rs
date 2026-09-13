//! `agent_rollup`: design step 4 (`docs/design/cost_ledger.md` sections 2,
//! 4 metric 3, 6 step 4). Joins `session_rollup` rows to `landed.rs`'s
//! commits through the `Agent:`/`Claude-Session:` trailers, so cost can be
//! divided by *landed* work rather than by session count alone.
//!
//! Join is commit-first ("does this landed PR have a session"), not
//! session-first, matching the design's own phrasing ("joins session_rollup
//! to landed PRs"): each landed commit already names its agent(s) via the
//! `Agent:` trailer, so the open question per commit is *which session did
//! the work*, not which agent gets credit.
//!
//! Cross-project safety: both the primary and fallback join only ever
//! consider sessions whose `project` equals the one `--project` this scan
//! was run for (`rollup.rs` passes the `--repo` argument's own basename, the
//! same convention `project_of()` already uses for transcript paths). A
//! session from another project is invisible to this join, full stop --
//! never filtered out after the fact, never scored and discarded.

use std::collections::BTreeSet;

use serde::Serialize;

use super::landed::LandedCommitRow;
use super::rollup::SessionRollupRow;

/// Tokens actually billed on the next turn: input + output +
/// cache_creation. Excludes cache_read (paid once, at a steep discount, not
/// "spent" the way a fresh token is) -- named explicitly on the row so a
/// reader never has to guess which of the several token totals this design
/// tracks (section 1.3) a number here rests on.
const TOKEN_BASIS: &str = "billed_noncache";

/// Default `--cap` (design step 4 brief, "150K agent caps" per
/// `llm-forge-direction-rust-and-agent-efficiency`).
pub const DEFAULT_CAP: u64 = 150_000;

#[derive(Debug, Clone, Serialize)]
pub struct AgentRollupRow {
    pub agent_name: String,
    pub tier: String,
    pub n_sessions: u64,
    pub n_landed_prs: u64,
    pub total_tokens: u64,
    pub total_cache_read: u64,
    pub token_basis: String,
    pub tokens_per_landed_pr: Option<f64>,
    pub cap_breaches: u64,
    /// The strongest join evidence behind any session credited to this
    /// agent: `"session_url"` if at least one of its sessions matched by
    /// id, else `"time_window"` if at least one matched only by the
    /// fallback, else `"unjoined"` when this agent has landed PRs but no
    /// session could be attributed to any of them at all.
    pub join_method: String,
}

/// One commit's join outcome, kept around only for the item-6 "count of
/// unjoined landed commits" report and for tests -- `build_agent_rollup`
/// folds this into `AgentRollupRow` and does not expose it as a table.
#[derive(Debug, Clone)]
pub struct CommitJoin {
    pub sha: String,
    pub pr_number: Option<u64>,
    pub session_ids: Vec<String>,
    pub join_method: &'static str,
    pub ambiguous: bool,
}

fn parse_utc_seconds(ts: &str) -> Option<i64> {
    let ts = ts.strip_suffix('Z')?;
    let (date, time) = ts.split_once('T')?;
    let mut date_parts = date.splitn(3, '-');
    let y: i64 = date_parts.next()?.parse().ok()?;
    let m: u32 = date_parts.next()?.parse().ok()?;
    let d: u32 = date_parts.next()?.parse().ok()?;
    let time = time.split('.').next()?; // drop fractional seconds if present
    let mut time_parts = time.splitn(3, ':');
    let hh: i64 = time_parts.next()?.parse().ok()?;
    let mm: i64 = time_parts.next()?.parse().ok()?;
    let ss: i64 = time_parts.next()?.parse().ok()?;
    Some(days_from_civil(y, m, d) * 86_400 + hh * 3600 + mm * 60 + ss)
}

/// Inverse of `rollup.rs::civil_from_days` (same Howard Hinnant
/// civil-calendar algorithm, duplicated for the same reason: this module
/// tree is compiled standalone via `#[path]` in the integration tests).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64;
    let mp = if m > 2 { m - 3 } else { m + 9 } as u64;
    let doy = (153 * mp + 2) / 5 + d as u64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe as i64 - 719_468
}

/// True when `[a_start, a_end]` and `[b_start, b_end]` overlap at all
/// (touching endpoints count, matching the design's closed interval
/// `[merged_at-6h, merged_at]`).
fn intervals_overlap(a_start: i64, a_end: i64, b_start: i64, b_end: i64) -> bool {
    a_start <= b_end && b_start <= a_end
}

fn billed_noncache(session: &SessionRollupRow) -> u64 {
    session.total_input + session.total_output + session.total_cache_creation
}

fn tier_of_model(model: &str) -> Option<&'static str> {
    let m = model.to_ascii_lowercase();
    // Order matters: check the more specific harness names before the
    // generic vendor-tier substrings they could otherwise collide with.
    if m.contains("fable") || m.contains("mythos") {
        Some("fable")
    } else if m.contains("opus") {
        Some("opus")
    } else if m.contains("sonnet") {
        Some("sonnet")
    } else if m.contains("haiku") {
        Some("haiku")
    } else if m.contains("glm") {
        Some("glm")
    } else {
        None
    }
}

/// `docs/ledger.md`'s tier table: one tier if every session's model maps to
/// the same one, `"mixed"` if more than one distinct tier is present,
/// `"unknown"` if no session's model matched any known substring at all.
fn infer_tier(models: &BTreeSet<&str>) -> String {
    let tiers: BTreeSet<&'static str> = models.iter().filter_map(|m| tier_of_model(m)).collect();
    match tiers.len() {
        0 => "unknown".to_string(),
        1 => tiers.into_iter().next().unwrap().to_string(),
        _ => "mixed".to_string(),
    }
}

/// Joins `commits` (one project's `landed.rs` output) to `sessions` (that
/// same project's `session_rollup` rows), returning one `CommitJoin` per
/// commit in `commits`'s order.
pub fn join_commits<'a>(
    commits: &'a [LandedCommitRow],
    sessions: &'a [SessionRollupRow],
    project: &str,
) -> Vec<CommitJoin> {
    let in_project: Vec<&SessionRollupRow> =
        sessions.iter().filter(|s| s.project == project).collect();

    // Fallback-eligible: same project AND the session itself carries no
    // `harness_session_ids`. A session that ever saw its own claude.ai URL
    // (e.g. because one of its own commits used a `Claude-Session:`
    // trailer) would have joined by `session_url` on that work already; a
    // *different*, URL-less commit falling back to it via time_window is
    // exactly the long-lived-coordinator-session over-attribution found in
    // the real-data check (`docs/ledger.md`), so such sessions are excluded
    // from the fallback pool entirely, not merely scored lower.
    let fallback_eligible: Vec<&SessionRollupRow> = in_project
        .iter()
        .copied()
        .filter(|s| s.harness_session_ids.is_empty())
        .collect();

    commits
        .iter()
        .map(|commit| {
            let commit_ids: BTreeSet<&str> = commit
                .harness_session_ids
                .iter()
                .map(String::as_str)
                .collect();
            let primary: Vec<&&SessionRollupRow> = in_project
                .iter()
                .filter(|s| {
                    s.harness_session_ids
                        .iter()
                        .any(|id| commit_ids.contains(id.as_str()))
                })
                .collect();
            if !primary.is_empty() {
                return CommitJoin {
                    sha: commit.sha.clone(),
                    pr_number: commit.pr_number,
                    session_ids: primary.iter().map(|s| s.session_id.clone()).collect(),
                    join_method: "session_url",
                    ambiguous: false,
                };
            }

            let mut fallback: Vec<&SessionRollupRow> = match parse_utc_seconds(&commit.merged_at) {
                Some(merged_at) => {
                    let window_start = merged_at - 6 * 3600;
                    fallback_eligible
                        .iter()
                        .copied()
                        .filter(|s| {
                            let (Some(first), Some(last)) =
                                (s.first_ts.as_deref(), s.last_ts.as_deref())
                            else {
                                return false;
                            };
                            let (Some(start), Some(end)) =
                                (parse_utc_seconds(first), parse_utc_seconds(last))
                            else {
                                return false;
                            };
                            intervals_overlap(start, end, window_start, merged_at)
                        })
                        .collect()
                }
                None => Vec::new(),
            };

            // A `glm`-named agent's commit only ever falls back to a
            // session whose own `models` say `glm`: crediting it to
            // whichever unrelated session merely happened to overlap in
            // time (the Fable-coordinator over-attribution this fix
            // closes) is worse than reporting the commit unjoined. No
            // matching session -> `unjoined`, on purpose, per commit.
            if commit
                .agent_names
                .iter()
                .any(|a| a.to_ascii_lowercase().starts_with("glm"))
            {
                fallback.retain(|s| {
                    s.models
                        .iter()
                        .any(|m| m.to_ascii_lowercase().contains("glm"))
                });
            }

            CommitJoin {
                sha: commit.sha.clone(),
                pr_number: commit.pr_number,
                session_ids: fallback.iter().map(|s| s.session_id.clone()).collect(),
                join_method: if fallback.is_empty() {
                    "unjoined"
                } else {
                    "time_window"
                },
                ambiguous: fallback.len() > 1,
            }
        })
        .collect()
}

/// Rolls `commits` + `sessions` up into one `AgentRollupRow` per agent named
/// on at least one landed commit. `cap` is the per-session `--cap` above
/// which a session counts as a breach (design step 4, `--cap`, default
/// `DEFAULT_CAP`).
///
/// `cap_breaches` counts every session credited to the agent whose own
/// `billed_noncache` total exceeds `cap` -- `SessionRollupRow` carries no
/// "this was a subagent transcript" flag (`rollup.rs`'s own note: an
/// `agent-*.jsonl` file "counts as its own session, no special-cased
/// handling"), so this is every session, not literally only ones from an
/// `agent-*.jsonl` file. That is the intended meaning here: any single
/// session run under this agent's name that alone burned past the cap.
pub fn build_agent_rollup(
    commits: &[LandedCommitRow],
    sessions: &[SessionRollupRow],
    project: &str,
    cap: u64,
) -> (Vec<AgentRollupRow>, Vec<CommitJoin>) {
    let joins = join_commits(commits, sessions, project);
    let sessions_by_id: std::collections::BTreeMap<&str, &SessionRollupRow> = sessions
        .iter()
        .map(|s| (s.session_id.as_str(), s))
        .collect();

    #[derive(Default)]
    struct Agg<'a> {
        landed_keys: BTreeSet<String>,
        session_ids: BTreeSet<&'a str>,
        methods: BTreeSet<&'static str>,
    }

    let mut by_agent: std::collections::BTreeMap<&str, Agg> = std::collections::BTreeMap::new();
    for (commit, join) in commits.iter().zip(joins.iter()) {
        // Dedup key for "one landed PR": the PR number when the subject
        // named one, else the commit sha itself (a direct-push commit is
        // still one landed change, just not one that came through a PR).
        let landed_key = commit
            .pr_number
            .map(|n| n.to_string())
            .unwrap_or_else(|| commit.sha.clone());
        for agent in &commit.agent_names {
            let agg = by_agent.entry(agent.as_str()).or_default();
            agg.landed_keys.insert(landed_key.clone());
            agg.methods.insert(join.join_method);
            for sid in &join.session_ids {
                if let Some((&key, _)) = sessions_by_id.get_key_value(sid.as_str()) {
                    agg.session_ids.insert(key);
                }
            }
        }
    }

    let mut rows: Vec<AgentRollupRow> = by_agent
        .into_iter()
        .map(|(agent_name, agg)| {
            let joined_sessions: Vec<&SessionRollupRow> = agg
                .session_ids
                .iter()
                .filter_map(|id| sessions_by_id.get(id).copied())
                .collect();
            let total_tokens: u64 = joined_sessions.iter().map(|s| billed_noncache(s)).sum();
            let total_cache_read: u64 = joined_sessions.iter().map(|s| s.total_cache_read).sum();
            let cap_breaches = joined_sessions
                .iter()
                .filter(|s| billed_noncache(s) > cap)
                .count() as u64;
            let models: BTreeSet<&str> = joined_sessions
                .iter()
                .flat_map(|s| s.models.iter().map(String::as_str))
                .collect();
            let n_landed_prs = agg.landed_keys.len() as u64;

            // Strongest evidence wins: session_url > time_window > unjoined
            // (comment on `AgentRollupRow::join_method`).
            let join_method = if agg.methods.contains("session_url") {
                "session_url"
            } else if agg.methods.contains("time_window") {
                "time_window"
            } else {
                "unjoined"
            };

            AgentRollupRow {
                agent_name: agent_name.to_string(),
                tier: infer_tier(&models),
                n_sessions: joined_sessions.len() as u64,
                n_landed_prs,
                total_tokens,
                total_cache_read,
                token_basis: TOKEN_BASIS.to_string(),
                tokens_per_landed_pr: if n_landed_prs == 0 {
                    None
                } else {
                    Some(total_tokens as f64 / n_landed_prs as f64)
                },
                cap_breaches,
                join_method: join_method.to_string(),
            }
        })
        .collect();
    rows.sort_by(|a, b| a.agent_name.cmp(&b.agent_name));
    (rows, joins)
}

/// Item 6's "count of unjoined landed commits": a finding to report, never
/// something to tune away by loosening the join.
pub fn unjoined_commit_count(joins: &[CommitJoin]) -> usize {
    joins.iter().filter(|j| j.join_method == "unjoined").count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[allow(clippy::too_many_arguments)]
    fn session(
        id: &str,
        project: &str,
        ids: &[&str],
        first_ts: &str,
        last_ts: &str,
        input: u64,
        output: u64,
        cache_creation: u64,
        cache_read: u64,
        models: &[&str],
    ) -> SessionRollupRow {
        SessionRollupRow {
            session_id: id.to_string(),
            project: project.to_string(),
            first_ts: Some(first_ts.to_string()),
            last_ts: Some(last_ts.to_string()),
            n_turns: 1,
            n_compactions: 0,
            total_input: input,
            total_output: output,
            total_cache_read: cache_read,
            total_cache_creation: cache_creation,
            resend_bytes: 0,
            resend_events: 0,
            harness_session_ids: ids.iter().map(|s| s.to_string()).collect(),
            models: models.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn commit(
        sha: &str,
        merged_at: &str,
        pr_number: Option<u64>,
        agents: &[&str],
        ids: &[&str],
    ) -> LandedCommitRow {
        LandedCommitRow {
            sha: sha.to_string(),
            merged_at: merged_at.to_string(),
            pr_number,
            agent_names: agents.iter().map(|s| s.to_string()).collect(),
            harness_session_ids: ids.iter().map(|s| s.to_string()).collect(),
            files_changed: 1,
            insertions: 1,
            deletions: 0,
        }
    }

    #[test]
    fn primary_join_by_session_url_wins_over_time_window() {
        let sessions = vec![session(
            "sess-1",
            "llm-forge",
            &["session_abc123"],
            "2026-09-01T00:00:00Z",
            "2026-09-01T01:00:00Z",
            1000,
            200,
            0,
            0,
            &["claude-sonnet-5"],
        )];
        let commits = vec![commit(
            "c1",
            "2026-09-01T02:00:00Z",
            Some(10),
            &["llm-b0"],
            &["session_abc123"],
        )];
        let (rows, joins) = build_agent_rollup(&commits, &sessions, "llm-forge", DEFAULT_CAP);
        assert_eq!(joins[0].join_method, "session_url");
        assert!(!joins[0].ambiguous);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].agent_name, "llm-b0");
        assert_eq!(rows[0].n_sessions, 1);
        assert_eq!(rows[0].n_landed_prs, 1);
        assert_eq!(rows[0].total_tokens, 1200);
        assert_eq!(rows[0].tokens_per_landed_pr, Some(1200.0));
        assert_eq!(rows[0].join_method, "session_url");
        assert_eq!(rows[0].tier, "sonnet");
        assert_eq!(rows[0].cap_breaches, 0);
        assert_eq!(unjoined_commit_count(&joins), 0);
    }

    #[test]
    fn time_window_fallback_flags_ambiguous_on_more_than_one_match() {
        let sessions = vec![
            session(
                "sess-a",
                "llm-forge",
                &[],
                "2026-09-01T20:00:00Z",
                "2026-09-01T21:00:00Z",
                80_000,
                10_000,
                0,
                0,
                &["glm-4.6"],
            ),
            session(
                "sess-b",
                "llm-forge",
                &[],
                "2026-09-01T22:00:00Z",
                "2026-09-01T23:30:00Z",
                90_000,
                5_000,
                0,
                0,
                &["glm-4.6"],
            ),
        ];
        // merged_at 2026-09-02T00:00:00Z, window [2026-09-01T18:00, 2026-09-02T00:00]
        // -- both sessions fall inside it.
        let commits = vec![commit(
            "c2",
            "2026-09-02T00:00:00Z",
            Some(11),
            &["glm"],
            &[],
        )];
        let (rows, joins) = build_agent_rollup(&commits, &sessions, "llm-forge", DEFAULT_CAP);
        assert_eq!(joins[0].join_method, "time_window");
        assert!(joins[0].ambiguous);
        assert_eq!(rows[0].n_sessions, 2);
        assert_eq!(rows[0].join_method, "time_window");
        assert_eq!(rows[0].tier, "glm");
        // 80_000+10_000=90_000, 90_000+5_000=95_000
        assert_eq!(rows[0].total_tokens, 185_000);
        assert_eq!(unjoined_commit_count(&joins), 0);
    }

    #[test]
    fn a_session_with_its_own_harness_session_ids_is_ineligible_for_time_window() {
        // Regression for the Fable-coordinator over-attribution found in
        // the real-data check: a session that knows its own claude.ai URL
        // would have joined by session_url on its own commits, so it must
        // never also catch an unrelated, URL-less commit via time_window
        // even when the windows overlap.
        let sessions = vec![session(
            "sess-fable",
            "llm-forge",
            &["session_01PoLjRxqVQGqy41fMDG26vX"],
            "2026-09-11T23:09:00Z",
            "2026-09-13T08:31:00Z",
            1_000_000,
            200_000,
            0,
            0,
            &["claude-fable-5.1"],
        )];
        let commits = vec![commit(
            "c7",
            "2026-09-13T07:15:23Z",
            Some(32),
            &["glm"],
            &[], // no Claude-Session trailer on this commit
        )];
        let (rows, joins) = build_agent_rollup(&commits, &sessions, "llm-forge", DEFAULT_CAP);
        assert_eq!(joins[0].join_method, "unjoined");
        assert_eq!(unjoined_commit_count(&joins), 1);
        assert_eq!(rows[0].n_sessions, 0);
        assert_eq!(rows[0].total_tokens, 0);
        assert_eq!(rows[0].join_method, "unjoined");
    }

    #[test]
    fn glm_agent_fallback_requires_a_glm_session_or_is_unjoined() {
        // A time-window candidate exists and overlaps, but its models say
        // sonnet, not glm -- a `glm`-named commit must not be credited to
        // another agent's session just because the window overlapped.
        let sessions = vec![session(
            "sess-sonnet",
            "llm-forge",
            &[],
            "2026-09-01T20:00:00Z",
            "2026-09-01T21:00:00Z",
            80_000,
            10_000,
            0,
            0,
            &["claude-sonnet-5"],
        )];
        let commits = vec![commit(
            "c8",
            "2026-09-02T00:00:00Z",
            Some(40),
            &["glm-4.6"],
            &[],
        )];
        let (rows, joins) = build_agent_rollup(&commits, &sessions, "llm-forge", DEFAULT_CAP);
        assert_eq!(joins[0].join_method, "unjoined");
        assert_eq!(unjoined_commit_count(&joins), 1);
        assert_eq!(rows[0].n_sessions, 0);
        assert_eq!(rows[0].n_landed_prs, 1);
        assert_eq!(rows[0].total_tokens, 0);
        assert_eq!(rows[0].tokens_per_landed_pr, Some(0.0));
        assert_eq!(rows[0].join_method, "unjoined");
    }

    #[test]
    fn a_session_from_another_project_never_joins() {
        let sessions = vec![session(
            "sess-other",
            "some-other-project",
            &["session_zzz"],
            "2026-09-01T23:59:00Z",
            "2026-09-01T23:59:30Z",
            999_999,
            999_999,
            0,
            0,
            &["claude-opus-4"],
        )];
        let commits = vec![commit(
            "c3",
            "2026-09-02T00:00:00Z",
            Some(12),
            &["llm-b0"],
            &["session_zzz"],
        )];
        let (rows, joins) = build_agent_rollup(&commits, &sessions, "llm-forge", DEFAULT_CAP);
        assert_eq!(joins[0].join_method, "unjoined");
        assert_eq!(unjoined_commit_count(&joins), 1);
        assert_eq!(rows[0].n_sessions, 0);
        assert_eq!(rows[0].total_tokens, 0);
        assert_eq!(rows[0].tokens_per_landed_pr, Some(0.0));
        assert_eq!(rows[0].join_method, "unjoined");
    }

    #[test]
    fn cap_breach_counts_sessions_over_the_cap() {
        let sessions = vec![session(
            "sess-big",
            "llm-forge",
            &["session_big1"],
            "2026-09-01T00:00:00Z",
            "2026-09-01T05:00:00Z",
            140_000,
            20_000,
            0,
            0,
            &["claude-opus-4"],
        )];
        let commits = vec![commit(
            "c4",
            "2026-09-01T06:00:00Z",
            Some(13),
            &["llm-b0"],
            &["session_big1"],
        )];
        let (rows, _joins) = build_agent_rollup(&commits, &sessions, "llm-forge", DEFAULT_CAP);
        assert_eq!(rows[0].cap_breaches, 1);
        assert_eq!(rows[0].tier, "opus");
    }

    #[test]
    fn mixed_tiers_report_mixed() {
        let sessions = vec![
            session(
                "s1",
                "llm-forge",
                &["session_m1"],
                "2026-09-01T00:00:00Z",
                "2026-09-01T00:10:00Z",
                10,
                10,
                0,
                0,
                &["claude-sonnet-5"],
            ),
            session(
                "s2",
                "llm-forge",
                &["session_m2"],
                "2026-09-01T01:00:00Z",
                "2026-09-01T01:10:00Z",
                10,
                10,
                0,
                0,
                &["glm-4.6"],
            ),
        ];
        let commits = vec![
            commit(
                "c5",
                "2026-09-01T00:20:00Z",
                Some(14),
                &["llm-b0"],
                &["session_m1"],
            ),
            commit(
                "c6",
                "2026-09-01T01:20:00Z",
                Some(15),
                &["llm-b0"],
                &["session_m2"],
            ),
        ];
        let (rows, _joins) = build_agent_rollup(&commits, &sessions, "llm-forge", DEFAULT_CAP);
        assert_eq!(rows[0].tier, "mixed");
        assert_eq!(rows[0].n_landed_prs, 2);
    }
}
