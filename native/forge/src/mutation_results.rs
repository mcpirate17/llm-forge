//! Parse existing test reports without loading a Python runtime or running tests.

use anyhow::{anyhow, bail, Context, Result};
use clap::{Args, ValueEnum};
use conductor_native::mutation_value_inputs::{
    attribution_supported, parse_cargo_libtest, parse_junit_file, JunitAdapter,
};
use std::fs::File;
use std::io::Read;
use std::path::PathBuf;

const MAX_REPORT_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum Adapter {
    PytestJunit,
    CtestJunit,
    CargoLibtest,
}

impl Adapter {
    fn label(self) -> &'static str {
        match self {
            Self::PytestJunit => "pytest-junit",
            Self::CtestJunit => "ctest-junit",
            Self::CargoLibtest => "cargo-libtest",
        }
    }
}

#[derive(Args)]
pub struct ResultsArgs {
    /// Report format: JUnit XML or stable libtest text / JSON lines.
    #[arg(long, value_enum)]
    adapter: Adapter,
    /// Existing report file; the command never launches a test runner.
    #[arg(long)]
    report: PathBuf,
    /// Expected test nodeid; repeat in ranking order.
    #[arg(long = "test", required = true)]
    ranked: Vec<String>,
}

fn read_report(path: &std::path::Path) -> Result<String> {
    let mut text = String::new();
    File::open(path)
        .with_context(|| format!("open report {}", path.display()))?
        .take(MAX_REPORT_BYTES + 1)
        .read_to_string(&mut text)
        .with_context(|| format!("read UTF-8 report {}", path.display()))?;
    if text.len() as u64 > MAX_REPORT_BYTES {
        bail!("report exceeds {MAX_REPORT_BYTES} bytes");
    }
    Ok(text)
}

pub fn run(args: ResultsArgs) -> Result<u8> {
    if !attribution_supported(args.adapter.label(), &args.ranked) {
        bail!("expected test identifiers are invalid or ambiguous for this adapter");
    }
    let parsed = match args.adapter {
        Adapter::PytestJunit => parse_junit_file(&args.report, JunitAdapter::Pytest, &args.ranked),
        Adapter::CtestJunit => parse_junit_file(&args.report, JunitAdapter::Ctest, &args.ranked),
        Adapter::CargoLibtest => parse_cargo_libtest(&read_report(&args.report)?, &args.ranked),
    }
    .map_err(|error| anyhow!(error))?;
    let complete = parsed["status"] == "COMPLETE";
    println!("{}", serde_json::to_string_pretty(&parsed)?);
    Ok(if complete { 0 } else { 1 })
}
