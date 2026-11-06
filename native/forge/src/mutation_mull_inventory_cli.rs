//! Observe one operator-selected runtime directory without granting execution.
use clap::Args;
use serde_json::json;
use std::path::PathBuf;

#[derive(Args)]
pub struct MullInventoryArgs {
    /// Existing canonical, safely owned directory; symlink entries are refused.
    #[arg(long)]
    directory: PathBuf,
}
pub fn run(args: MullInventoryArgs) -> anyhow::Result<u8> {
    let digest = crate::mutation_mull_toolchain::observe_directory(&args.directory)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "schema_version":1,"directory":args.directory,"sha256":digest,
            "scope":"point-in-time sorted content inventory; no execution authority"
        }))?
    );
    Ok(0)
}
