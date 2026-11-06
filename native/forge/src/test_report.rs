//! Native coordinator for the shared advisory Python test-closure classifier.
use crate::{
    candidate_source::Source, test_report_git::Inventory, test_report_references::References,
};
use anyhow::{ensure, Context, Result};
use clap::Args;
use conductor_native::dead_tests::{inputs::Capture, report::Analysis};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

#[derive(Args)]
pub(crate) struct TestArgs {
    /// Exit 1 only when a test has a hard missing first-party import.
    #[arg(long)]
    check: bool,
    /// Explicitly write the legacy report object inside the repository.
    /// The default operation only prints JSON; existing source inputs cannot be overwritten.
    #[arg(long)]
    json_out: Option<PathBuf>,
}

struct Snapshot {
    source: Source,
    inputs: Capture,
    references: References,
    inventory: Inventory,
    report: Value,
}

impl Snapshot {
    fn capture(root: &Path) -> Result<Self> {
        let source = Source::open(root)?;
        let inventory = Inventory::capture(&source)?;
        let mut inputs = Capture::open(source.root())?;
        let references = References::capture(&mut inputs, &inventory.tracked)?;
        let analysis = Analysis::from_captured(source.root(), &inventory.tracked, &mut inputs)?;
        let classified = analysis
            .report(
                &references.config,
                &references.notes,
                &inventory.last_commit_dates,
            )
            .map_err(anyhow::Error::msg)?;
        let mut report = serde_json::to_value(classified)?;
        report["untracked_tests"] = json!(&inventory.untracked_tests);
        Ok(Self {
            source,
            inputs,
            references,
            inventory,
            report,
        })
    }

    fn verify(&self) -> Result<()> {
        self.inventory.verify(&self.source)?;
        self.references.verify(self.source.root())?;
        self.inputs.verify()
    }

    fn envelope(&self) -> Result<Value> {
        let mut digest = Sha256::new();
        let mut files = 0;
        let mut bytes = 0u64;
        for (path, hash, length) in self.inputs.sources() {
            // Length-delimited JSON rows avoid ambiguity in unusual legal Git paths.
            digest.update(serde_json::to_vec(&(path, hash, length))?);
            digest.update(b"\n");
            files += 1;
            bytes += length;
        }
        let lookup_hash = format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(self.inputs.lookups())?)
        );
        Ok(
            json!({"schema_version":1, "authority":"native-static-analysis", "advisory":true,
            "root":self.source.root(), "head":self.inventory.head,
            "scope":"tracked Python files; first-party import and reference signals",
            "history":{"source":"HEAD non-merge changes", "shallow":self.inventory.shallow},
            "inputs":{"consistency":"cooperative recheck; not an atomic snapshot",
                "source_files":files,"source_bytes":bytes,"source_rows_sha256":format!("{:x}",digest.finalize()),
                "lookup_count":self.inputs.lookups().len(),"lookup_sha256":lookup_hash},
            "report":self.report}),
        )
    }

    fn output_path(&self, requested: &Path) -> Result<PathBuf> {
        let path = crate::mutation_write::confined_output(
            self.source.root(),
            requested,
            "test report output",
        )
        .map_err(anyhow::Error::msg)?;
        let relative = path
            .strip_prefix(self.source.root())?
            .to_str()
            .context("output path must be UTF-8")?;
        crate::candidate_source::safe_path(relative.as_bytes())?;
        ensure!(
            Path::new(relative)
                .components()
                .all(|part| part.as_os_str() != ".git")
                && !path.starts_with(self.source.repository().path())
                && !path.starts_with(self.source.common_dir()),
            "report output cannot modify Git metadata"
        );
        ensure!(!self.inventory.tracked.iter().any(|p| p == relative)
            && !self.inputs.sources().any(|(p, _, _)| p == relative)
            && !self.inputs.lookups().contains_key(relative)
            && !self.references.includes(relative)
            && path.extension().is_some_and(|suffix| suffix == "json"),
            "report output must be an untracked .json file outside captured inputs and reference trees");
        Ok(path)
    }
}

pub(crate) fn run(root: &Path, max_bytes: usize, args: TestArgs) -> Result<u8> {
    let snapshot = Snapshot::capture(root)?;
    let output = crate::analysis::encode(&snapshot.envelope()?, max_bytes)?;
    let artifact = args
        .json_out
        .as_ref()
        .map(|requested| -> Result<_> {
            let path = snapshot.output_path(requested)?;
            let text = crate::json_canon::python_pretty(snapshot.report.clone())?;
            ensure!(
                text.len() <= 4 * 1024 * 1024,
                "test report artifact exceeds 4 MiB"
            );
            Ok((path, text))
        })
        .transpose()?;
    snapshot.verify()?;
    if let Some((path, text)) = artifact {
        crate::mutation_write::atomic_write(&path, &text).map_err(anyhow::Error::msg)?;
    }
    crate::analysis::write_output(&output)?;
    let broken = snapshot.report["broken"]
        .as_array()
        .context("missing classified broken tests")?;
    Ok(u8::from(args.check && !broken.is_empty()))
}

#[cfg(test)]
#[path = "test_report_tests.rs"]
mod tests;
