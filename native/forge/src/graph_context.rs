//! Bounded native graph context for a host checkout and concrete code references.
//!
//! Forge writes its own `.forge/graph.db` via `graph index`; the external
//! `.code-review-graph/graph.db` remains a read-only compatibility source.

#[path = "graph_index.rs"]
mod indexer;
#[path = "graph_context_store.rs"]
mod store;

use anyhow::{bail, ensure, Context, Result};
use clap::{Args, Subcommand};
use regex::Regex;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use store::{GraphStore, Relationship, Symbol};

const MAX_SOURCE_BYTES: usize = 1 << 18;
const MAX_SCAN_BYTES: usize = 16_384;
const MAX_REF_OUTPUT_BYTES: usize = 4_000;
const MAX_CONTEXT_OUTPUT_BYTES: usize = 32_768;
const MAX_PATH_BYTES: usize = 1_024;

#[derive(Args)]
pub struct GraphArgs {
    /// Host checkout containing source and an optional graph database.
    #[arg(long, default_value = ".", global = true)]
    host: PathBuf,
    /// Explicit graph database path (relative to host or absolute).
    #[arg(long, global = true)]
    db: Option<PathBuf>,
    #[command(subcommand)]
    action: GraphCommand,
}

#[derive(Subcommand)]
enum GraphCommand {
    /// Build a syntax-only Python/Rust graph in Forge-owned SQLite.
    Index,
    /// Bounded source and indexed callers/callees for one file or symbol.
    Context(ContextArgs),
    /// Discover concrete code references in a bounded message prefix.
    Refs(RefsArgs),
}

#[derive(Args)]
struct ContextArgs {
    /// Existing file inside the host checkout, relative or absolute.
    file: String,
    /// Exact indexed symbol name to narrow the source and relationships.
    #[arg(long)]
    symbol: Option<String>,
    /// Maximum edges per direction (1..50).
    #[arg(long, default_value_t = 12)]
    max_edges: usize,
    /// Maximum serialized JSON bytes (512..32768).
    #[arg(long, default_value_t = 8_192)]
    max_bytes: usize,
}

#[derive(Args)]
struct RefsArgs {
    /// Bounded message body or other text containing code paths.
    #[arg(long, conflicts_with = "body_file")]
    text: Option<String>,
    /// Read a bounded prefix of an existing UTF-8 message body file.
    #[arg(long)]
    body_file: Option<PathBuf>,
    /// Maximum distinct contained code references (1..4).
    #[arg(long, default_value_t = 2)]
    max_refs: usize,
    /// Maximum text bytes scanned for references (256..16384).
    #[arg(long, default_value_t = 4_096)]
    scan_bytes: usize,
    /// Maximum serialized JSON bytes (256..4000).
    #[arg(long, default_value_t = 1_200)]
    max_bytes: usize,
}

#[derive(Serialize)]
struct SourceExcerpt {
    line_start: usize,
    line_end: usize,
    text: String,
    truncated: bool,
}

#[derive(Serialize)]
struct ContextOutput {
    schema_version: u8,
    file_path: String,
    symbol: Option<String>,
    graph_status: String,
    source: Option<SourceExcerpt>,
    symbols: Vec<Symbol>,
    callers: Vec<Relationship>,
    callees: Vec<Relationship>,
    omitted_symbols_at_least: usize,
    omitted_relationships_at_least: usize,
    truncated: bool,
}

#[derive(Serialize)]
struct CompactContext {
    path: String,
    symbol: Option<String>,
    source: String,
    callers: Vec<String>,
    callees: Vec<String>,
    graph_status: String,
}

#[derive(Serialize)]
struct RefsOutput {
    schema_version: u8,
    authority: &'static str,
    contexts: Vec<CompactContext>,
    omitted_refs: usize,
    rejected_refs: usize,
    input_truncated: bool,
    truncated: bool,
}

struct SourceFile {
    relative: String,
    text: String,
    digest: String,
}

pub fn run(args: GraphArgs) -> Result<u8> {
    let GraphArgs { host, db, action } = args;
    let host = host.canonicalize().context("host directory not found")?;
    ensure!(host.is_dir(), "host is not a directory: {}", host.display());
    match action {
        GraphCommand::Index => {
            let report = indexer::index(&host, db.as_deref())?;
            println!("{}", serde_json::to_string(&report)?);
        }
        GraphCommand::Context(args) => run_context(&host, db.as_deref(), args)?,
        GraphCommand::Refs(args) => run_refs(&host, db.as_deref(), args)?,
    }
    Ok(0)
}

fn run_context(host: &Path, db: Option<&Path>, args: ContextArgs) -> Result<()> {
    ensure!(
        (1..=50).contains(&args.max_edges),
        "--max-edges must be between 1 and 50"
    );
    ensure!(
        (512..=MAX_CONTEXT_OUTPUT_BYTES).contains(&args.max_bytes),
        "--max-bytes must be between 512 and {MAX_CONTEXT_OUTPUT_BYTES}"
    );
    let symbol = validate_symbol(args.symbol.as_deref())?;
    let file = read_source(host, &args.file)?;
    let store = GraphStore::open(host, db)?;
    let mut output = project_context(&store, host, &file, symbol, args.max_edges)?;
    fit_context(&mut output, args.max_bytes)?;
    println!("{}", serde_json::to_string(&output)?);
    Ok(())
}

fn run_refs(host: &Path, db: Option<&Path>, args: RefsArgs) -> Result<()> {
    ensure!(
        (1..=4).contains(&args.max_refs),
        "--max-refs must be between 1 and 4"
    );
    ensure!(
        (256..=MAX_SCAN_BYTES).contains(&args.scan_bytes),
        "--scan-bytes must be between 256 and {MAX_SCAN_BYTES}"
    );
    ensure!(
        (256..=MAX_REF_OUTPUT_BYTES).contains(&args.max_bytes),
        "--max-bytes must be between 256 and {MAX_REF_OUTPUT_BYTES}"
    );
    let (text, input_truncated) = read_message_text(args.text, args.body_file, args.scan_bytes)?;
    let mut output = RefsOutput {
        schema_version: 1,
        authority: "bounded-a2a-code-context",
        contexts: Vec::new(),
        omitted_refs: 0,
        rejected_refs: 0,
        input_truncated,
        truncated: false,
    };
    let mut seen_raw = HashSet::new();
    let mut seen_canonical = HashSet::new();
    let mut store = None;
    for captures in ref_re().captures_iter(&text) {
        let path = captures.name("path").unwrap().as_str();
        let symbol = captures.name("symbol").map(|m| m.as_str());
        if !seen_raw.insert((path.to_string(), symbol.map(str::to_string))) {
            continue;
        }
        let (canonical, relative) = match resolve_code_path(host, path) {
            Ok(found) => found,
            Err(_) => {
                output.rejected_refs += 1;
                continue;
            }
        };
        if !seen_canonical.insert((relative.clone(), symbol.map(str::to_string))) {
            continue;
        }
        if output.contexts.len() >= args.max_refs {
            output.omitted_refs += 1;
            continue;
        }
        let file = match read_source_resolved(&canonical, relative) {
            Ok(file) => file,
            Err(_) => {
                output.rejected_refs += 1;
                continue;
            }
        };
        if store.is_none() {
            store = Some(GraphStore::open(host, db)?);
        }
        let context = project_context(store.as_ref().unwrap(), host, &file, symbol, 3)?;
        let compact = compact_context(context);
        output.contexts.push(compact);
    }
    fit_refs(&mut output, args.max_bytes)?;
    println!("{}", serde_json::to_string(&output)?);
    Ok(())
}

fn validate_symbol(symbol: Option<&str>) -> Result<Option<&str>> {
    if let Some(symbol) = symbol {
        ensure!(
            !symbol.is_empty() && symbol.len() <= 256,
            "invalid --symbol length"
        );
        ensure!(
            symbol
                .chars()
                .all(|c| c.is_alphanumeric() || c == '_' || c == '.'),
            "--symbol must contain only letters, digits, underscores or dots"
        );
    }
    Ok(symbol)
}

fn read_source(host: &Path, raw: &str) -> Result<SourceFile> {
    let (canonical, relative) = resolve_code_path(host, raw)?;
    read_source_resolved(&canonical, relative)
}

fn resolve_code_path(host: &Path, raw: &str) -> Result<(PathBuf, String)> {
    ensure!(
        !raw.is_empty() && raw.len() <= MAX_PATH_BYTES && !raw.contains('\0'),
        "invalid code path (empty, NUL, or over {MAX_PATH_BYTES} bytes)"
    );
    let path = Path::new(raw);
    let candidate = if path.is_absolute() {
        path.to_path_buf()
    } else {
        host.join(path)
    };
    let canonical = candidate
        .canonicalize()
        .with_context(|| format!("code file not found: {raw}"))?;
    let relative = canonical
        .strip_prefix(host)
        .with_context(|| format!("code path escapes host: {raw}"))?;
    ensure!(
        relative
            .components()
            .all(|c| { !c.as_os_str().to_string_lossy().starts_with('.') }),
        "hidden code path is not allowed: {raw}"
    );
    let relative = relative.to_string_lossy().replace('\\', "/");
    Ok((canonical, relative))
}

fn read_source_resolved(canonical: &Path, relative: String) -> Result<SourceFile> {
    let metadata = canonical
        .metadata()
        .with_context(|| format!("stat code file: {}", canonical.display()))?;
    ensure!(metadata.is_file(), "code path is not a file: {relative}");
    ensure!(
        metadata.len() <= MAX_SOURCE_BYTES as u64,
        "code file exceeds {MAX_SOURCE_BYTES} bytes: {relative}"
    );
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    File::open(canonical)?
        .take((MAX_SOURCE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= MAX_SOURCE_BYTES,
        "code file grew beyond {MAX_SOURCE_BYTES} bytes: {relative}"
    );
    ensure!(
        !bytes.contains(&0),
        "code file contains NUL bytes: {relative}"
    );
    let digest = format!("{:x}", Sha256::digest(&bytes));
    let text =
        String::from_utf8(bytes).with_context(|| format!("code file is not UTF-8: {relative}"))?;
    Ok(SourceFile {
        relative,
        text,
        digest,
    })
}

fn project_context(
    store: &GraphStore,
    host: &Path,
    file: &SourceFile,
    symbol: Option<&str>,
    max_edges: usize,
) -> Result<ContextOutput> {
    let mut projection = store.project(host, &file.relative, symbol, max_edges)?;
    let valid_hashes: Vec<&str> = projection
        .indexed_hashes
        .iter()
        .map(String::as_str)
        .filter(|hash| hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()))
        .collect();
    if !valid_hashes.is_empty()
        && valid_hashes
            .iter()
            .any(|h| !h.eq_ignore_ascii_case(&file.digest))
    {
        projection.graph_status = "stale (indexed source hash differs)".to_string();
        projection.callers.clear();
        projection.callees.clear();
    }
    let selected = if symbol.is_some() && projection.symbols.len() == 1 {
        Some(&projection.symbols[0])
    } else {
        None
    };
    let source = source_excerpt(&file.text, selected);
    if selected.is_some() && source.is_none() {
        projection.graph_status = "stale (indexed line range outside source)".to_string();
        projection.callers.clear();
        projection.callees.clear();
    }
    let omitted_symbols = projection.symbols.len().saturating_sub(40);
    projection.symbols.truncate(40);
    Ok(ContextOutput {
        schema_version: 1,
        file_path: file.relative.clone(),
        symbol: symbol.map(str::to_string),
        graph_status: projection.graph_status,
        source: source.or_else(|| source_excerpt(&file.text, None)),
        symbols: projection.symbols,
        callers: projection.callers,
        callees: projection.callees,
        omitted_symbols_at_least: omitted_symbols,
        omitted_relationships_at_least: usize::from(projection.relationship_limit_reached),
        truncated: omitted_symbols > 0 || projection.relationship_limit_reached,
    })
}

fn source_excerpt(text: &str, selected: Option<&Symbol>) -> Option<SourceExcerpt> {
    let (start, end) = selected
        .map(|symbol| {
            (
                symbol.line_start.max(1) as usize,
                symbol.line_end.max(symbol.line_start).max(1) as usize,
            )
        })
        .unwrap_or((1, 24));
    if selected.is_some() && end > text.lines().count() {
        return None;
    }
    let mut out = String::new();
    let mut last = 0;
    let mut truncated = false;
    for (index, line) in text.lines().enumerate() {
        let line_number = index + 1;
        if line_number < start {
            continue;
        }
        if line_number > end {
            truncated = selected.is_none();
            break;
        }
        if out.len() + line.len() + 1 > 2_048 {
            let remaining = 2_048usize.saturating_sub(out.len());
            out.push_str(prefix_bytes(line, remaining));
            truncated = true;
            last = line_number;
            break;
        }
        out.push_str(line);
        out.push('\n');
        last = line_number;
    }
    (last >= start).then_some(SourceExcerpt {
        line_start: start,
        line_end: last,
        text: out,
        truncated,
    })
}

fn prefix_bytes(value: &str, max_bytes: usize) -> &str {
    let mut end = value.len().min(max_bytes);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

fn fit_context(output: &mut ContextOutput, max_bytes: usize) -> Result<()> {
    while serde_json::to_vec(&output)?.len() > max_bytes {
        output.truncated = true;
        if let Some(source) = &mut output.source {
            if source.text.len() > 128 {
                let reduced = source.text.len() / 2;
                source
                    .text
                    .truncate(prefix_bytes(&source.text, reduced).len());
                source.truncated = true;
                continue;
            }
        }
        if output.callees.pop().is_some() || output.callers.pop().is_some() {
            output.omitted_relationships_at_least += 1;
            continue;
        }
        if output.symbols.pop().is_some() {
            output.omitted_symbols_at_least += 1;
            continue;
        }
        if output.source.take().is_some() {
            continue;
        }
        bail!("context metadata exceeds --max-bytes {max_bytes}");
    }
    Ok(())
}

fn read_message_text(
    text: Option<String>,
    body_file: Option<PathBuf>,
    scan_bytes: usize,
) -> Result<(String, bool)> {
    let (text, truncated) = match (text, body_file) {
        (Some(text), None) => (text, false),
        (None, Some(path)) => {
            let mut bytes = Vec::new();
            File::open(&path)
                .with_context(|| format!("open message body: {}", path.display()))?
                .take((scan_bytes + 1) as u64)
                .read_to_end(&mut bytes)?;
            let truncated = bytes.len() > scan_bytes;
            if truncated {
                bytes.truncate(scan_bytes);
            }
            let valid_end = match std::str::from_utf8(&bytes) {
                Ok(_) => bytes.len(),
                Err(err) if truncated && err.error_len().is_none() => err.valid_up_to(),
                Err(_) => bail!("message body is not UTF-8"),
            };
            bytes.truncate(valid_end);
            (
                String::from_utf8(bytes).context("message body is not UTF-8")?,
                truncated,
            )
        }
        _ => bail!("provide exactly one of --text or --body-file"),
    };
    ensure!(
        text.len() <= scan_bytes,
        "message body exceeds --scan-bytes {scan_bytes}"
    );
    ensure!(!text.contains('\0'), "message body contains NUL bytes");
    Ok((text, truncated))
}

fn ref_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?:^|[^A-Za-z0-9_.-])(?P<path>/?(?:[A-Za-z0-9_.-]+/)+[A-Za-z0-9_.-]+\.(?:py|rs|ts|tsx|js|jsx|go|c|cc|cpp|h|hpp))(?:::(?P<symbol>[A-Za-z_][A-Za-z0-9_]*(?:\.[A-Za-z_][A-Za-z0-9_]*)*))?",
        )
        .expect("valid code-reference regex")
    })
}

fn compact_context(output: ContextOutput) -> CompactContext {
    CompactContext {
        path: output.file_path,
        symbol: output.symbol,
        source: output
            .source
            .map(|source| normalize_whitespace(&source.text, 320))
            .unwrap_or_default(),
        callers: output
            .callers
            .into_iter()
            .take(3)
            .map(|row| prefix_bytes(&row.qualified_name, 120).to_string())
            .collect(),
        callees: output
            .callees
            .into_iter()
            .take(3)
            .map(|row| prefix_bytes(&row.qualified_name, 120).to_string())
            .collect(),
        graph_status: output.graph_status,
    }
}

fn normalize_whitespace(value: &str, max_bytes: usize) -> String {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    prefix_bytes(&normalized, max_bytes).to_string()
}

fn fit_refs(output: &mut RefsOutput, max_bytes: usize) -> Result<()> {
    while serde_json::to_vec(&output)?.len() > max_bytes {
        output.truncated = true;
        let Some(last) = output.contexts.last_mut() else {
            bail!("reference metadata exceeds --max-bytes {max_bytes}");
        };
        if last.source.len() > 48 {
            let reduced = last.source.len() / 2;
            last.source
                .truncate(prefix_bytes(&last.source, reduced).len());
            continue;
        }
        if last.callees.pop().is_some() || last.callers.pop().is_some() {
            continue;
        }
        output.contexts.pop();
        output.omitted_refs += 1;
    }
    Ok(())
}
