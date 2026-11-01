//! Native changed-line scanners; the parent candidate Stage owns execution isolation.
#[path = "candidate_scan_config.rs"]
mod config;
#[path = "candidate_scan_mode.rs"]
mod mode;
#[path = "candidate_scan_plan.rs"]
mod plan;

use crate::{
    candidate_source::Source,
    source_checks::{self, Finding},
};
use anyhow::{ensure, Context, Result};
use clap::Args;
use mode::Mode;
use serde_json::json;
use std::path::PathBuf;

#[derive(Args)]
pub struct ScanArgs {
    #[arg(long, value_enum)]
    check: Mode,
    #[arg(long, default_value = ".")]
    repo: PathBuf,
    #[arg(
        long,
        required_unless_present = "tool_version",
        conflicts_with = "tool_version"
    )]
    base: Option<String>,
    #[arg(
        long,
        required_unless_present = "tool_version",
        conflicts_with = "tool_version"
    )]
    tree: Option<String>,
    /// Report native scanner identity without opening Git or configuration.
    #[arg(long, conflicts_with = "execute")]
    tool_version: bool,
    /// Execute the planned scanners; otherwise only bind and report inputs.
    #[arg(long)]
    execute: bool,
    #[arg(last = true, value_name = "FILE")]
    files: Vec<String>,
}

pub fn run(args: ScanArgs) -> Result<u8> {
    match run_inner(args) {
        Ok(code) => Ok(code),
        Err(error) => {
            eprintln!("candidate scan incomplete: {error:#}");
            Ok(2)
        }
    }
}

fn run_inner(args: ScanArgs) -> Result<u8> {
    if args.tool_version {
        ensure!(
            args.files.is_empty(),
            "tool version accepts no candidate files"
        );
        println!(
            "forge-native-candidate-scan 1 {}",
            env!("CARGO_PKG_VERSION")
        );
        return Ok(0);
    }
    let root = args.repo.canonicalize().context("resolve scan snapshot")?;
    let (source, candidate, binding) = Source::tool_context(
        &root,
        args.base.as_deref().context("missing base OID")?,
        args.tree.as_deref().context("missing candidate tree OID")?,
    )?;
    let plan = plan::Plan::new(&source, &candidate, args.check, &args.files)?;
    binding.verify(&source)?;
    let findings = if args.execute {
        Some(scan(args.check, &plan)?)
    } else {
        None
    };
    plan.verify()?;
    binding.verify(&source)?;
    let failed = findings.as_ref().is_some_and(|rows| !rows.is_empty());
    let report = json!({"schema_version":1,"check":args.check,"executed":args.execute,
        "plan":plan.ranges,"excluded":plan.excluded,"hot_paths":plan.hot_paths,
        "config_blob_oid":plan.config_blob_oid,"source_bytes":plan.source_bytes,
        "scanned_file_count":if args.execute {plan.sources.len()} else {0},"findings":findings});
    crate::json_canon::encoded_size(&report, 32 * 1024 * 1024)?;
    println!("{}", serde_json::to_string(&report)?);
    Ok(u8::from(failed))
}

fn scan(mode: Mode, plan: &plan::Plan) -> Result<Vec<Finding>> {
    let mut findings = Vec::new();
    for (index, text) in &plan.sources {
        let file = &plan.ranges.files[*index];
        let rows = source_checks::scan(&file.path, text, mode.checks())
            .with_context(|| format!("scan {}", file.path))?;
        for row in rows {
            ensure!(
                row.path == file.path && row.line > 0 && row.end_line >= row.line,
                "invalid scanner finding"
            );
            if mode.includes(&row, &file.ranges) {
                ensure!(
                    findings.len() < 4096,
                    "candidate findings exceed 4096; scan incomplete"
                );
                findings.push(row);
            }
        }
    }
    findings.sort();
    Ok(findings)
}
