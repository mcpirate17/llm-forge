//! Native memory snapshot construction, verified recall and source auditing.
use super::{memory_hits, sidecar_path, Query};
use anyhow::{ensure, Result};
use clap::{Args, Subcommand};
use conductor_native::memory_index_sidecar::{self, Reader};
use serde_json::json;
use std::path::PathBuf;

#[derive(Args)]
pub struct MemoryArgs {
    #[command(subcommand)]
    action: Action,
}
#[derive(Subcommand)]
enum Action {
    /// Explicitly compile JSONL to a checksummed immutable mapped snapshot.
    Build {
        #[arg(long)]
        source: PathBuf,
        /// Default: SOURCE.sidecar. Publication is atomic under a writer lease.
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Retrieve bounded previews using one task-bound offline query vector.
    Query {
        #[arg(long)]
        source: PathBuf,
        #[arg(long)]
        sidecar: Option<PathBuf>,
        #[arg(long)]
        query: PathBuf,
        #[arg(long, allow_hyphen_values = true)]
        task: String,
        #[arg(long, default_value_t = 8)]
        top_k: usize,
    },
    /// Validate snapshot checksums, source identity and complete source SHA-256.
    Verify {
        #[arg(long)]
        source: PathBuf,
        #[arg(long)]
        sidecar: Option<PathBuf>,
    },
}
pub fn run(args: MemoryArgs) -> Result<u8> {
    let value = match args.action {
        Action::Build { source, output } => {
            let output = output.unwrap_or_else(|| sidecar_path(&source));
            memory_index_sidecar::build_sidecar(&source, &output).map_err(anyhow::Error::msg)?;
            json!({"snapshot":output,"metadata":Reader::open(&source,&output)?.metadata()})
        }
        Action::Verify { source, sidecar } => {
            let sidecar = sidecar.unwrap_or_else(|| sidecar_path(&source));
            let reader = Reader::open(&source, &sidecar)?;
            reader.verify_source_hash()?;
            json!({"verified":true,"metadata":reader.metadata()})
        }
        Action::Query {
            source,
            sidecar,
            query,
            task,
            top_k,
        } => {
            ensure!(
                !task.trim().is_empty() && task.len() <= 4096,
                "task must be nonempty and at most 4096 bytes"
            );
            let query = Query::load(&query, &task)?;
            let hits = memory_hits(&source, sidecar.as_deref(), &query, top_k)?;
            let hits:Vec<_>=hits.into_iter().map(|(score,source,path,title,text)|json!({"score":score,"source":source,"path":path,"title":title,"text":text})).collect();
            json!({"schema_version":1,"mode":"mapped-f32","embedding_fingerprint":query.embedding_fingerprint,"hits":hits})
        }
    };
    let output = serde_json::to_string(&value)?;
    ensure!(
        output.len() <= 1024 * 1024,
        "memory response exceeds 1 MiB; request fewer results"
    );
    println!("{output}");
    Ok(0)
}
