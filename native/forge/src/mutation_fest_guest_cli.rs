//! Export an operator-selected private Fest guest inventory without executing it.
use clap::Args;
use std::path::PathBuf;

#[derive(Args)]
pub struct FestGuestArgs {
    /// Existing private, independently provisioned Python and Fest prefix.
    #[arg(long)]
    root: PathBuf,
    /// New manifest outside the prefix, under a private existing directory.
    #[arg(long)]
    output: PathBuf,
}

pub fn run(args: FestGuestArgs) -> anyhow::Result<u8> {
    let exported = crate::mutation_fest_guest::export(&args.root, &args.output)?;
    println!("{}", serde_json::to_string_pretty(&exported)?);
    Ok(0)
}
