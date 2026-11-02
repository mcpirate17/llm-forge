//! Internal Stage worker; no interpreter is linked by the Forge binary.
use anyhow::{ensure, Context, Result};
use clap::Args;
use std::{
    io::Write,
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Args)]
pub struct EquivalenceWorkerArgs {
    /// Operator-granted interpreter; Stage confines every guest descendant.
    #[arg(long)]
    pub python: PathBuf,
    /// Operator-held, digest-bound guest extension outside the private runtime.
    #[arg(long)]
    pub guest_path: PathBuf,
    /// Native-produced target plan in the same private runtime directory.
    #[arg(long)]
    pub targets: PathBuf,
    /// Summary file created once in the same runtime directory.
    #[arg(long)]
    pub output: PathBuf,
    /// Captured waiver file from the same candidate tree.
    #[arg(long)]
    pub waivers: Option<PathBuf>,
    #[arg(long)]
    pub timeout_seconds: f64,
    #[arg(long)]
    pub jobs: usize,
}
fn direct_child(root: &Path, child: &Path) -> Result<()> {
    ensure!(
        child.is_absolute() && child.parent() == Some(root),
        "worker input is not a direct runtime child"
    );
    Ok(())
}
pub fn run(args: EquivalenceWorkerArgs) -> Result<()> {
    ensure!(
        args.timeout_seconds.is_finite() && (0.001..=3600.0).contains(&args.timeout_seconds),
        "invalid module timeout"
    );
    ensure!((1..=32).contains(&args.jobs), "invalid worker count");
    let runtime = args.targets.parent().context("target plan has no parent")?;
    ensure!(
        args.guest_path.is_absolute() && !args.guest_path.starts_with(runtime),
        "invalid external guest path"
    );
    direct_child(runtime, &args.targets)?;
    direct_child(runtime, &args.output)?;
    if let Some(waivers) = &args.waivers {
        direct_child(runtime, waivers)?;
    }
    ensure!(
        args.python.is_absolute(),
        "interpreter must be an explicit absolute grant"
    );
    let info = std::fs::metadata(&args.targets)?;
    ensure!(
        info.is_file() && info.len() <= 8 * 1024 * 1024,
        "invalid target plan"
    );
    let input = std::fs::read(&args.targets)?;
    ensure!(input.len() <= 8 * 1024 * 1024, "target plan exceeds 8 MiB");
    let targets: Vec<slop_core::gate_types::Target> = serde_json::from_slice(&input)?;
    ensure!(targets.len() <= 4096, "target plan exceeds 4096 modules");
    let root = std::env::current_dir()?;
    ensure!(
        root.parent() == runtime.parent()
            && runtime.file_name().is_some_and(|name| name == "runtime"),
        "equivalence worker requires a private Stage snapshot"
    );
    let python = args.python.to_str().context("interpreter is not UTF-8")?;
    let summary = slop_core::gate::gate_with_guest(
        &root,
        python,
        Some(&args.guest_path),
        &targets,
        args.waivers.as_deref(),
        Duration::from_secs_f64(args.timeout_seconds),
        Some(args.jobs),
    )?;
    let data = serde_json::to_vec(&summary)?;
    ensure!(
        data.len() <= 4 * 1024 * 1024,
        "equivalence summary exceeds 4 MiB"
    );
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(args.output)?;
    file.write_all(&data)?;
    file.sync_all()?;
    Ok(())
}
