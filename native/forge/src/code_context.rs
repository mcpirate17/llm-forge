//! Source-preserving signature context with bounded native graph relationships.
//! This is context data, never executable source or proof of graph completeness.
use anyhow::{ensure, Context, Result};
use clap::Args;
use conductor_native::graph_context::{
    dispatch,
    files::{SourceRoot, MAX_SOURCE_BYTES},
    GraphRelationship,
};
use serde::{Deserialize, Serialize};
use std::io::{self, Write};
use std::path::Path;

pub const MAX_OUTPUT_BYTES: usize = 4 * 1024 * 1024;
const PYTHON_FORMAT: &str = "source-preserving-v1";
const RUST_FORMAT: &str = "rust-signatures-v1";
#[derive(Args)]
pub struct ContextArgs {
    /// Python or Rust source path inside the pinned --root checkout.
    file: String,
    /// Bare or class-qualified definition selector.
    #[arg(long)]
    symbol: Option<String>,
    /// Extract source signatures without opening a graph database.
    #[arg(long)]
    no_graph: bool,
    /// Emit structured context instead of Markdown.
    #[arg(long)]
    json: bool,
    /// Refuse a continuation after the requested source snapshot changes.
    #[arg(long)]
    expected_hash: Option<String>,
}
#[derive(Debug, Serialize)]
pub struct ContextSummary {
    pub schema_version: u8,
    pub file_path: String,
    pub language: String,
    pub source_sha256: String,
    pub format: String,
    pub skeleton: String,
    pub symbols: Vec<String>,
    pub callers: Vec<GraphRelationship>,
    pub callees: Vec<GraphRelationship>,
    pub graph_status: String,
}
#[derive(Deserialize)]
struct Skeleton {
    skeleton: String,
    symbols: Vec<String>,
    language: String,
    #[serde(default)]
    format: Option<String>,
}
#[derive(Deserialize)]
struct Relationships {
    callers: Vec<GraphRelationship>,
    callees: Vec<GraphRelationship>,
    status: String,
}
fn valid_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn unchanged(root: &SourceRoot, file: &str, expected: &str) -> Result<()> {
    ensure!(
        root.hash(file)?.as_deref() == Some(expected),
        "requested source changed during context extraction; retry"
    );
    Ok(())
}
struct CappedBuffer(Vec<u8>);
impl Write for CappedBuffer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > MAX_OUTPUT_BYTES.saturating_sub(self.0.len()) {
            return Err(io::Error::other(
                "code context exceeds 4 MiB output byte limit",
            ));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn bounded_json(value: &impl Serialize) -> Result<Vec<u8>> {
    let mut writer = CappedBuffer(Vec::new());
    serde_json::to_writer(&mut writer, value).context("serialize bounded code context")?;
    Ok(writer.0)
}
/// Python retains source layout/quotes/decorators. Rust retains signature and
/// attribute text with reconstructed scope wrappers and ellipsis bodies; it
/// does not promise original whitespace or full-source preservation. Python
/// class selection includes methods; module docs are retained intentionally.
/// SourceRoot binds text and digest; source is rechecked after both extraction
/// and relationships. Later unconfined same-UID changes remain possible.
pub fn get_file_context(
    root: &Path,
    file: &str,
    symbol: Option<&str>,
    with_graph: bool,
    expected_hash: Option<&str>,
) -> Result<ContextSummary> {
    ensure!(
        symbol.is_none_or(|s| s.len() <= 512 && !s.chars().any(char::is_control)),
        "context symbol must be at most 512 bytes without control characters"
    );
    if let Some(hash) = expected_hash {
        ensure!(
            valid_hash(hash),
            "expected source hash must be 64 lowercase hexadecimal characters"
        );
    }
    let root = SourceRoot::open(root)?;
    let relative = root.normalize(file)?;
    let file = relative.to_str().context("source path must be UTF-8")?;
    let (language, operation, format) = match relative.extension().and_then(|v| v.to_str()) {
        Some("py") => ("python", "python_skeleton", PYTHON_FORMAT),
        Some("rs") => ("rust", "rust_skeleton", RUST_FORMAT),
        _ => anyhow::bail!("unsupported source language; code context accepts .py and .rs"),
    };
    let source = root
        .read_text(file, MAX_SOURCE_BYTES)?
        .context("source file not found")?;
    if let Some(expected) = expected_hash {
        ensure!(
            source.sha256 == expected,
            "source hash changed from expected snapshot"
        );
    }
    let symbol = symbol.filter(|s| !s.is_empty());
    let projection = dispatch(
        operation,
        &serde_json::json!({"source":source.text,"target_symbol":symbol}),
    )
    .map_err(anyhow::Error::msg)?;
    bounded_json(&projection)?;
    let projection: Skeleton =
        serde_json::from_value(projection).context("invalid native skeleton projection")?;
    ensure!(
        projection.language == language,
        "native skeleton language mismatch"
    );
    ensure!(
        projection.format.as_deref() == Some(format),
        "native skeleton format mismatch"
    );
    unchanged(&root, file, &source.sha256)?;
    let relationships = if with_graph {
        let value = dispatch(
            "relationships",
            &serde_json::json!({"repo":root.host(),
            "file_path":file,"target_symbol":symbol}),
        )
        .map_err(anyhow::Error::msg)?;
        bounded_json(&value)?;
        serde_json::from_value::<Relationships>(value)
            .context("invalid native graph relationships")?
    } else {
        Relationships {
            callers: Vec::new(),
            callees: Vec::new(),
            status: "disabled".into(),
        }
    };
    unchanged(&root, file, &source.sha256)?;
    let summary = ContextSummary {
        schema_version: 1,
        file_path: file.to_owned(),
        language: language.into(),
        source_sha256: source.sha256,
        format: format.into(),
        skeleton: projection.skeleton,
        symbols: projection.symbols,
        callers: relationships.callers,
        callees: relationships.callees,
        graph_status: relationships.status,
    };
    bounded_json(&summary)?;
    Ok(summary)
}
/// Reuse core role grouping and append snapshot/format/status binding metadata.
pub fn format_markdown(summary: &ContextSummary) -> Result<String> {
    bounded_json(summary)?;
    let value = serde_json::to_value(summary)?;
    let rendered = dispatch("markdown", &value).map_err(anyhow::Error::msg)?;
    let markdown = rendered
        .as_str()
        .context("native Markdown renderer did not return text")?;
    let mut output = CappedBuffer(Vec::new());
    output.write_all(markdown.trim_end().as_bytes())?;
    write!(
        output,
        "\n\n*Source SHA-256: `{}`; format: `{}`; graph status: {}.*\n",
        summary.source_sha256, summary.format, summary.graph_status
    )?;
    String::from_utf8(output.0).context("native Markdown renderer returned invalid UTF-8")
}
pub fn run(root: &Path, args: ContextArgs) -> Result<u8> {
    let summary = get_file_context(
        root,
        &args.file,
        args.symbol.as_deref(),
        !args.no_graph,
        args.expected_hash.as_deref(),
    )?;
    let output = if args.json {
        let mut bytes = bounded_json(&summary)?;
        bytes.push(b'\n');
        bytes
    } else {
        format_markdown(&summary)?.into_bytes()
    };
    ensure!(
        output.len() <= MAX_OUTPUT_BYTES,
        "code context exceeds 4 MiB output byte limit"
    );
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(&output)?;
    stdout.flush()?;
    Ok(0)
}
