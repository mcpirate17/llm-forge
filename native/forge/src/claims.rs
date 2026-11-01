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
fn list(repo: &Path, compact: bool, filters: &[String], show_paths: bool) -> Result<()> {
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
    if !compact {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({"sha256":digest,
            "claims":selected.iter().map(|claim| lifecycle::stored_claim(claim)).collect::<Vec<_>>() }))?
        );
        return Ok(());
    }
    let now = instant::now();
    let mut active = Vec::new();
    for claim in selected {
        if claim.active(now)? {
            active.push(claim);
        }
    }
    println!("claims: {} active", active.len());
    for claim in active {
        println!(
            "{}  {}  due {} ends {}  {} paths  {}",
            claim.claim_id,
            claim.owner,
            claim.expected_at.as_deref().unwrap_or(&claim.expires_at),
            claim.expires_at,
            claim.paths.len(),
            claim.justification.replace('\n', " ")
        );
        if show_paths || !filters.is_empty() {
            println!("    {}", claim.paths.join(" "));
        }
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
