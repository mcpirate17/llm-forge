//! Execute source analysis against private captured candidate Git context.
use crate::{
    candidate_analysis::{Mode, Plan},
    candidate_source::Source,
};
use anyhow::{ensure, Context, Result};
use clap::Args;
use serde_json::json;
use std::path::PathBuf;

#[derive(Args)]
pub struct AnalysisArgs {
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
    #[arg(long)]
    policy: Option<String>,
    #[arg(long)]
    today: Option<String>,
    #[arg(long, conflicts_with = "execute")]
    tool_version: bool,
    #[arg(long)]
    execute: bool,
    #[arg(last = true, value_name = "FILE")]
    files: Vec<String>,
}
pub fn run(args: AnalysisArgs) -> Result<u8> {
    match run_inner(args) {
        Ok(code) => Ok(code),
        Err(error) => {
            eprintln!("analysis incomplete: {error:#}");
            Ok(2)
        }
    }
}
fn run_inner(args: AnalysisArgs) -> Result<u8> {
    if args.tool_version {
        ensure!(
            args.files.is_empty(),
            "tool-version accepts no candidate files"
        );
        println!(
            "forge-native-analysis 1 {} {}",
            env!("CARGO_PKG_VERSION"),
            args.check.id()
        );
        return Ok(0);
    }
    let root = args
        .repo
        .canonicalize()
        .context("resolve analysis snapshot")?;
    let (source, mut candidate, binding) = Source::tool_context(
        &root,
        args.base.as_deref().context("missing base OID")?,
        args.tree.as_deref().context("missing tree OID")?,
    )?;
    let today = args
        .today
        .map(Ok)
        .unwrap_or_else(crate::candidate_plan::today)?;
    let policy = crate::candidate_policy::load(&root, args.policy.as_deref(), &today)?;
    policy.classify_candidate(&mut candidate)?;
    let provider = if args.check == Mode::ImportDeclaration {
        std::env::var("FORGE_IMPORT_PROVIDERS")
            .map(Some)
            .or_else(|error| {
                if error == std::env::VarError::NotPresent {
                    Ok(None)
                } else {
                    Err(error)
                }
            })
            .context("provider specification must be UTF-8")?
    } else {
        None
    };
    let plan = Plan::new(
        &source,
        &candidate,
        &root,
        &policy,
        args.check,
        &args.files,
        provider.as_deref(),
    )?;
    binding.verify(&source)?;
    let result = if args.execute {
        Some(plan.evaluate()?)
    } else {
        None
    };
    plan.verify()?;
    binding.verify(&source)?;
    let failed = result.as_ref().is_some_and(|result| {
        result["findings"]
            .as_array()
            .is_some_and(|rows| !rows.is_empty())
    });
    let report = json!({"schema_version":1,"check":args.check.id(),"executed":args.execute,
        "plan":plan.report,"result":result});
    crate::json_canon::encoded_size(&report, 32 * 1024 * 1024)?;
    println!("{}", serde_json::to_string(&report)?);
    Ok(u8::from(failed))
}
