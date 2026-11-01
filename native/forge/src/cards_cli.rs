//! Native schema-2 knowledge-card creation and explicit model-bound search.
use crate::{cards, embedding::Config};
use anyhow::{ensure, Result};
use clap::{Args, Subcommand};
use serde_json::Value;
use std::path::{Path, PathBuf};

#[derive(Args)]
pub struct CardsArgs {
    #[command(subcommand)]
    action: Action,
}

#[derive(Subcommand)]
enum Action {
    /// Build a complete card index through an explicitly pinned local model.
    Build {
        #[arg(long, default_value = ".")]
        host: PathBuf,
        /// Defaults to the host's configured notes directory.
        #[arg(long)]
        notes: Option<PathBuf>,
        /// Output parent must exist; publication preserves an existing index on failure.
        #[arg(long)]
        output: PathBuf,
        #[arg(long)]
        embedding: PathBuf,
        #[arg(long,default_value_t=120,value_parser=clap::value_parser!(u64).range(1..=3600))]
        timeout_seconds: u64,
    },
    /// Embed a query in the exact native recipe recorded by the index.
    Search {
        #[arg(long)]
        index: PathBuf,
        #[arg(long)]
        embedding: PathBuf,
        #[arg(long, allow_hyphen_values = true)]
        task: String,
        #[arg(long, default_value_t = 8)]
        top_k: usize,
    },
}

fn config(path: &Path) -> Result<Config> {
    let bytes = crate::bounded_file::read(path, 16 * 1024)?;
    Ok(serde_json::from_value(crate::tool_schema::parse_document(
        std::str::from_utf8(&bytes)?,
    )?)?)
}

pub fn run(args: CardsArgs) -> Result<u8> {
    let value: Value = match args.action {
        Action::Build {
            host,
            notes,
            output,
            embedding,
            timeout_seconds,
        } => serde_json::to_value(cards::build(cards::Settings {
            host,
            notes,
            output,
            embedding: config(&embedding)?,
            timeout_seconds,
        })?)?,
        Action::Search {
            index,
            embedding,
            task,
            top_k,
        } => serde_json::to_value(cards::search(
            &cards::load(&index)?,
            &config(&embedding)?,
            &task,
            top_k,
        )?)?,
    };
    let output = serde_json::to_string(&value)?;
    ensure!(
        output.len() <= 1024 * 1024,
        "card response exceeds 1 MiB; request fewer results"
    );
    println!("{output}");
    Ok(0)
}
