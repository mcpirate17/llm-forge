use super::{build, BuildHeader, EmbeddingRow, QueryRequest, Reader, Store};
use anyhow::{ensure, Result};
use clap::{Args, Subcommand};
use conductor_native::bounded_io::read_line;
use std::fs::OpenOptions;
use std::io::BufReader;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;

#[derive(Args)]
pub struct CodeIndexArgs {
    #[command(subcommand)]
    action: Action,
}
#[derive(Subcommand)]
enum Action {
    /// Inspect source scope, AST/line chunk counts and byte costs without embedding.
    Plan {
        #[arg(long)]
        source_root: PathBuf,
        #[arg(long)]
        options: PathBuf,
    },
    /// Extract and embed an explicit source scope, then atomically refresh its index.
    Refresh {
        #[arg(long)]
        index: PathBuf,
        #[arg(long)]
        source_root: PathBuf,
        #[arg(long)]
        config: PathBuf,
    },
    /// Ingest bounded JSONL: one BuildHeader followed by sorted EmbeddingRows.
    Build {
        #[arg(long)]
        index: PathBuf,
        #[arg(long)]
        source_root: PathBuf,
        #[arg(long)]
        input: PathBuf,
    },
    /// Query a source-bound snapshot from a bounded QueryRequest JSON file.
    Query {
        #[arg(long)]
        index: PathBuf,
        #[arg(long)]
        source_root: PathBuf,
        #[arg(long)]
        request: PathBuf,
    },
    /// Verify checksums and print the current generation's identity.
    Status {
        #[arg(long)]
        index: PathBuf,
    },
}
pub fn run(args: CodeIndexArgs) -> Result<u8> {
    let result = match args.action {
        Action::Plan {
            source_root,
            options,
        } => {
            let options: super::extract::Options =
                serde_json::from_slice(&crate::harness::state::read(&options, 1024 * 1024)?)?;
            plan_summary(super::extract::plan(&source_root, &options)?)
        }
        Action::Refresh {
            index,
            source_root,
            config,
        } => {
            let config: super::extract::Refresh =
                serde_json::from_slice(&crate::harness::state::read(&config, 1024 * 1024)?)?;
            serde_json::to_value(super::extract::refresh(
                &Store::create(&index)?,
                &source_root,
                config,
            )?)?
        }
        Action::Build {
            index,
            source_root,
            input,
        } => {
            let file = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(input)?;
            ensure!(
                file.metadata()?.is_file(),
                "embedding input must be a regular JSONL file"
            );
            let mut input = BufReader::new(file);
            let mut line = Vec::new();
            ensure!(
                read_line(&mut input, &mut line, 1024 * 1024)? > 0,
                "embedding stream is empty"
            );
            let header: BuildHeader = serde_json::from_slice(&line)?;
            let mut done = false;
            let rows = std::iter::from_fn(|| {
                if done {
                    return None;
                }
                match read_line(&mut input, &mut line, 1024 * 1024) {
                    Ok(0) => {
                        done = true;
                        None
                    }
                    Ok(_) => {
                        Some(serde_json::from_slice::<EmbeddingRow>(&line).map_err(Into::into))
                    }
                    Err(error) => {
                        done = true;
                        Some(Err(error.into()))
                    }
                }
            });
            serde_json::to_value(build(&Store::create(&index)?, &source_root, header, rows)?)?
        }
        Action::Query {
            index,
            source_root,
            request,
        } => {
            let request: QueryRequest =
                serde_json::from_slice(&crate::harness::state::read(&request, 1024 * 1024)?)?;
            serde_json::to_value(
                Reader::open(&Store::open(&index)?)?.search(&source_root, request)?,
            )?
        }
        Action::Status { index } => {
            serde_json::to_value(Reader::open(&Store::open(&index)?)?.manifest())?
        }
    };
    println!("{}", serde_json::to_string(&result)?);
    Ok(0)
}

fn plan_summary(plan: super::extract::Plan) -> serde_json::Value {
    let count = plan
        .files
        .iter()
        .filter(|file| file.syntax_error.is_some())
        .count();
    let previews: Vec<_> = plan.files.iter().filter_map(|file| file.syntax_error.as_ref().map(|error|
        serde_json::json!({"path":file.path.chars().take(512).collect::<String>(),"error":error.chars().take(512).collect::<String>()}))).take(32).collect();
    serde_json::json!({"source_root":plan.source_root,"files":plan.files.len(),"rows":plan.rows,
        "source_bytes":plan.source_bytes,"syntax_error_count":count,"syntax_errors_preview":previews,
        "model_requests":0,"index_mutations":0})
}
