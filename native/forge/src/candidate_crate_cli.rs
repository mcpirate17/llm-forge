//! Native version discipline over captured source and explicit installed metadata.
#[path = "candidate_crate_plan.rs"]
mod plan;
use crate::{candidate_installed::Installed, candidate_source::Source};
use anyhow::{ensure, Context, Result};
use clap::Args;
use serde_json::json;
use std::path::PathBuf;

#[derive(Args)]
pub struct CrateArgs {
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
    #[arg(long, conflicts_with = "execute")]
    tool_version: bool,
    /// Compare production inputs and versions; otherwise only capture the plan.
    #[arg(long)]
    execute: bool,
    #[arg(last = true, value_name = "FILE")]
    files: Vec<String>,
}

pub fn run(args: CrateArgs) -> Result<u8> {
    match run_inner(args) {
        Ok(code) => Ok(code),
        Err(error) => {
            eprintln!("crate-version incomplete: {error:#}");
            Ok(2)
        }
    }
}

fn run_inner(args: CrateArgs) -> Result<u8> {
    if args.tool_version {
        ensure!(
            args.files.is_empty(),
            "tool version accepts no candidate files"
        );
        println!("forge-native-crate-version 1 {}", env!("CARGO_PKG_VERSION"));
        return Ok(0);
    }
    let root = args
        .repo
        .canonicalize()
        .context("resolve crate-version snapshot")?;
    let (source, candidate, binding) = Source::tool_context(
        &root,
        args.base.as_deref().context("missing base OID")?,
        args.tree.as_deref().context("missing tree OID")?,
    )?;
    let mut plan = plan::Plan::new(&source, &candidate, &args.files)?;
    let installed = Installed::from_env(plan.wanted.clone(), &root)?;
    plan.payload["installed"] = json!(installed.packages);
    binding.verify(&source)?;
    let result = if args.execute {
        Some(
            conductor_native::candidate_crate_version::evaluate(&plan.payload)
                .map_err(anyhow::Error::msg)?,
        )
    } else {
        None
    };
    plan.verify()?;
    installed.verify()?;
    binding.verify(&source)?;
    let failed = result.as_ref().is_some_and(|result| {
        result["findings"]
            .as_array()
            .is_some_and(|rows| !rows.is_empty())
    });
    let report = json!({"schema_version":1,"check":"crate-version","executed":args.execute,
        "plan":{"base_tree_oid":candidate.base_tree_oid,"candidate_tree_oid":candidate.tree_oid,
        "selected_files":plan.selected,"manifest_paths":plan.manifests,"installed_metadata":installed.evidence},"result":result});
    crate::json_canon::encoded_size(&report, 32 * 1024 * 1024)?;
    println!("{}", serde_json::to_string(&report)?);
    Ok(u8::from(failed))
}
