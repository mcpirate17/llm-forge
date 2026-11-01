//! Standalone native telemetry recording, byte-preserving hook tee, and summary.
use anyhow::{ensure, Context, Result};
use clap::{Args, Subcommand};
use conductor_native::context_telemetry_aggregate::{format_summary, summarize_bytes};
use serde_json::Value;
use std::io::{Read, Write};
use std::path::PathBuf;

const MAX_STDIN_BYTES: usize = 1024 * 1024;
const MAX_FILES: usize = 32;
const MAX_TOTAL_BYTES: usize = 64 * 1024 * 1024;

#[derive(Args)]
pub struct TelemetryArgs {
    #[command(subcommand)]
    action: Option<Action>,
}

#[derive(Subcommand)]
enum Action {
    /// Record one PostToolUse payload from bounded stdin (also the default).
    Record,
    /// Echo bounded stdin exactly, then record its injected-context metadata.
    HookContext {
        #[arg(long, value_parser = hook_label)]
        hook: String,
        #[arg(long, default_value = "", value_parser = label)]
        event: String,
        #[arg(long, default_value = "", value_parser = label)]
        category: String,
    },
    /// Aggregate bounded regular JSONL files; defaults to the telemetry sink.
    Summary {
        paths: Vec<PathBuf>,
        #[arg(long, default_value_t = 8000, value_parser = clap::value_parser!(u64).range(0..=1048576))]
        bound_bytes: u64,
        #[arg(long)]
        json: bool,
    },
}

fn label(value: &str) -> std::result::Result<String, String> {
    if value.chars().count() > 100 || value.chars().any(char::is_control) {
        return Err("telemetry label must be at most 100 characters without controls".into());
    }
    Ok(value.into())
}

fn hook_label(value: &str) -> std::result::Result<String, String> {
    if value.is_empty() {
        return Err("telemetry hook must not be empty".into());
    }
    label(value)
}

fn stdin_bytes() -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    std::io::stdin()
        .lock()
        .take(MAX_STDIN_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= MAX_STDIN_BYTES,
        "telemetry stdin exceeds 1 MiB"
    );
    Ok(bytes)
}

fn payload(bytes: &[u8]) -> Result<Value> {
    let value = crate::tool_schema::parse_document(std::str::from_utf8(bytes)?)
        .context("invalid telemetry JSON input")?;
    ensure!(value.is_object(), "telemetry payload must be a JSON object");
    Ok(value)
}

fn hook_context(hook: &str, event: &str, category: &str) -> Result<()> {
    let bytes = stdin_bytes()?;
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(&bytes)?;
    stdout.flush()?;
    let input = payload(&bytes)?;
    let encoded = crate::context_telemetry::hook_context_line(hook, &input, event, "");
    let mut record: Value = serde_json::from_slice(&encoded)?;
    if record["output_bytes"].as_u64().unwrap_or(0) == 0 {
        return Ok(());
    }
    if !category.is_empty() {
        record["category"] = Value::String(category.into());
    }
    let mut encoded = serde_json::to_vec(&record)?;
    encoded.push(b'\n');
    crate::context_telemetry::record_encoded_result(
        &crate::context_telemetry::telemetry_path()?,
        &encoded,
    )
    .context("write telemetry hook event")?;
    Ok(())
}

fn summary(mut paths: Vec<PathBuf>, bound_bytes: u64, json: bool) -> Result<()> {
    if paths.is_empty() {
        paths.push(crate::context_telemetry::telemetry_path()?);
    }
    ensure!(
        paths.len() <= MAX_FILES,
        "telemetry summary exceeds 32 files"
    );
    let mut total = 0;
    let mut inputs = Vec::with_capacity(paths.len());
    for path in paths {
        let maximum =
            (crate::context_telemetry::MAX_LOG_BYTES as usize).min(MAX_TOTAL_BYTES - total);
        let bytes = forge::bounded_file::read(&path, maximum)
            .with_context(|| format!("read bounded telemetry log {}", path.display()))?;
        total += bytes.len();
        inputs.push((path.to_string_lossy().into_owned(), bytes));
    }
    let borrowed = inputs
        .iter()
        .map(|(path, bytes)| (path.as_str(), bytes.as_slice()))
        .collect::<Vec<_>>();
    let report = summarize_bytes(&borrowed, i128::from(bound_bytes));
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("{}", format_summary(&report).map_err(anyhow::Error::msg)?);
    }
    Ok(())
}

pub fn run(args: TelemetryArgs) -> Result<u8> {
    match args.action.unwrap_or(Action::Record) {
        Action::Record => {
            let input = payload(&stdin_bytes()?)?;
            crate::context_telemetry::record_encoded_result(
                &crate::context_telemetry::telemetry_path()?,
                &crate::context_telemetry::event_line(&input),
            )
            .context("write telemetry event")?;
        }
        Action::HookContext {
            hook,
            event,
            category,
        } => hook_context(&hook, &event, &category)?,
        Action::Summary {
            paths,
            bound_bytes,
            json,
        } => summary(paths, bound_bytes, json)?,
    }
    Ok(0)
}
