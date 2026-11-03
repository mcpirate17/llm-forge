//! Command-line access to native structured ownership claims.
use anyhow::{bail, ensure, Context, Result};
use clap::{Args, Subcommand};
use forge::{instant, ownership, ownership_lifecycle as lifecycle};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Args)]
pub struct ClaimArgs {
    #[command(subcommand)]
    action: Action,
}
#[derive(Subcommand)]
enum Action {
    Create {
        #[arg(long, default_value = ".")]
        repo: PathBuf,
        #[arg(long, default_value = "")]
        owner: String,
        #[arg(long)]
        justification: String,
        #[arg(long, default_value_t = 45.0)]
        expected_minutes: f64,
        #[arg(long, default_value_t = 60.0)]
        max_minutes: f64,
        #[arg(long)]
        hours: Option<f64>,
        #[arg(required = true)]
        paths: Vec<String>,
    },
    Release {
        claim_id: String,
        #[arg(long)]
        owner: String,
        #[arg(long, default_value = ".")]
        repo: PathBuf,
    },
    Touch {
        claim_id: String,
        #[arg(long)]
        owner: String,
        #[arg(long, default_value = ".")]
        repo: PathBuf,
    },
    List {
        #[arg(long, default_value = ".")]
        repo: PathBuf,
        #[arg(long)]
        compact: bool,
        #[arg(long)]
        path: Vec<String>,
        #[arg(long)]
        paths: bool,
    },
}
fn checkout(repo: &Path) -> Result<PathBuf> {
    let path = repo.canonicalize()?;
    let (root, _) =
        forge::repository::checkout_of_checked(&path)?.context("repository not found")?;
    Ok(root)
}
fn is_vendor(value: &str) -> bool {
    ["codex", "claude", "grok", "qwen"].contains(&value)
}
fn claim_owner(repo: &Path, owner: &str) -> Result<String> {
    let declared = crate::identity::normalize(owner);
    ensure!(
        declared.is_empty() || !is_vendor(&declared),
        "owner must name a lane, not a vendor"
    );
    let env: HashMap<String, String> = std::env::vars().collect();
    let resolved = crate::identity::resolve_owner(Some(repo), &env).unwrap_or_default();
    ensure!(
        declared.is_empty() || resolved.is_empty() || is_vendor(&resolved) || declared == resolved,
        "owner {declared:?} disagrees with this lane {resolved:?}"
    );
    let result = if declared.is_empty() {
        resolved
    } else {
        declared
    };
    ensure!(
        !result.is_empty() && !is_vendor(&result),
        "owner must name a lane, not a vendor"
    );
    Ok(result)
}
fn directories(paths: &[String]) -> String {
    let mut counts = std::collections::BTreeMap::<String, usize>::new();
    for path in paths {
        let parent = path
            .rsplit_once('/')
            .map(|(parent, _)| format!("{parent}/"))
            .unwrap_or_else(|| ".".into());
        *counts.entry(parent).or_default() += 1;
    }
    counts
        .into_iter()
        .map(|(path, count)| format!("{path}({count})"))
        .collect::<Vec<_>>()
        .join(" ")
}
fn compact(
    claims: &[&ownership::OwnershipClaim],
    digest: &str,
    now: f64,
    paths: bool,
) -> Result<String> {
    let mut active = Vec::new();
    for claim in claims {
        if claim.active(now)? {
            active.push(*claim);
        }
    }
    let mut overrun = 0;
    for claim in &active {
        overrun += usize::from(claim.overrun(now)?);
    }
    let mut lines = vec![format!(
        "claims: {} active ({} overrun), {} expired, sha256 {}",
        active.len(),
        overrun,
        claims.len() - active.len(),
        digest.get(..12).context("invalid claim digest")?
    )];
    for claim in active {
        let mut why = claim.justification.trim().replace('\n', " ");
        if why.chars().count() > 72 {
            why = why.chars().take(71).collect::<String>() + "…";
        }
        let idle = (now - claim.activity()?.unwrap_or(claim.creation()?)) / 60.0;
        let due = instant::format_ymd_hm(claim.expected()?);
        let ends = instant::format_ymd_hm(claim.deadline(now)?);
        let state = if claim.overrun(now)? {
            "OVERRUN"
        } else {
            "on-time"
        };
        lines.push(format!(
            "{}  {:<14} {:<7} due {}Z ends {}Z  idle {:>3.0}/{}m  {:>2} paths  {}  {}",
            claim.claim_id,
            claim.owner,
            state,
            &due[5..],
            &ends[5..],
            idle,
            claim.idle_window_seconds(now)? / 60.0,
            claim.paths.len(),
            directories(&claim.paths),
            why
        ));
        if paths {
            lines.push(format!("    {}", claim.paths.join(" ")));
        }
    }
    Ok(lines.join("\n"))
}
fn list(repo: &Path, is_compact: bool, filters: &[String], show_paths: bool) -> Result<()> {
    let (claims, digest) = lifecycle::list(repo)?;
    let selected: Vec<_> = claims
        .iter()
        .filter(|claim| {
            filters.is_empty()
                || filters.iter().any(|target| {
                    claim
                        .paths
                        .iter()
                        .any(|path| ownership::paths_overlap(target, path))
                })
        })
        .collect();
    if is_compact {
        println!(
            "{}",
            compact(
                &selected,
                &digest,
                instant::now(),
                show_paths || !filters.is_empty()
            )?
        );
    } else {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({"sha256":digest,
            "claims":selected.iter().map(|claim| lifecycle::reported_claim(claim)).collect::<Vec<_>>() }))?
        );
    }
    Ok(())
}

pub fn run(args: ClaimArgs) -> Result<()> {
    match args.action {
        Action::Create {
            repo,
            owner,
            justification,
            expected_minutes,
            max_minutes,
            hours,
            paths,
        } => {
            let repo = checkout(&repo)?;
            let owner = claim_owner(&repo, &owner)?;
            let claim = lifecycle::create(
                &repo,
                lifecycle::Create {
                    owner: &owner,
                    paths: &paths,
                    justification: &justification,
                    expected_minutes,
                    max_minutes: hours.map_or(max_minutes, |h| h * 60.0),
                },
            )?;
            println!(
                "created {} owner={} expected={} max={} paths={}",
                claim.claim_id,
                claim.owner,
                claim.expected_at.as_deref().unwrap_or(""),
                claim.expires_at,
                claim.paths.join(",")
            );
        }
        Action::Release {
            repo,
            claim_id,
            owner,
        } => {
            if !lifecycle::release(&checkout(&repo)?, &claim_id, &owner)? {
                bail!("ownership claim does not exist: {claim_id}");
            }
            println!("released {claim_id}");
        }
        Action::Touch {
            repo,
            claim_id,
            owner,
        } => {
            lifecycle::touch(&checkout(&repo)?, &claim_id, &owner)?;
            println!("touched {claim_id}; idle timer reset, max time unchanged");
        }
        Action::List {
            repo,
            compact,
            path,
            paths,
        } => list(&checkout(&repo)?, compact, &path, paths)?,
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claim(
        id: &str,
        created: &str,
        expires: &str,
        expected: Option<&str>,
    ) -> ownership::OwnershipClaim {
        ownership::OwnershipClaim {
            claim_id: format!("claim-{id}"),
            owner: id.into(),
            paths: vec!["p.py".into()],
            justification: "j".into(),
            created_at: created.into(),
            expires_at: expires.into(),
            expected_at: expected.map(str::to_owned),
            last_seen: None,
        }
    }

    #[test]
    fn compact_view_counts_expired_and_uses_idle_deadline() {
        let live = claim(
            "live",
            "2026-08-27T12:00:00+00:00",
            "2026-08-27T14:00:00+00:00",
            None,
        );
        let dead = claim(
            "dead",
            "2026-08-27T09:00:00+00:00",
            "2026-08-27T10:00:00+00:00",
            None,
        );
        let now = instant::parse("2026-08-27T12:00:00+00:00").unwrap();
        let text = compact(&[&live, &dead], "abcdef0123456789", now, false).unwrap();
        assert_eq!(
            text.lines().next().unwrap(),
            "claims: 1 active (0 overrun), 1 expired, sha256 abcdef012345"
        );
        assert!(text.contains("claim-live") && !text.contains("claim-dead"));
        assert!(text.contains("due 08-27 14:00Z ends 08-27 12:45Z"));
        assert!(text.contains("idle   0/45m"));
    }

    #[test]
    fn compact_view_marks_overrun_and_truncates_justification() {
        let mut claim = claim(
            "lane",
            "2026-08-27T11:50:00+00:00",
            "2026-08-27T14:00:00+00:00",
            Some("2026-08-27T11:59:00+00:00"),
        );
        claim.last_seen = Some("2026-08-27T11:58:00+00:00".into());
        claim.justification = "x".repeat(100);
        let now = instant::parse("2026-08-27T12:00:00+00:00").unwrap();
        let text = compact(&[&claim], "abcdef0123456789", now, true).unwrap();
        assert!(text.contains("1 active (1 overrun), 0 expired"));
        assert!(text.contains("OVERRUN"));
        assert!(text.contains("idle   2/10m"));
        assert!(text.contains(&("x".repeat(71) + "…")));
        assert!(text.lines().any(|line| line == "    p.py"));
    }
}
