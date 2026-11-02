//! Bounded interpreter-free Radon 6.0.1 report and baseline ratchet.
mod input;
mod report;
use anyhow::{ensure, Result};
use clap::{Args, ValueEnum};
use conductor_native::{project_paths, radon_complexity};
use std::path::PathBuf;

const DEFAULT_PATHS: &[&str] = &["conductor", "research", "aria_core", "aria_designer"];
const DEFAULT_EXCLUDES: &[&str] = &[
    "*/rust/*",
    "*/tests/*",
    "*/tmp/*",
    "*/research/tmp/*",
    "*__pycache__*",
];
const MAX_ROWS: usize = 65536;
const MAX_BASELINE: usize = 16 * 1024 * 1024;

#[derive(Args)]
pub struct ComplexityArgs {
    /// Host start directory; existing CONDUCTOR_HOST_ROOT precedence is retained.
    #[arg(long, global = true, value_parser = absolute_repo)]
    repo: Option<PathBuf>,
    /// Relative to the host root, or an explicit absolute baseline file.
    #[arg(long, global = true)]
    baseline: Option<PathBuf>,
    #[arg(long, default_value = "D", value_parser = ["A", "B", "C", "D", "E", "F"], global = true)]
    min_rank: String,
    #[arg(long = "path", global = true)]
    paths: Vec<String>,
    /// Replaces the default exclusion list when provided (fnmatch semantics).
    #[arg(long = "exclude", global = true)]
    excludes: Vec<String>,
    #[arg(long, exclusive = true)]
    tool_version: bool,
    #[arg(value_enum)]
    command: Option<Action>,
}
#[derive(Clone, Copy, ValueEnum)]
enum Action {
    Report,
    Check,
    RefreshBaseline,
}
fn absolute_repo(raw: &str) -> std::result::Result<PathBuf, String> {
    let path = PathBuf::from(raw);
    if !path.is_absolute() {
        return Err("--repo must be absolute".into());
    }
    Ok(path)
}
fn choices(values: Vec<String>, defaults: &[&str]) -> Vec<String> {
    if values.is_empty() {
        defaults.iter().map(|v| (*v).into()).collect()
    } else {
        values
    }
}
pub fn run(args: ComplexityArgs) -> Result<u8> {
    if args.tool_version {
        ensure!(
            args.command.is_none(),
            "--tool-version cannot run a complexity command"
        );
        println!("{}", radon_complexity::PROTOCOL);
        return Ok(0);
    }
    let Some(action) = args.command else {
        eprintln!("complexity requires report, check or refresh-baseline");
        return Ok(2);
    };
    let root = project_paths::host_root(args.repo.as_deref()).map_err(anyhow::Error::msg)?;
    let baseline = match args.baseline {
        Some(path) => root.join(path),
        None => project_paths::radon_baseline(&root).map_err(anyhow::Error::msg)?,
    };
    // Every command resolves the path before scanning, but only check reads it.
    let baseline = input::resolve(&baseline)?;
    let paths = choices(args.paths, DEFAULT_PATHS);
    let excludes = choices(args.excludes, DEFAULT_EXCLUDES);
    let scan = match input::scan(&root, &paths, &excludes) {
        Ok(scan) => scan,
        Err(error) => {
            println!("Parse error: {error:#}");
            return Ok(2);
        }
    };
    if !scan.errors.is_empty() {
        for (path, error) in scan.errors {
            println!("Parse error: {path}: {error}");
        }
        return Ok(2);
    }
    match action {
        Action::Report => {
            report::print(&scan.blocks, &args.min_rank);
            Ok(0)
        }
        Action::Check => report::check(&baseline, &scan.blocks, &args.min_rank),
        Action::RefreshBaseline => {
            report::write(&baseline, &scan.blocks, &args.min_rank, &paths, &excludes)?;
            report::print(&scan.blocks, &args.min_rank);
            println!(
                "Wrote baseline: {}",
                baseline.strip_prefix(&root).unwrap_or(&baseline).display()
            );
            Ok(0)
        }
    }
}
