//! Native candidate analysis; the existing parent Stage owns execution isolation.
#[path = "candidate_clang_tidy_config.rs"]
mod config;
#[path = "candidate_clang_tidy_context.rs"]
mod context;
#[path = "candidate_clang_tidy_coverage.rs"]
mod coverage;
use crate::candidate_inputs as inputs;
#[path = "candidate_clang_tidy_plan.rs"]
mod plan;
#[path = "candidate_clang_tidy_state.rs"]
mod state;

use crate::{
    candidate_clang_format_cli::{tool, verify_files},
    candidate_source::{lines::RangeScope, Source},
};
use anyhow::{ensure, Context as _, Result};
use cap_std::fs::Dir;
use clap::Args;
use serde_json::json;
use std::{
    io::Write,
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Args)]
pub struct TidyArgs {
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
    /// Probe the explicitly provisioned native binary without opening Git.
    #[arg(long, conflicts_with = "execute")]
    tool_version: bool,
    /// Run every selected configuration; otherwise emit the complete bound plan.
    #[arg(long)]
    execute: bool,
    #[arg(last = true, value_name = "FILE")]
    files: Vec<String>,
}

pub fn run(args: TidyArgs) -> Result<u8> {
    match run_inner(args) {
        Ok(code) => Ok(code),
        Err(error) => {
            eprintln!("clang-tidy incomplete: {error:#}");
            Ok(2)
        }
    }
}

fn run_inner(args: TidyArgs) -> Result<u8> {
    let root = args.repo.canonicalize().context("resolve tidy snapshot")?;
    ensure!(root.is_dir(), "tidy snapshot must be a directory");
    if args.tool_version {
        ensure!(
            args.files.is_empty(),
            "tool version accepts no candidate files"
        );
        return tool::Tool::from_env_key(&root, "FORGE_CLANG_TIDY")?.tidy_version();
    }
    ensure!(
        !args.files.is_empty() && args.files.len() <= 4096,
        "tidy requires 1..4096 selected paths"
    );
    let (source, candidate, binding) = Source::tool_context(
        &root,
        args.base.as_deref().context("missing base OID")?,
        args.tree.as_deref().context("missing candidate tree OID")?,
    )?;
    let selected: Vec<_> = args.files.iter().map(String::as_str).collect();
    let ranges = source.changed_ranges(&candidate, &selected)?;
    let directory = Dir::open_ambient_dir(&root, cap_std::ambient_authority())?;
    verify_files(&directory, &ranges.files)?;
    if !ranges
        .files
        .iter()
        .any(|file| file.scope == RangeScope::Changed)
    {
        binding.verify(&source)?;
        println!(
            "{}",
            json!({"schema_version":1,"executed":false,"plan":{"invocation_count":0,"reason":"no added or modified lines"}})
        );
        return Ok(0);
    }
    let mut context = context::Context::load(&source, &candidate)?;
    let plan = plan::Plan::new(&mut context, &ranges)?;
    binding.verify(&source)?;
    if !args.execute {
        let report = json!({"schema_version":1,"executed":false,"plan":plan.report()});
        crate::json_canon::encoded_size(&report, 32 * 1024 * 1024)?;
        println!("{}", serde_json::to_string(&report)?);
        return Ok(0);
    }
    let result = execute(&root, &plan);
    let verified = (|| -> Result<()> {
        context.verify()?;
        verify_files(&directory, &ranges.files)?;
        binding.verify(&source)
    })();
    combine(result, verified)
}

fn combine(result: Result<u8>, finalization: Result<()>) -> Result<u8> {
    match (result, finalization) {
        (Ok(code), Ok(())) => Ok(code),
        (Err(error), Ok(())) | (Ok(_), Err(error)) => Err(error),
        (Err(error), Err(finalization)) => {
            Err(error.context(format!("finalization also failed: {finalization:#}")))
        }
    }
}

fn execute(root: &Path, plan: &plan::Plan) -> Result<u8> {
    let tool = tool::Tool::from_env_key(root, "FORGE_CLANG_TIDY")?;
    let mut state = state::State::new(root)?;
    let result = (|| -> Result<u8> {
        let mut code = 0;
        for (index, invocation) in plan.invocations.iter().enumerate() {
            let status = invoke(root, &tool, &mut state, plan, index, invocation)?;
            if status != 0 {
                code = status;
            }
        }
        tool.verify()?;
        Ok(code)
    })();
    combine(result, state.finish())
}

fn invoke(
    root: &Path,
    tool: &tool::Tool,
    state: &mut state::State,
    plan: &plan::Plan,
    index: usize,
    invocation: &plan::Invocation,
) -> Result<u8> {
    let paths = state.write_case(index, &invocation.database, &invocation.config)?;
    let scratch = state.path();
    eprintln!(
        "clang-tidy: {} (database record {})",
        invocation.file.escape_default(),
        invocation.record
    );
    let mut result = analyze(root, tool, &scratch, &paths, invocation, &plan.line_filter)?;
    if result.status.success() && !invocation.headers.is_empty() {
        let expanded = coverage::expanded_filter(
            &result.stderr,
            &invocation.directory,
            &invocation.headers,
            &plan.line_filter,
        )?;
        if expanded != plan.line_filter {
            // LLVM filters diagnostics by the observed spelling, including ../.
            // One bounded retry preserves exact line ranges for those aliases.
            result = analyze(root, tool, &scratch, &paths, invocation, &expanded)?;
            if result.status.success() {
                ensure!(
                    coverage::expanded_filter(
                        &result.stderr,
                        &invocation.directory,
                        &invocation.headers,
                        &plan.line_filter
                    )? == expanded,
                    "header include spellings changed between analysis attempts"
                );
            }
        }
    }
    std::io::stdout().write_all(&result.stdout)?;
    std::io::stderr().write_all(&result.stderr)?;
    tool::status_code(result.status)
}

fn analyze(
    root: &Path,
    tool: &tool::Tool,
    scratch: &Path,
    paths: &(PathBuf, PathBuf),
    invocation: &plan::Invocation,
    filter: &str,
) -> Result<std::process::Output> {
    let mut command = tool.command(root)?;
    command
        .env("HOME", scratch)
        .env("TMPDIR", scratch)
        .arg(format!("-p={}", paths.0.display()))
        .arg(format!("--config-file={}", paths.1.display()))
        .args([
            "--warnings-as-errors=*",
            "--header-filter=.*",
            "--exclude-header-filter=",
            "--system-headers",
            "--format-style=none",
            "--use-color=false",
        ])
        .arg(format!("--line-filter={filter}"))
        .arg(root.join(&invocation.file));
    let bytes: usize = command
        .get_args()
        .map(|arg| arg.as_encoded_bytes().len() + 1)
        .sum();
    ensure!(bytes <= 128 * 1024, "clang-tidy command exceeds 128 KiB");
    let result =
        crate::process_probe::run(&mut command, Duration::from_secs(120), coverage::MAX_TRACE)?;
    tool.verify()?;
    Ok(result)
}
