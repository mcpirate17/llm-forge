//! Export only the coordinator's newly sealed report; never reread an old receipt.
use crate::{candidate_receipt, candidate_report, candidate_report_store};
use anyhow::{ensure, Context, Result};
use clap::Args;
use serde_json::Value;
use std::{
    collections::BTreeSet,
    path::{Component, Path, PathBuf},
};
#[derive(Args)]
pub(super) struct Outputs {
    /// Atomically publish the sealed receipt under an existing real directory.
    #[arg(long, requires = "execute")]
    receipt: Option<PathBuf>,
    /// Export SARIF from this execution's sealed receipt, including failed reviews.
    #[arg(long, requires = "execute")]
    sarif_out: Option<PathBuf>,
    /// Export JUnit from this execution's sealed receipt, including failed reviews.
    #[arg(long, requires = "execute")]
    junit_out: Option<PathBuf>,
}
impl Outputs {
    pub(super) fn is_empty(&self) -> bool {
        self.receipt.is_none() && self.sarif_out.is_none() && self.junit_out.is_none()
    }
    pub fn validate(&self) -> Result<()> {
        let mut destinations = BTreeSet::new();
        for path in [&self.receipt, &self.sarif_out, &self.junit_out]
            .into_iter()
            .flatten()
        {
            let name = path.file_name().context("review output must name a file")?;
            ensure!(
                path.components().all(|p| matches!(
                    p,
                    Component::RootDir | Component::Normal(_) | Component::CurDir
                )),
                "review output path must not contain traversal"
            );
            let parent = path
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new("."));
            let resolved = parent
                .canonicalize()
                .context("review output parent must already exist")?
                .join(name);
            ensure!(
                destinations.insert(resolved),
                "review output paths must be distinct"
            );
        }
        Ok(())
    }
    pub fn publish(&self, report: &Value) -> Result<()> {
        self.validate()?;
        if self.receipt.is_none() && self.sarif_out.is_none() && self.junit_out.is_none() {
            return Ok(());
        }
        let raw = serde_json::to_vec(report)?;
        let receipt = candidate_receipt::parse_and_verify(&raw)?;
        // Render every requested format before publishing any file. Publication
        // stays atomic per destination; multiple files are not a transaction.
        let mut exports = Vec::new();
        if let Some(path) = &self.receipt {
            exports.push((path, raw));
        }
        for (path, format) in [
            (&self.sarif_out, candidate_report::Format::Sarif),
            (&self.junit_out, candidate_report::Format::Junit),
        ] {
            if let Some(path) = path {
                exports.push((path, candidate_report::render(&receipt, format)?));
            }
        }
        for (path, bytes) in exports {
            candidate_report_store::publish(path, &bytes)?;
        }
        Ok(())
    }
}
