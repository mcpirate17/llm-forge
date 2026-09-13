//! `forge mutation receipt-show`: print a receipt with its detail expanded.
//!
//! The human/debug window into the slim-receipt format: reads the file, runs
//! the same native expansion `receipt_expand_detail_native` performs for
//! Python readers, and prints the full receipt (summary plus per-mutant
//! detail) as pretty JSON. Superseded receipts refuse loudly -- a pointer is
//! not evidence, and pretending otherwise would hide which file to read.

use anyhow::{bail, Context, Result};
use clap::Args;
use conductor_native::receipt_slim::expand_file;
use std::path::PathBuf;

#[derive(Args)]
pub struct ShowArgs {
    /// Receipt file to expand and print.
    pub path: PathBuf,
}

pub fn run(args: &ShowArgs) -> Result<u8> {
    match expand_file(&args.path) {
        Ok(receipt) => {
            println!(
                "{}",
                serde_json::to_string_pretty(&receipt).context("expanded receipt serializes")?
            );
            Ok(0)
        }
        Err(error) => {
            bail!("{}: {error}", args.path.display());
        }
    }
}
