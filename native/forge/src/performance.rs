//! Numerical benchmark receipts and explicit optional profiler invocations.

use crate::land_exec::{run_measured, tail, Outcome, ShellRun};
use anyhow::{ensure, Context, Result};
use clap::{Args, Subcommand, ValueEnum};
use conductor_native::performance_receipt::{
    compare, summarize, Evidence, Receipt, Sample, EVIDENCE_SCHEMA, MAX_GATE_REGRESSION_PERCENT,
    SCHEMA,
};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[path = "performance_identity.rs"]
pub(crate) mod identity;

#[derive(Args)]
pub struct PerformanceArgs {
    #[command(subcommand)]
    action: PerformanceCommand,
}

#[derive(Subcommand)]
enum PerformanceCommand {
    /// Measure a literal shell workload; every sample must succeed.
    Benchmark(BenchmarkArgs),
    /// Compare validated receipts with matched workload/environment identities.
    Compare(CompareArgs),
    /// Invoke an explicitly selected external profiler; never installs tools.
    Profile(ProfileArgs),
}

#[derive(Args)]
struct BenchmarkArgs {
    #[arg(long)]
    command: String,
    #[arg(long, required = true)]
    source: Vec<String>,
    #[arg(long)]
    input: Vec<String>,
    #[arg(long)]
    env: Vec<String>,
    #[arg(long, default_value_t = 20)]
    samples: usize,
    #[arg(long, default_value_t = 2)]
    warmups: usize,
    #[arg(long, default_value_t = 60)]
    timeout_seconds: u64,
    #[arg(long)]
    output: PathBuf,
}

#[derive(Args)]
struct CompareArgs {
    #[arg(long)]
    current: PathBuf,
    #[arg(long)]
    baseline: PathBuf,
    #[arg(long, default_value_t = 10.0)]
    max_regression_percent: f64,
    /// Save both sealed receipts and the bounded regression budget for review.
    #[arg(long)]
    output: Option<PathBuf>,
}

#[derive(Clone, ValueEnum)]
enum Profiler {
    Hyperfine,
    PySpy,
    CargoFlamegraph,
}

#[derive(Args)]
struct ProfileArgs {
    #[arg(long, value_enum)]
    tool: Profiler,
    #[arg(long)]
    output: PathBuf,
    #[arg(long, default_value_t = 300)]
    timeout_seconds: u64,
    /// Profiler/workload arguments after --, passed as literal argv.
    #[arg(last = true, required = true)]
    args: Vec<String>,
}

pub fn run(args: PerformanceArgs) -> Result<u8> {
    match args.action {
        PerformanceCommand::Benchmark(args) => benchmark(args),
        PerformanceCommand::Compare(args) => {
            let current = load(&args.current)?;
            let baseline = load(&args.baseline)?;
            let comparison = compare(&current, &baseline, args.max_regression_percent)
                .map_err(anyhow::Error::msg)?;
            if let Some(output) = args.output {
                ensure!(
                    args.max_regression_percent <= MAX_GATE_REGRESSION_PERCENT,
                    "review evidence budget may not exceed {MAX_GATE_REGRESSION_PERCENT}%"
                );
                let evidence = Evidence {
                    schema: EVIDENCE_SCHEMA.into(),
                    current,
                    baseline,
                    max_regression_percent: args.max_regression_percent,
                };
                write_new(&output, &serde_json::to_vec_pretty(&evidence)?)?;
            }
            println!("{}", serde_json::to_string(&comparison)?);
            Ok(if comparison.passed { 0 } else { 1 })
        }
        PerformanceCommand::Profile(args) => profile(args),
    }
}

fn load(path: &Path) -> Result<Receipt> {
    ensure!(
        std::fs::metadata(path)?.len() <= 2 * 1024 * 1024,
        "benchmark receipt exceeds 2 MiB"
    );
    let receipt: Receipt = serde_json::from_slice(&std::fs::read(path)?)?;
    receipt.validate().map_err(anyhow::Error::msg)?;
    Ok(receipt)
}

fn benchmark(args: BenchmarkArgs) -> Result<u8> {
    ensure!(
        (5..=1000).contains(&args.samples) && args.warmups <= 1000,
        "need 5..1000 samples and <=1000 warmups"
    );
    ensure!(
        (1..=86400).contains(&args.timeout_seconds),
        "timeout-seconds must be 1..86400"
    );
    ensure!(
        !args.command.trim().is_empty() && !args.output.exists(),
        "command must be nonempty and output must be new"
    );
    let root = crate::local_check::root()?;
    let env = identity::parse_env(&args.env)?;
    let before = identity::identity(
        &root,
        &args.source,
        &args.command,
        &args.input,
        &env,
        args.warmups,
        args.timeout_seconds,
    )?;
    let dir = scratch(&root)?;
    let mut samples = Vec::with_capacity(args.samples);
    for index in 0..args.warmups + args.samples {
        let log = dir.join(format!("sample-{index}.log"));
        let measured = run_measured(
            &ShellRun {
                command: &args.command,
                cwd: &root,
                env: &env,
                log: &log,
                timeout: Duration::from_secs(args.timeout_seconds),
            },
            1024 * 1024,
        )?;
        ensure!(
            measured.outcome == Outcome::Passed,
            "benchmark sample {index} failed: {:?}; {}\n{}",
            measured.outcome,
            log.display(),
            tail(&log, 20)
        );
        if index >= args.warmups {
            samples.push(Sample {
                wall_ms: measured.usage.wall_ms,
                cpu_ms: measured.usage.cpu_ms,
                max_rss_bytes: measured.usage.max_rss_bytes,
            });
        }
    }
    let after = identity::identity(
        &root,
        &args.source,
        &args.command,
        &args.input,
        &env,
        args.warmups,
        args.timeout_seconds,
    )?;
    ensure!(
        before == after,
        "source/workload/environment changed during benchmark; no receipt issued"
    );
    let mut receipt = Receipt {
        schema: SCHEMA.into(),
        created_unix: SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),
        identity: before,
        summary: summarize(&samples).map_err(anyhow::Error::msg)?,
        samples,
        receipt_sha256: String::new(),
    };
    receipt.seal().map_err(anyhow::Error::msg)?;
    receipt.validate().map_err(anyhow::Error::msg)?;
    write_new(&args.output, &serde_json::to_vec_pretty(&receipt)?)?;
    println!(
        "{}",
        serde_json::json!({"receipt":args.output,"sha256":receipt.receipt_sha256,"summary":receipt.summary})
    );
    Ok(0)
}

fn scratch(root: &Path) -> Result<PathBuf> {
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let path = crate::local_check::common_dir(root)?
        .join("forge-performance")
        .join(format!("run-{nonce}-{}", std::process::id()));
    std::fs::create_dir_all(&path)?;
    Ok(path)
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    let result = std::fs::hard_link(&temporary, path)
        .with_context(|| format!("publishing new receipt {}", path.display()));
    let cleanup = std::fs::remove_file(&temporary);
    result?;
    cleanup?;
    Ok(())
}

pub(crate) fn executable(name: &str) -> Result<PathBuf> {
    for dir in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
        let path = dir.join(name);
        if path.is_file() {
            use std::os::unix::fs::PermissionsExt;
            if path.metadata()?.permissions().mode() & 0o111 != 0 {
                return Ok(path);
            }
        }
    }
    anyhow::bail!(
        "optional profiler {name} is unavailable; install the declared tool before requesting it"
    )
}

fn profile(args: ProfileArgs) -> Result<u8> {
    ensure!(
        (1..=86400).contains(&args.timeout_seconds) && !args.output.exists(),
        "profile timeout must be 1..86400; output must be new"
    );
    let (name, prefix) = match args.tool {
        Profiler::Hyperfine => ("hyperfine", vec!["--export-json"]),
        Profiler::PySpy => ("py-spy", vec!["record", "--output"]),
        Profiler::CargoFlamegraph => {
            executable("perf")?;
            ("cargo-flamegraph", vec!["flamegraph", "--output"])
        }
    };
    let mut argv = vec![identity::quote(&executable(name)?.display().to_string())];
    argv.extend(prefix.into_iter().map(identity::quote));
    argv.push(identity::quote(&args.output.display().to_string()));
    argv.extend(args.args.iter().map(|arg| identity::quote(arg)));
    let root = crate::local_check::root()?;
    let log = scratch(&root)?.join("profile.log");
    let measured = run_measured(
        &ShellRun {
            command: &argv.join(" "),
            cwd: &root,
            env: &[],
            log: &log,
            timeout: Duration::from_secs(args.timeout_seconds),
        },
        1024 * 1024,
    )?;
    ensure!(
        measured.outcome == Outcome::Passed && args.output.is_file(),
        "profiler failed: {:?}; {}\n{}",
        measured.outcome,
        log.display(),
        tail(&log, 20)
    );
    println!(
        "{}",
        serde_json::json!({"tool":name,"output":args.output,"usage":measured.usage})
    );
    Ok(0)
}
