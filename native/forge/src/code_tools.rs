//! Bounded native source inspection and context-only body folding.
use crate::{patch, source_file::SourceRoot};
use anyhow::{ensure, Context, Result};
use clap::{Args, Subcommand};
use conductor_native::syntax::{self, Inspection, Language, MAX_SOURCE_BYTES};
use serde_json::{json, Value};
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

/// Shared catalog contract for the existing native inspection operation.
/// Keep the original harness field insertion order for serialized identities.
pub(crate) fn inspection_definition() -> Value {
    let string = |min: u64, max: u64| json!({"type":"string","minLength":min,"maxLength":max});
    let integer = |min: u64, max: u64| json!({"type":"integer","minimum":min,"maximum":max});
    let fields = vec![
        ("path", string(1, 4096)),
        ("language", string(1, 32)),
        ("symbol", string(1, 256)),
        ("max_results", integer(1, 1000)),
        ("max_bytes", integer(512, 1048576)),
    ];
    let properties: serde_json::Map<String, Value> = fields
        .into_iter()
        .map(|(key, value)| (key.into(), value))
        .collect();
    json!({"name":"code_inspect","description":"Native definitions, lexical references and scopes; bounded output with source hash.",
        "inputSchema":{"type":"object","properties":properties,"required":["path"],"additionalProperties":false}})
}

#[derive(Args)]
pub struct CodeArgs {
    #[arg(long, default_value = ".", global = true)]
    root: PathBuf,
    #[command(subcommand)]
    action: Action,
}

#[derive(Subcommand)]
enum Action {
    /// Python/Rust signatures, source snapshot hashes and graph relationships.
    Context(crate::code_context::ContextArgs),
    /// Definitions, signatures, lexical references, scopes and dependency spellings.
    Inspect {
        #[command(flatten)]
        source: SourceArgs,
        #[arg(long)]
        symbol: Option<String>,
        #[arg(long, default_value_t = 100)]
        max_results: usize,
        #[arg(long, default_value_t = 32768)]
        max_bytes: usize,
    },
    /// Fold function bodies for context; never writes the source file.
    Fold {
        #[command(flatten)]
        source: SourceArgs,
        #[arg(long, default_value_t = 32768)]
        max_bytes: usize,
    },
    /// Prepare a hash-checked edit; --apply commits after native syntax validation.
    Patch {
        #[arg(long)]
        request: PathBuf,
        #[arg(long)]
        apply: bool,
    },
    /// Restore exact bytes from a retained recovery blob, guarded by current hash.
    Restore {
        file: PathBuf,
        #[arg(long)]
        expected_hash: String,
        #[arg(long)]
        git_snapshot: String,
        #[arg(long)]
        apply: bool,
    },
}

#[derive(Args)]
struct SourceArgs {
    file: PathBuf,
    /// Override extension detection, e.g. cpp for a C++ .h header.
    #[arg(long)]
    language: Option<String>,
    #[arg(long)]
    expected_hash: Option<String>,
}

fn load(root: &Path, args: &SourceArgs) -> Result<(String, Language)> {
    let root = SourceRoot::open(root)?;
    let language = match &args.language {
        Some(name) => Language::from_name(name)?,
        None => {
            Language::from_path(&args.file).context("unsupported extension; specify --language")?
        }
    };
    let snapshot = root.read(&args.file)?;
    if let Some(expected) = &args.expected_hash {
        ensure!(*expected == snapshot.sha256(), "source hash changed");
    }
    Ok((snapshot.content, language))
}

pub(crate) fn project(inspection: &Inspection, symbol: Option<&str>, limit: usize) -> Value {
    let definitions: Vec<_> = inspection
        .symbols
        .iter()
        .filter(|s| symbol.is_none_or(|name| s.name == name || s.qualified == name))
        .collect();
    let references: Vec<_> = inspection
        .references
        .iter()
        .filter(|r| symbol.is_none_or(|name| r.name == name || r.scope == name))
        .collect();
    json!({"language":inspection.language,"source_sha256":inspection.source_sha256,
        "authority":"native-lexical-syntax","symbols":definitions.iter().take(limit).collect::<Vec<_>>(),
        "references":references.iter().take(limit).collect::<Vec<_>>(),
        "dependencies":inspection.dependencies.iter().take(limit).collect::<Vec<_>>(),
        "symbol_count":definitions.len(),"reference_count":references.len(),
        "dependency_count":inspection.dependencies.len(),"dynamic_calls":inspection.dynamic_calls,
        "truncated":definitions.len()>limit || references.len()>limit || inspection.dependencies.len()>limit})
}

pub fn run(args: CodeArgs) -> Result<u8> {
    match args.action {
        Action::Context(context) => return crate::code_context::run(&args.root, context),
        Action::Inspect {
            source,
            symbol,
            max_results,
            max_bytes,
        } => {
            ensure!(
                (1..=1000).contains(&max_results),
                "max-results must be 1..1000"
            );
            ensure!(
                (512..=MAX_SOURCE_BYTES).contains(&max_bytes),
                "max-bytes must be 512..1048576"
            );
            let (source, language) = load(&args.root, &source)?;
            let inspection = syntax::inspect(&source, language)?;
            let mut limit = max_results;
            loop {
                let output =
                    serde_json::to_string(&project(&inspection, symbol.as_deref(), limit))?;
                if output.len() <= max_bytes {
                    println!("{output}");
                    break;
                }
                ensure!(
                    limit > 1,
                    "selected symbol/signature exceeds max-bytes; increase the explicit budget"
                );
                limit /= 2;
            }
        }
        Action::Fold { source, max_bytes } => {
            ensure!(
                (512..=MAX_SOURCE_BYTES).contains(&max_bytes),
                "max-bytes must be 512..1048576"
            );
            let (source, language) = load(&args.root, &source)?;
            let inspection = syntax::inspect(&source, language)?;
            let output = serde_json::to_string(&json!({"source_sha256":inspection.source_sha256,
                "language":language,"context_only":true,"source":inspection.fold(&source)?}))?;
            ensure!(output.len() <= max_bytes, "folded source exceeds max-bytes");
            println!("{output}");
        }
        Action::Patch { request, apply } => {
            let file = std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(request)
                .context("open patch request")?;
            ensure!(
                file.metadata()?.is_file(),
                "patch request must be a regular file"
            );
            let mut document = String::new();
            file.take(4 * MAX_SOURCE_BYTES as u64 + 1)
                .read_to_string(&mut document)?;
            ensure!(
                document.len() <= 4 * MAX_SOURCE_BYTES,
                "patch request byte limit exceeded"
            );
            let request: patch::Request = serde_json::from_str(&document)?;
            let root = SourceRoot::open(&args.root)?;
            let plan = patch::prepare(&root, &request)?;
            let report = if apply {
                plan.apply(&root)?
            } else {
                plan.report()
            };
            println!("{}", serde_json::to_string(&report)?);
        }
        Action::Restore {
            file,
            expected_hash,
            git_snapshot,
            apply,
        } => {
            let root = SourceRoot::open(&args.root)?;
            let plan = patch::prepare_restore(&root, &file, &expected_hash, &git_snapshot)?;
            let report = if apply {
                plan.apply(&root)?
            } else {
                plan.report()
            };
            println!("{}", serde_json::to_string(&report)?);
        }
    }
    Ok(0)
}
