//! One candidate-built contract runtime, inside the existing supervised Stage.
use crate::{
    candidate_cargo_lint_cli::runtime::Runtime,
    candidate_cargo_test_cli::{capture_shared, Captured},
    candidate_contract_run,
};
use anyhow::{ensure, Context, Result};
use clap::Args;
use std::{
    collections::BTreeSet,
    fs::OpenOptions,
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    process::Command,
};
#[derive(Args)]
pub struct ContractArgs {
    #[arg(long)]
    repo: PathBuf,
    #[arg(long)]
    base: String,
    #[arg(long)]
    tree: String,
    #[arg(long)]
    python: PathBuf,
    #[arg(long = "target")]
    targets: Vec<String>,
    #[arg(long = "bind")]
    bind: Vec<String>,
    #[arg(long)]
    execute: bool,
    #[arg(long, conflicts_with = "execute")]
    tool_version: bool,
}
fn runtime(snapshot: &Path) -> Result<PathBuf> {
    let raw = std::env::var_os("TMPDIR").context("private TMPDIR missing")?;
    crate::candidate_rust_coverage_run::private_runtime(Path::new(&raw), snapshot)
}
fn capture(args: &ContractArgs, targets: &[String]) -> Result<Captured> {
    ensure!(
        args.python.is_absolute() && std::fs::metadata(&args.python)?.is_file(),
        "invalid granted contract Python"
    );
    let mut paths: BTreeSet<String> = args.bind.iter().cloned().collect();
    paths.insert("native/forge/Cargo.toml".into());
    for target in targets {
        paths.insert(format!("native/conductor-native/tests/{target}.rs"));
    }
    ensure!(paths.len() <= 4095, "too many contract bindings");
    capture_shared(
        &args.repo,
        &args.base,
        &args.tree,
        "native/conductor-native/Cargo.toml",
        &paths.into_iter().collect::<Vec<_>>(),
    )
}
fn write_evidence(runtime: &Path, value: &impl serde::Serialize) -> Result<()> {
    let bytes = serde_json::to_vec(value)?;
    ensure!(
        bytes.len() <= 1024 * 1024,
        "contract evidence exceeds limit"
    );
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(runtime.join("contract-evidence.json"))?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    Ok(())
}
fn execute(
    args: &ContractArgs,
    targets: &[String],
    private: &Path,
    runtime: &Runtime,
) -> Result<()> {
    if args.tool_version {
        let mut cargo = runtime.command(None)?;
        cargo.arg("--version");
        ensure!(cargo.status()?.success(), "Cargo version failed");
        let mut python = Command::new(&args.python);
        candidate_contract_run::pin_python(&mut python, &args.python)?;
        let status = python.arg("--version").status()?;
        ensure!(status.success(), "contract Python version failed");
    } else {
        let evidence =
            candidate_contract_run::run(runtime, &args.repo, private, &args.python, targets)?;
        write_evidence(private, &evidence)?;
    }
    Ok(())
}
pub fn run(args: ContractArgs) -> Result<u8> {
    let targets = candidate_contract_run::targets(&args.targets)?;
    let primary = capture(&args, &targets)?;
    let private = runtime(&args.repo)?;
    let slop = if candidate_contract_run::slop_needed(&args.repo, &private, &args.python, &targets)?
    {
        Some(capture_shared(
            &args.repo,
            &args.base,
            &args.tree,
            "native/slop-core/Cargo.toml",
            &[],
        )?)
    } else {
        None
    };
    primary.verify()?;
    if let Some(bound) = &slop {
        bound.verify()?;
    }
    if !args.execute && !args.tool_version {
        return Ok(0);
    }
    let runtime = Runtime::new(&args.repo, "test", !args.tool_version)?;
    let result = execute(&args, &targets, &private, &runtime);
    let verified = primary
        .verify()
        .and_then(|_| slop.as_ref().map_or(Ok(()), Captured::verify));
    let cleanup = runtime.finish();
    ensure!(
        result.is_ok() && verified.is_ok() && cleanup.is_ok(),
        "contract run: {}; binding: {}; cleanup: {}",
        result
            .err()
            .map(|e| format!("{e:#}"))
            .unwrap_or_else(|| "ok".into()),
        verified
            .err()
            .map(|e| format!("{e:#}"))
            .unwrap_or_else(|| "ok".into()),
        cleanup
            .err()
            .map(|e| format!("{e:#}"))
            .unwrap_or_else(|| "ok".into())
    );
    Ok(0)
}
