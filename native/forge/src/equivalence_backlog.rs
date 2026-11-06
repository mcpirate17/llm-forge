//! Native backlog orchestration. Preview is read-only; publication and consumption are explicit.
use crate::equivalence_backlog_inputs::{absolute, Host, Input, FILE_LIMIT};
use crate::{candidate_inputs::DirectoryBinding, candidate_report_store};
use anyhow::{ensure, Context, Result};
use clap::{Args, Subcommand};
use serde_json::{json, Value};
use slop_core::{
    backlog::{fold_captured, FoldOutput},
    gate_types::Summary,
};
use std::{path::PathBuf, time::SystemTime};

const MAX_SUMMARIES: usize = 256;
const TOTAL_LIMIT: usize = 64 * 1024 * 1024;

#[derive(Args)]
pub struct EquivalenceArgs {
    #[command(subcommand)]
    action: Action,
}
#[derive(Subcommand)]
enum Action {
    /// Fold captured equivalence summaries; preview by default, without running a probe.
    Backlog(BacklogArgs),
}
#[derive(Args)]
pub struct BacklogArgs {
    /// Explicit summary files, followed by sorted configured gate-*.json artifacts.
    summaries: Vec<PathBuf>,
    /// Start of host configuration discovery; defaults to the working directory.
    #[arg(long, default_value = ".")]
    root: PathBuf,
    /// Explicit ledger output; required if not configured. Parent must exist.
    #[arg(long)]
    ledger: Option<PathBuf>,
    /// Explicit report output; required if not configured. Parent must exist.
    #[arg(long)]
    report: Option<PathBuf>,
    /// Publish both artifacts; without this flag only render a preview.
    #[arg(long, conflicts_with = "dry_run")]
    apply: bool,
    /// Explicit spelling of the default read-only preview.
    #[arg(long)]
    dry_run: bool,
    /// After publication, consume unchanged discovered artifacts inside this host.
    #[arg(long, requires = "apply")]
    consume_recorded: bool,
    /// Emit a versioned JSON result instead of Markdown or a status line.
    #[arg(long)]
    json: bool,
}

struct Artifacts {
    path: PathBuf,
    binding: Option<DirectoryBinding>,
}
impl Artifacts {
    fn capture(path: PathBuf) -> Result<(Self, Vec<PathBuf>)> {
        let binding = match std::fs::symlink_metadata(&path) {
            Ok(_) => Some(DirectoryBinding::repository(&path, None)?),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        let mut paths = Vec::new();
        if binding.is_some() {
            for (index, entry) in std::fs::read_dir(&path)?.enumerate() {
                ensure!(index < 4096, "artifact directory exceeds 4096 entries");
                let entry = entry?;
                let name = entry.file_name();
                let name = name.as_encoded_bytes();
                if name.starts_with(b"gate-") && name.ends_with(b".json") {
                    paths.push(entry.path());
                    ensure!(paths.len() <= MAX_SUMMARIES, "too many recorded summaries");
                }
            }
        }
        paths.sort();
        let out = Self { path, binding };
        out.verify()?;
        Ok((out, paths))
    }
    fn verify(&self) -> Result<()> {
        match &self.binding {
            Some(binding) => binding.verify(),
            None => Input {
                path: self.path.clone(),
                held: None,
            }
            .verify(),
        }
    }
    fn excludes_output(&self, output: &std::path::Path) -> Result<()> {
        let name = output
            .file_name()
            .context("output filename missing")?
            .as_encoded_bytes();
        ensure!(
            output.parent() != Some(self.path.as_path())
                || !name.starts_with(b"gate-")
                || !name.ends_with(b".json"),
            "output occupies the recorded-summary discovery namespace: {}",
            output.display()
        );
        Ok(())
    }
}

struct Prepared {
    host: Host,
    summaries: Vec<Input>,
    discovered: usize,
    artifacts: Option<Artifacts>,
    output_parents: Vec<DirectoryBinding>,
    previous_ledger: Input,
    previous_report: Input,
    folded: FoldOutput,
}

fn capture_summaries(paths: &[PathBuf]) -> Result<(Vec<Input>, Vec<Summary>)> {
    ensure!(
        paths.len() <= MAX_SUMMARIES,
        "backlog exceeds 256 summaries"
    );
    let mut total = 0usize;
    let mut inputs = Vec::with_capacity(paths.len());
    let mut summaries = Vec::with_capacity(paths.len());
    for path in paths {
        let remaining = TOTAL_LIMIT - total;
        ensure!(remaining > 0, "summaries exhaust 64 MiB aggregate limit");
        let (input, bytes) = Input::capture(path.clone(), FILE_LIMIT.min(remaining))?;
        let bytes = bytes.with_context(|| format!("summary missing: {}", path.display()))?;
        total += bytes.len();
        ensure!(
            total <= TOTAL_LIMIT,
            "summaries exceed 64 MiB aggregate limit"
        );
        let value =
            crate::tool_schema::parse_document_with_limit(std::str::from_utf8(&bytes)?, FILE_LIMIT)
                .with_context(|| format!("parse summary {}", path.display()))?;
        summaries.push(serde_json::from_value(value)?);
        inputs.push(input);
    }
    Ok((inputs, summaries))
}

fn prepare(args: &BacklogArgs) -> Result<Prepared> {
    let host = Host::capture(&args.root)?;
    let ledger = host.output("ledger", args.ledger.as_deref())?;
    let report = host.output("report", args.report.as_deref())?;
    ensure!(ledger != report, "ledger and report must be distinct");
    let mut paths = args
        .summaries
        .iter()
        .map(|p| absolute(p))
        .collect::<Result<Vec<_>>>()?;
    let discovered = paths.len();
    let artifacts = host
        .path("run_artifacts")?
        .map(|path| -> Result<Artifacts> {
            if args.consume_recorded {
                ensure!(
                    path.starts_with(&host.root),
                    "consumption of configured artifacts outside the host is refused"
                );
            }
            let (artifacts, recorded) = Artifacts::capture(path)?;
            paths.extend(recorded);
            Ok(artifacts)
        })
        .transpose()?;
    if let Some(artifacts) = &artifacts {
        artifacts.excludes_output(&ledger)?;
        artifacts.excludes_output(&report)?;
    }
    for input in paths.iter().chain(host.inputs.iter().map(|v| &v.path)) {
        ensure!(
            input != &ledger && input != &report,
            "output aliases input {}",
            input.display()
        );
    }
    let output_parents = [&ledger, &report]
        .iter()
        .map(|path| {
            DirectoryBinding::repository(path.parent().context("output parent missing")?, None)
                .with_context(|| {
                    format!(
                        "output parent must exist and be safely owned: {}",
                        path.display()
                    )
                })
        })
        .collect::<Result<Vec<_>>>()?;
    let (previous_ledger, prior) = Input::capture(ledger, FILE_LIMIT)?;
    let (previous_report, _) = Input::capture(report, FILE_LIMIT)?;
    let (summaries, typed) = capture_summaries(&paths)?;
    let folded = fold_captured(&typed, prior.as_deref(), SystemTime::now())?;
    ensure!(
        folded.ledger.len() <= FILE_LIMIT && folded.report.len() <= FILE_LIMIT,
        "backlog output exceeds 16 MiB"
    );
    let prepared = Prepared {
        host,
        summaries,
        discovered,
        artifacts,
        output_parents,
        previous_ledger,
        previous_report,
        folded,
    };
    prepared.verify()?;
    Ok(prepared)
}

impl Prepared {
    fn verify_inputs(&self) -> Result<()> {
        self.host.verify()?;
        for parent in &self.output_parents {
            parent.verify()?;
        }
        if let Some(artifacts) = &self.artifacts {
            artifacts.verify()?;
        }
        for input in &self.summaries {
            input.verify()?;
        }
        Ok(())
    }
    fn verify(&self) -> Result<()> {
        self.verify_inputs()?;
        self.previous_ledger.verify()?;
        self.previous_report.verify()
    }
    fn verify_consumption(&self) -> Result<()> {
        if let Some(artifacts) = &self.artifacts {
            ensure!(
                artifacts.path.starts_with(&self.host.root),
                "consumption of configured artifacts outside the host is refused"
            );
            if let Some(binding) = &artifacts.binding {
                binding.verify_mutation()?;
            }
        }
        for input in &self.summaries[self.discovered..] {
            input
                .held
                .as_ref()
                .context("captured artifact missing")?
                .verify_mutation()?;
        }
        Ok(())
    }
    fn verify_publication(&self, consume: bool) -> Result<()> {
        self.verify()?;
        for parent in &self.output_parents {
            parent.verify_mutation()?;
        }
        for input in [&self.previous_ledger, &self.previous_report] {
            if let Some(held) = &input.held {
                held.verify_mutation()?;
            }
        }
        if consume {
            self.verify_consumption()?;
        }
        Ok(())
    }
    fn consume(&self) -> Result<Vec<PathBuf>> {
        self.verify_inputs()
            .context("outputs published; input recheck failed; no artifacts consumed")?;
        self.verify_consumption()
            .context("outputs published; consumption admission failed; no artifacts consumed")?;
        let recorded = &self.summaries[self.discovered..];
        let mut consumed = Vec::new();
        for (index, input) in recorded.iter().enumerate() {
            let held = input.held.as_ref().context("captured artifact missing")?;
            held.consume().with_context(|| {
                format!(
                    "outputs published; consumed={consumed:?}; retained={:?}",
                    recorded[index..]
                        .iter()
                        .map(|i| &i.path)
                        .collect::<Vec<_>>()
                )
            })?;
            consumed.push(input.path.clone());
            held.sync_parent().with_context(|| format!(
                "outputs published; removed={consumed:?}; removal durability uncertain; retained={:?}",
                recorded[index + 1..].iter().map(|i| &i.path).collect::<Vec<_>>()))?;
        }
        Ok(consumed)
    }
    fn finish(&self, apply: bool, consume: bool) -> Result<Value> {
        ensure!(!consume || apply, "consumption requires publication");
        self.verify()?;
        let mut consumed = Vec::new();
        if apply {
            self.verify_publication(consume)?;
            candidate_report_store::publish_pair_checked(
                (&self.previous_report.path, self.folded.report.as_bytes()),
                (&self.previous_ledger.path, self.folded.ledger.as_bytes()),
                || self.verify_publication(consume),
            )?;
            if consume {
                consumed = self.consume()?;
            }
        }
        let mut output = json!({"schema_version":1, "status":if apply {"APPLIED"} else {"PREVIEW"},
            "ledger":self.previous_ledger.path, "report":self.previous_report.path,
            "summaries":self.summaries.len(), "recorded":self.summaries.len()-self.discovered,
            "consumed":consumed, "findings":self.folded.findings, "items":self.folded.items,
            "new":self.folded.new, "carried":self.folded.carried, "fixed":self.folded.fixed});
        if !apply {
            output["preview"] = json!(self.folded.report);
        }
        Ok(output)
    }
}

pub fn run(args: EquivalenceArgs) -> Result<u8> {
    let Action::Backlog(args) = args.action;
    let prepared = prepare(&args)?;
    let result = prepared.finish(args.apply, args.consume_recorded)?;
    if args.json {
        println!("{}", serde_json::to_string(&result)?);
    } else if args.apply {
        println!(
            "{}",
            prepared.folded.status(
                &prepared.previous_report.path,
                &prepared.previous_ledger.path
            )
        );
    } else {
        println!("{}", prepared.folded.report);
    }
    Ok(0)
}

#[cfg(test)]
#[path = "equivalence_backlog_tests.rs"]
mod tests;
