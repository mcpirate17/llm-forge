//! `forge ledger`: the cost ledger's reader (design step 1 of
//! `docs/design/cost_ledger.md` section 6). Parses harness transcript JSONL
//! and hook telemetry JSONL into the shapes section 2 defines, without ever
//! retaining block text past this boundary.

pub mod agent;
pub mod audit;
pub mod calibrate;
pub mod landed;
pub mod prune;
pub mod reader;
pub mod rollup;
pub mod schema;
pub mod session_ids;
pub mod writer;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Args, ValueEnum};

use schema::InputKind;

#[derive(Clone, Copy, ValueEnum)]
pub enum KindArg {
    Transcript,
    Telemetry,
}

#[derive(Args)]
pub struct ReadArgs {
    /// Transcript or telemetry JSONL file(s) to summarize.
    pub paths: Vec<PathBuf>,

    /// One `TranscriptSummary`/`TelemetrySummary` JSON object per file
    /// (JSONL on stdout). Mutually exclusive with `--summary`; `--summary`
    /// is the default when neither is passed.
    #[arg(long)]
    pub json: bool,

    /// One human-readable `key=value` line per file (the default).
    #[arg(long)]
    pub summary: bool,

    /// Force the input kind instead of detecting it from the first parsed
    /// line of each file.
    #[arg(long, value_enum)]
    pub kind: Option<KindArg>,
}

pub fn run(args: ReadArgs) -> Result<i32> {
    if args.json && args.summary {
        anyhow::bail!("--json and --summary are mutually exclusive");
    }
    if args.paths.is_empty() {
        anyhow::bail!("forge ledger read: pass at least one path");
    }
    let as_json = args.json;
    for path in &args.paths {
        let kind = resolve_kind(path, args.kind)?;
        match kind {
            InputKind::Transcript => {
                let summary = reader::read_transcript_file(path)
                    .with_context(|| format!("reading transcript {}", path.display()))?;
                if as_json {
                    println!("{}", serde_json::to_string(&summary)?);
                } else {
                    print_transcript_summary(&summary);
                }
            }
            InputKind::Telemetry => {
                let summary = reader::read_telemetry_file(path)
                    .with_context(|| format!("reading telemetry {}", path.display()))?;
                if as_json {
                    println!("{}", serde_json::to_string(&summary)?);
                } else {
                    print_telemetry_summary(&summary);
                }
            }
        }
    }
    Ok(0)
}

fn resolve_kind(path: &Path, requested: Option<KindArg>) -> Result<InputKind> {
    match requested {
        Some(KindArg::Transcript) => Ok(InputKind::Transcript),
        Some(KindArg::Telemetry) => Ok(InputKind::Telemetry),
        None => reader::detect_kind(path),
    }
}

fn print_transcript_summary(summary: &schema::TranscriptSummary) {
    let c = &summary.chars_by_block_type;
    println!(
        "{} bytes={} lines={} turns_with_usage={} input_tokens={} output_tokens={} \
cache_read_input_tokens={} cache_creation_input_tokens={} chars_text={} chars_tool_result={} \
chars_tool_use={} chars_thinking={} chars_image={} chars_other={} skipped_lines={}",
        summary.path,
        summary.bytes,
        summary.lines,
        summary.turns_with_usage,
        summary.total_input_tokens,
        summary.total_output_tokens,
        summary.total_cache_read_input_tokens,
        summary.total_cache_creation_input_tokens,
        c.text,
        c.tool_result,
        c.tool_use,
        c.thinking,
        c.image,
        c.other,
        summary.read_stats.skipped_lines.len(),
    );
}

fn print_telemetry_summary(summary: &schema::TelemetrySummary) {
    if summary.hooks.is_empty() {
        println!(
            "{} bytes={} lines={} skipped_lines={} (no hooks)",
            summary.path,
            summary.bytes,
            summary.lines,
            summary.read_stats.skipped_lines.len()
        );
        return;
    }
    for hook in &summary.hooks {
        println!(
            "{} hook={} calls={} p50_ms={} p90_ms={} output_bytes={}",
            summary.path,
            hook.hook,
            hook.n_calls,
            fmt_ms(hook.p50_ms),
            fmt_ms(hook.p90_ms),
            hook.total_output_bytes,
        );
    }
}

fn fmt_ms(value: Option<f64>) -> String {
    match value {
        Some(ms) => format!("{ms:.3}"),
        None => "NA".to_string(),
    }
}
