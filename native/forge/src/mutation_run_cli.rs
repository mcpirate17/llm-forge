//! `forge mutation inspect` / `forge mutation run`: the native campaign runner's
//! command line. The run itself lives in `forge::mutation_run`; this module owns
//! what only the binary knows, which is its build stamp and the caller's identity.

use anyhow::{Context, Result};
use clap::Args;
use forge::json_canon::python_pretty;
use forge::mutation_campaign::{Campaign, Stop};
use forge::mutation_run::{self, BuildStamp, RunRequest};
use forge::mutation_score::{status, PASS, RATCHET_HELD};
use serde_json::{json, Value};
use std::path::PathBuf;

use crate::mutation_plan::{resolve_lane, resolve_repo_root};

const NOT_READY_EXIT: i32 = 3;
const REFUSED_EXIT: i32 = 4;

#[derive(Args)]
pub struct InspectArgs {
    /// Generated campaign manifest.
    pub campaign: PathBuf,

    /// Repository root; defaults to the nearest ancestor with a `.git` entry.
    #[arg(long)]
    pub repo_root: Option<PathBuf>,
}

#[derive(Args)]
pub struct RunArgs {
    /// Generated campaign manifest.
    pub campaign: PathBuf,

    /// Required: this mutates a disposable snapshot of the repository.
    #[arg(long)]
    pub allow_mutations: bool,

    /// Receipt path inside the repository; defaults to the receipt root.
    #[arg(long)]
    pub receipt: Option<PathBuf>,

    /// Claim owner for the dirty-file scope; defaults to this process's
    /// resolved governance identity.
    #[arg(long)]
    pub owner: Option<String>,

    /// Base the changed-file scope is measured from.
    #[arg(long, default_value = "origin/main")]
    pub base: String,

    /// Exact repository-relative source this run may touch; repeatable.
    #[arg(long = "only")]
    pub only: Vec<String>,

    /// Repository root; defaults to the nearest ancestor with a `.git` entry.
    #[arg(long)]
    pub repo_root: Option<PathBuf>,
}

fn stamp() -> BuildStamp {
    BuildStamp {
        forge_version: env!("CARGO_PKG_VERSION"),
        source_sha256: env!("FORGE_SOURCE_SHA256"),
        git_commit: env!("FORGE_GIT_FULL_REV"),
        source_matches_git: env!("FORGE_SOURCE_MATCHES_GIT") == "1",
    }
}

fn print(payload: Value) -> Result<()> {
    print!(
        "{}",
        python_pretty(payload).context("rendering the report")?
    );
    Ok(())
}

fn refused(message: String) -> Result<i32> {
    print(json!({"status": "REFUSED", "error": message}))?;
    Ok(REFUSED_EXIT)
}

/// `inspect`: 0 when the campaign could run now, 3 when it could not.
pub fn inspect(args: InspectArgs) -> Result<i32> {
    let repo_root = resolve_repo_root(args.repo_root)?;
    let report = Campaign::load(&args.campaign)
        .and_then(|campaign| mutation_run::inspect(&campaign, &repo_root));
    match report {
        Ok(report) => {
            let ready = report.get("status").and_then(Value::as_str) == Some("READY");
            print(Value::Object(report))?;
            Ok(if ready { 0 } else { NOT_READY_EXIT })
        }
        Err(message) => refused(message),
    }
}

/// `run`: 0 for PASS or RATCHET_HELD, 1 for any other verdict, 4 for a refusal.
pub fn run(args: RunArgs) -> Result<i32> {
    let repo_root = resolve_repo_root(args.repo_root)?;
    let owner = match resolve_lane(args.owner, &repo_root) {
        Ok(owner) => owner,
        Err(message) => return refused(message),
    };
    let request = RunRequest {
        manifest: args.campaign,
        repo_root,
        owner,
        base: args.base,
        only: args.only,
        allow_mutations: args.allow_mutations,
        receipt: args.receipt,
        stamp: stamp(),
    };
    match mutation_run::run_campaign(&request) {
        Ok(receipt) => {
            let verdict = status(&receipt).to_string();
            print(Value::Object(receipt))?;
            Ok(i32::from(verdict != PASS && verdict != RATCHET_HELD))
        }
        Err(Stop::Refused(message)) => refused(message),
        Err(Stop::Cancelled(signal)) => refused(format!("cancelled by signal {signal}")),
    }
}
