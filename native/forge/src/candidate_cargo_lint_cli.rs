//! Native lint tool; a parent candidate Stage supplies sandbox and process ownership.
#[path = "candidate_cargo_config.rs"]
mod config;
#[path = "candidate_cargo_runtime.rs"]
pub(crate) mod runtime;
use anyhow::{ensure, Context, Result};
use cap_std::fs::Dir;
use clap::Args;
use conductor_native::candidate_cargo_lint::{self as lint, Mode, SkipKind};
use serde_json::json;
use std::path::{Path, PathBuf};

#[derive(Args)]
pub struct LintArgs {
    #[arg(long, default_value = ".")]
    repo: PathBuf,
    #[arg(long, value_parser = ["fmt", "clippy"])]
    mode: String,
    /// Probe the explicitly provisioned native toolchain, bypassing roster planning.
    #[arg(long, conflicts_with = "execute")]
    tool_version: bool,
    /// Run the selected tools. Without this flag, print the confined native plan.
    #[arg(long)]
    execute: bool,
    #[arg(last = true, value_name = "FILE")]
    files: Vec<String>,
}

pub fn run(args: LintArgs) -> Result<u8> {
    let root = args
        .repo
        .canonicalize()
        .context("resolve cargo lint root")?;
    ensure!(root.is_dir(), "cargo lint root must be a directory");
    if args.tool_version {
        ensure!(
            args.files.is_empty(),
            "tool version accepts no candidate files"
        );
        let runtime = runtime::Runtime::new(&root, &args.mode, false)?;
        let result = version(&runtime, &args.mode);
        return completed(result, runtime.finish());
    }
    let directory = Dir::open_ambient_dir(&root, cap_std::ambient_authority())?;
    let (roster_path, roster) = config::load(&directory, &root)?;
    let mode = if args.mode == "fmt" {
        Mode::Fmt
    } else {
        Mode::Clippy
    };
    let files: Vec<&str> = args.files.iter().map(String::as_str).collect();
    let plan = lint::plan(&directory, &roster, &files, mode).map_err(anyhow::Error::msg)?;
    if !args.execute {
        let report = json!({"schema_version":1,"executed":false,"mode":args.mode,
            "roster_path":roster_path,"plan":plan});
        crate::json_canon::encoded_size(&report, 32 * 1024 * 1024)?;
        println!("{}", serde_json::to_string(&report)?);
        return Ok(0);
    }
    let mut incomplete = false;
    for skip in &plan.skips {
        eprintln!(
            "cargo-{}: skipping {} ({})",
            args.mode,
            skip.crate_path.escape_default(),
            skip.reason.escape_default()
        );
        incomplete |= matches!(skip.kind, SkipKind::Prerequisite);
    }
    if plan.manifests.is_empty() {
        return Ok(if incomplete { 2 } else { 0 });
    }
    let runtime = runtime::Runtime::new(&root, &args.mode, args.mode == "clippy")?;
    let result = execute(&runtime, &root, &args.mode, &plan.manifests, incomplete);
    completed(result, runtime.finish())
}
fn completed(result: Result<u8>, cleanup: Result<()>) -> Result<u8> {
    match (result, cleanup) {
        (Ok(code), Ok(())) => Ok(code),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) => Err(error.context("cargo runtime cleanup")),
        (Err(error), Err(cleanup)) => Err(anyhow::anyhow!("{error:#}; cleanup: {cleanup:#}")),
    }
}
fn version(runtime: &runtime::Runtime, mode: &str) -> Result<u8> {
    // Stage records the first stdout line as the actual tool version.
    eprintln!("forge-cargo-lint/1 mode={mode}");
    let status = runtime.command(None)?.args([mode, "--version"]).status()?;
    status_code(status)
}
fn status_code(status: std::process::ExitStatus) -> Result<u8> {
    let code = status
        .code()
        .context("cargo lint tool terminated by signal")?;
    u8::try_from(code).context("invalid cargo lint exit status")
}
fn execute(
    runtime: &runtime::Runtime,
    root: &Path,
    mode: &str,
    manifests: &[String],
    incomplete: bool,
) -> Result<u8> {
    let mut code = if incomplete { 2 } else { 0 };
    for manifest in manifests {
        let path = root.join(manifest);
        eprintln!("cargo-{mode}: {}", manifest.escape_default());
        let mut command = runtime.command(Some(&path))?;
        command.arg(mode);
        if mode == "clippy" {
            command.args(["--offline", "--locked"]);
        }
        command.arg("--manifest-path").arg(path);
        if mode == "fmt" {
            command.arg("--check");
        } else {
            command.args(["--all-targets", "--", "-D", "warnings"]);
        }
        let current = status_code(command.status()?)?;
        if current != 0 {
            code = current;
        }
    }
    Ok(code)
}
