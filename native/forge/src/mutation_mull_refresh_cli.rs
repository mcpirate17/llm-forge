//! Refresh one explicitly selected Mull manifest; never run an engine or issue evidence.
use crate::candidate_inputs::{self, BoundFile, DirectoryBinding};
use crate::candidate_report_store;
use crate::mutation_campaign::sha256_hex;
use anyhow::{ensure, Context, Result};
use clap::Args;
use conductor_native::{mutation_mull_refresh, mutation_snapshot_inputs};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

const MANIFEST_LIMIT: usize = 1024 * 1024;
const SOURCE_LIMIT: usize = 32 * 1024 * 1024;

#[derive(Args)]
pub struct RefreshArgs {
    /// Existing manifest file inside the repository; registration is not inferred.
    pub campaign: PathBuf,
    /// Repository root; defaults to the nearest ancestor containing .git.
    #[arg(long)]
    pub repo_root: Option<PathBuf>,
    /// Publish the refreshed pins. Without this flag, only preview the change.
    #[arg(long)]
    pub apply: bool,
    /// Include the complete proposed manifest in the JSON report.
    #[arg(long)]
    pub verbose: bool,
}

struct Prepared {
    root: DirectoryBinding,
    manifest: BoundFile,
    source: BoundFile,
    driver: BoundFile,
    destination: PathBuf,
    bytes: Vec<u8>,
    proposed: Value,
    report: Value,
    changed: bool,
}

fn relative_manifest<'a>(root: &Path, path: &'a Path) -> Result<&'a str> {
    let relative = if path.is_absolute() {
        path.strip_prefix(root)
            .context("campaign must be inside the repository")?
    } else {
        path
    };
    let text = relative.to_str().context("campaign path must be UTF-8")?;
    mutation_snapshot_inputs::validate_path(text).map_err(anyhow::Error::msg)?;
    ensure!(
        !text.bytes().any(|b| b.is_ascii_control()),
        "invalid campaign path"
    );
    Ok(text)
}

fn prepare(repo_root: &Path, campaign: &Path) -> Result<Prepared> {
    let root_path = std::path::absolute(repo_root)?;
    let root = DirectoryBinding::repository(&root_path, None)?;
    let relative = relative_manifest(&root_path, campaign)?;
    let destination = root_path.join(relative);
    let (manifest, old_bytes) = candidate_inputs::repository_live(&destination, MANIFEST_LIMIT)?;
    let existing = crate::tool_schema::parse_document(std::str::from_utf8(&old_bytes)?)?;
    let (source_path, driver_path) =
        mutation_mull_refresh::inputs(&existing).map_err(anyhow::Error::msg)?;
    ensure!(
        relative != source_path && relative != driver_path,
        "campaign aliases a selected input"
    );
    let (source, source_bytes) =
        candidate_inputs::repository_live(&root_path.join(&source_path), SOURCE_LIMIT)?;
    let (driver, driver_bytes) =
        candidate_inputs::repository_live(&root_path.join(&driver_path), SOURCE_LIMIT)?;
    let source_hash = sha256_hex(&source_bytes);
    let driver_hash = sha256_hex(&driver_bytes);
    let proposed = mutation_mull_refresh::refresh(&existing, &source_hash, &driver_hash)
        .map_err(anyhow::Error::msg)?;
    let changed = proposed != existing;
    let old_hash = sha256_hex(&old_bytes);
    let bytes = if changed {
        let mut rendered = serde_json::to_vec_pretty(&proposed)?;
        rendered.push(b'\n');
        rendered
    } else {
        old_bytes
    };
    ensure!(
        bytes.len() <= MANIFEST_LIMIT,
        "refreshed manifest exceeds 1 MiB"
    );
    let report = json!({
        "schema_version": 1, "status": "PREVIEW", "campaign": relative,
        "campaign_id": existing["campaign_id"], "changed": changed,
        "old_manifest_sha256": old_hash, "proposed_manifest_sha256": sha256_hex(&bytes),
        "pins": [
            {"path": source_path, "old_sha256": existing["source_sha256"][&source_path], "new_sha256": source_hash},
            {"path": driver_path, "old_sha256": existing["test_sha256"][&driver_path], "new_sha256": driver_hash}
        ],
        "evidence_status": "NOT_EVALUATED"
    });
    let prepared = Prepared {
        root,
        manifest,
        source,
        driver,
        destination,
        bytes,
        proposed,
        report,
        changed,
    };
    prepared.verify()?;
    Ok(prepared)
}

impl Prepared {
    fn verify(&self) -> Result<()> {
        self.root.verify()?;
        self.source.verify()?;
        self.driver.verify()?;
        self.manifest.verify()?;
        self.root.verify()
    }

    fn finish(mut self, apply: bool, verbose: bool) -> Result<Value> {
        self.verify()?;
        if apply && self.changed {
            self.manifest.verify_mutation()?;
            candidate_report_store::publish_checked(&self.destination, &self.bytes, || {
                self.verify()?;
                self.manifest.verify_mutation()
            })?;
        }
        if apply {
            self.report["status"] = json!(if self.changed { "APPLIED" } else { "UNCHANGED" });
        }
        if verbose {
            self.report["manifest"] = self.proposed;
        }
        Ok(self.report)
    }
}

pub fn run(repo_root: &Path, args: &RefreshArgs) -> Result<u8> {
    let report = prepare(repo_root, &args.campaign)?.finish(args.apply, args.verbose)?;
    println!("{}", serde_json::to_string(&report)?);
    Ok(0)
}

#[cfg(test)]
#[path = "mutation_mull_refresh_cli_tests.rs"]
mod tests;
