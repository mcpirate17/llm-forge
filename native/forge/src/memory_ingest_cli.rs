use crate::{
    embedding::Config,
    memory_ingest::{self, Report, Settings},
    memory_store::Store,
};
use anyhow::Result;
use clap::Args;
use std::path::{Path, PathBuf};

#[derive(Args)]
pub struct IngestArgs {
    #[arg(long, default_value = ".")]
    host: PathBuf,
    /// Absolute private store directory; its parent must exist.
    #[arg(long)]
    store: PathBuf,
    #[arg(long)]
    catalog: Option<PathBuf>,
    /// Pinned native Ollama configuration JSON; no automatic model download.
    #[arg(long)]
    embedding: PathBuf,
    /// Select source IDs. Partial updates require a complete prior generation.
    #[arg(long)]
    source: Vec<String>,
    /// Re-embed all selected files even when their content is unchanged.
    #[arg(long)]
    full: bool,
    /// Explicitly allow missing selected roots to remove their prior rows.
    #[arg(long)]
    allow_missing_roots: bool,
    #[arg(long,default_value_t=120,value_parser=clap::value_parser!(u64).range(1..=3600))]
    timeout_seconds: u64,
}
#[derive(Args)]
pub struct SearchArgs {
    #[arg(long)]
    store: PathBuf,
    #[arg(long)]
    embedding: PathBuf,
    #[arg(long, allow_hyphen_values = true)]
    task: String,
    #[arg(long, default_value_t = 8)]
    top_k: usize,
}
fn config(path: &Path) -> Result<Config> {
    let bytes = crate::bounded_file::read(path, 16384)?;
    Ok(serde_json::from_value(crate::tool_schema::parse_document(
        std::str::from_utf8(&bytes)?,
    )?)?)
}
pub fn ingest(args: IngestArgs) -> Result<Report> {
    memory_ingest::ingest(Settings {
        host: args.host,
        store: args.store,
        catalog: args.catalog,
        sources: args.source,
        full: args.full,
        allow_missing_roots: args.allow_missing_roots,
        timeout_seconds: args.timeout_seconds,
        embedding: config(&args.embedding)?,
    })
}
pub fn search(args: SearchArgs) -> Result<serde_json::Value> {
    memory_ingest::search(
        &Store::open(&args.store)?,
        config(&args.embedding)?,
        &args.task,
        args.top_k,
    )
}
