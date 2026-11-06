//! Native preparation of explicitly selected offline Cargo inputs.
use crate::mutation_cargo_seed::{self, ProvisionRequest};
use anyhow::Context;
use clap::Args;
use std::path::PathBuf;

#[derive(Args)]
pub struct SeedArgs {
    /// Explicit Cargo cache home; only registry archive/index inputs are read.
    #[arg(long)]
    cache_root: PathBuf,
    /// Cargo.lock whose registry packages and checksums select the archives.
    #[arg(long)]
    lockfile: PathBuf,
    /// Independently captured SHA-256 of the exact lockfile bytes.
    #[arg(long)]
    lockfile_sha256: String,
    /// Explicit opaque registry cache namespace used by the selected Cargo version.
    #[arg(long)]
    registry_directory: String,
    /// Operator pin for the Cargo executable that will consume this seed.
    #[arg(long)]
    cargo_sha256: String,
    /// New private destination directory; an existing destination is refused.
    #[arg(long)]
    output: PathBuf,
}

pub fn run(args: SeedArgs) -> anyhow::Result<u8> {
    let prepared = mutation_cargo_seed::provision(&ProvisionRequest {
        cache_root: &args.cache_root,
        lock_path: &args.lockfile,
        expected_lock_sha256: &args.lockfile_sha256,
        registry_directory: &args.registry_directory,
        cargo_sha256: &args.cargo_sha256,
        destination: &args.output,
    })
    .context("Cargo seed preparation")?;
    println!("{}", serde_json::to_string_pretty(&prepared)?);
    Ok(0)
}
