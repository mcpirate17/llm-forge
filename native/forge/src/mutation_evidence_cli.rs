//! Read-only mutation reporting frontdoors; all validation lives in the library.
use anyhow::{Context, Result};
use clap::Args;
use forge::{
    candidate_engine_identity::BuildStamp,
    mutation_evidence_inventory::{self, ReportRequest, Selection},
};
use serde_json::{json, Value};
use std::{fs::OpenOptions, io::Write, path::PathBuf};

#[derive(Args)]
pub struct ReportArgs {
    /// Repository root; defaults to the nearest ancestor with a `.git` entry.
    #[arg(long)]
    repo_root: Option<PathBuf>,
    /// Registry inside the repository; defaults to its project configuration.
    #[arg(long)]
    registry: Option<String>,
    /// Observed legacy package parent containing conductor/ and tooling/.
    /// Omission leaves legacy runner identity unavailable; grants no execution authority.
    #[arg(long)]
    legacy_component_root: Option<PathBuf>,
    /// Operator-selected native identity pin, outside the audited repository.
    #[arg(long, requires = "native_runner_pin_sha256")]
    native_runner_pin: Option<PathBuf>,
    #[arg(long, requires = "native_runner_pin")]
    native_runner_pin_sha256: Option<String>,
}

#[derive(Args)]
#[command(group(clap::ArgGroup::new("selection").required(true).args(["paths", "changed"])))]
pub struct EvidenceArgs {
    #[command(flatten)]
    report: ReportArgs,
    /// Explicit repository-relative test paths.
    #[arg(conflicts_with = "changed")]
    paths: Vec<String>,
    /// Select changed tests instead of explicit paths; defaults to diff against HEAD.
    #[arg(long)]
    changed: bool,
    /// Compare with this ref using merge-base semantics.
    #[arg(long, requires = "changed")]
    base: Option<String>,
    /// Emit workflow annotations and append to GITHUB_STEP_SUMMARY when set.
    #[arg(long, requires = "changed")]
    github: bool,
}

pub fn evidence(args: EvidenceArgs, stamp: &BuildStamp) -> Result<u8> {
    let selection = if args.changed {
        Selection::Changed(args.base)
    } else {
        Selection::Explicit(args.paths)
    };
    run(args.report, selection, args.github, stamp)
}

pub fn coverage(args: ReportArgs, stamp: &BuildStamp) -> Result<u8> {
    run(args, Selection::Coverage, false, stamp)
}

pub fn canary(args: ReportArgs, stamp: &BuildStamp) -> Result<u8> {
    run(args, Selection::Canary, false, stamp)
}

fn run(args: ReportArgs, selection: Selection, github: bool, stamp: &BuildStamp) -> Result<u8> {
    let report = (|| {
        let request = ReportRequest {
            repo_root: crate::mutation_plan::resolve_repo_root(args.repo_root)?,
            registry: args.registry,
            selection,
            legacy_component_root: args.legacy_component_root,
            include_untracked: true,
            native_runner_pin: args.native_runner_pin.zip(args.native_runner_pin_sha256),
        };
        mutation_evidence_inventory::run_report(&request, Some(stamp))
    })();
    let (payload, code) = match report {
        Ok(report) => report,
        Err(error) => (json!({"status":"REFUSED", "error":format!("{error:#}")}), 4),
    };
    if github && code != 4 {
        github_output(&payload)?;
    }
    print!("{}", forge::json_canon::python_pretty(payload)?);
    Ok(code)
}

fn github_output(report: &Value) -> Result<()> {
    let output = conductor_native::mutation_coverage::github_output(report);
    for annotation in output["annotations"]
        .as_array()
        .context("native workflow annotations missing")?
    {
        println!("{}", annotation.as_str().context("invalid annotation")?);
    }
    let summary = output["summary"]
        .as_str()
        .context("native summary missing")?;
    if let Some(path) = std::env::var_os("GITHUB_STEP_SUMMARY").filter(|p| !p.is_empty()) {
        if !summary.is_empty() {
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .context("open GitHub step summary")?
                .write_all(summary.as_bytes())
                .context("append GitHub step summary")?;
        }
    }
    Ok(())
}
