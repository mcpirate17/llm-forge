//! Thin native CLI for an operator-selected installed runner package.
use anyhow::Result;
use clap::Args;
use std::path::PathBuf;

#[derive(Args)]
pub struct BundleArgs {
    #[arg(long, default_value = ".")]
    repo: PathBuf,
    /// Canonical absolute installed root containing the exact runner inventory.
    #[arg(long)]
    package_root: PathBuf,
    /// New manifest under an existing owned mode-0700 parent directory.
    #[arg(long)]
    output: PathBuf,
    /// Optional installed lineage document beneath package-root.
    #[arg(long)]
    lineage: Option<PathBuf>,
    /// Optional installed native ELF shared-library probe beneath package-root; never loaded here.
    #[arg(long)]
    external_invariant_probe: Option<PathBuf>,
    /// Existing canonical snapshot/scratch roots that must not overlap inputs/output.
    #[arg(long = "exclude-root")]
    exclude_roots: Vec<PathBuf>,
}

pub fn run(args: BundleArgs) -> Result<()> {
    let generated =
        crate::candidate_runner_bundle::generate(crate::candidate_runner_bundle::Request {
            repo: &args.repo,
            package_root: &args.package_root,
            output: &args.output,
            lineage: args.lineage.as_deref(),
            external_invariant_probe: args.external_invariant_probe.as_deref(),
            exclude_roots: &args.exclude_roots,
        })?;
    println!("{}  {}", generated.manifest_sha256, args.output.display());
    Ok(())
}
