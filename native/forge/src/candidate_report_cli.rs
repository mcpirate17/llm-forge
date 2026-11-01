//! Native report export. Successful export does not mean a review passed.
use crate::{bounded_file, candidate_receipt, candidate_report, candidate_report_store};
use anyhow::{Context, Result};
use clap::Args;
use std::{io::Write, path::PathBuf};

#[derive(Args)]
pub struct ReportArgs {
    /// A sealed, schema-version-1 receipt. Its recorded evidence remains untrusted.
    file: PathBuf,
    #[arg(long, value_enum, default_value = "human")]
    format: candidate_report::Format,
    /// Atomically replace this file. Its existing parent must contain no symlinks.
    /// Omit to write the complete rendered report to standard output.
    #[arg(long)]
    output: Option<PathBuf>,
}

pub fn run(args: ReportArgs) -> Result<()> {
    let bytes = bounded_file::read(&args.file, candidate_receipt::MAX_BYTES)
        .context("read candidate receipt for export")?;
    let receipt = candidate_receipt::parse_and_verify(&bytes)?;
    let report = candidate_report::render(&receipt, args.format)?;
    if let Some(path) = args.output {
        candidate_report_store::publish(&path, &report)?;
    } else {
        let mut output = std::io::stdout().lock();
        output
            .write_all(&report)
            .context("write candidate report")?;
        output.flush().context("flush candidate report")?;
    }
    Ok(())
}
