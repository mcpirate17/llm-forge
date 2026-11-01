//! Snapshot-bound, changed-line formatter. The parent Stage owns isolation.
#[path = "candidate_clang_style.rs"]
mod style;
#[path = "candidate_clang_tool.rs"]
mod tool;
use crate::candidate_source::{
    lines::{FileRanges, RangeScope},
    Source,
};
use anyhow::{ensure, Context, Result};
use cap_std::fs::Dir;
use clap::Args;
use conductor_native::candidate_cargo_lint::read_optional;
use git2::{ObjectType, Oid};
use serde_json::json;
use std::path::PathBuf;

#[derive(Args)]
pub struct FormatArgs {
    #[arg(long, default_value = ".")]
    repo: PathBuf,
    #[arg(
        long,
        required_unless_present = "tool_version",
        conflicts_with = "tool_version"
    )]
    base: Option<String>,
    #[arg(
        long,
        required_unless_present = "tool_version",
        conflicts_with = "tool_version"
    )]
    tree: Option<String>,
    /// Probe the explicitly provisioned native binary, without opening Git.
    #[arg(long, conflicts_with = "execute")]
    tool_version: bool,
    /// Execute formatting checks; otherwise print the bound changed-line plan.
    #[arg(long)]
    execute: bool,
    #[arg(last = true, value_name = "FILE")]
    files: Vec<String>,
}

pub fn run(args: FormatArgs) -> Result<u8> {
    let root = args
        .repo
        .canonicalize()
        .context("resolve formatter snapshot")?;
    ensure!(root.is_dir(), "formatter snapshot must be a directory");
    if args.tool_version {
        ensure!(
            args.files.is_empty(),
            "tool version accepts no candidate files"
        );
        return tool::Tool::from_env(&root)?.version();
    }
    ensure!(
        !args.files.is_empty() && args.files.len() <= 4096,
        "formatter requires 1..4096 selected paths"
    );
    let (source, candidate, binding) = Source::tool_context(
        &root,
        args.base.as_deref().context("missing base OID")?,
        args.tree.as_deref().context("missing candidate tree OID")?,
    )?;
    let selected: Vec<_> = args.files.iter().map(String::as_str).collect();
    let plan = source.changed_ranges(&candidate, &selected)?;
    let directory = Dir::open_ambient_dir(&root, cap_std::ambient_authority())?;
    verify_files(&directory, &plan.files)?;
    let changed: Vec<_> = plan
        .files
        .iter()
        .filter(|file| file.scope == RangeScope::Changed)
        .map(|file| file.path.clone())
        .collect();
    let styles = if changed.is_empty() {
        None
    } else {
        Some(style::Plan::new(&source, &candidate.tree_oid, &changed)?)
    };
    binding.verify(&source)?;
    if !args.execute {
        let value = json!({"schema_version":1,"executed":false,"plan":plan});
        crate::json_canon::encoded_size(&value, 32 * 1024 * 1024)?;
        println!("{}", serde_json::to_string(&value)?);
        return Ok(0);
    }
    let Some(styles) = styles else {
        eprintln!("clang-format: selected paths have no added or modified lines");
        return Ok(0);
    };
    let tool = tool::Tool::from_env(&root)?;
    let mut code = 0;
    styles.verify()?;
    for file in &plan.files {
        if file.scope != RangeScope::Changed {
            continue;
        }
        verify_file(&directory, file)?;
        eprintln!("clang-format: {}", file.path.escape_default());
        let status = tool.format(&root, file, &styles.argument_for(&file.path)?)?;
        verify_file(&directory, file)?;
        if status != 0 {
            code = status;
        }
    }
    styles.verify()?;
    verify_files(&directory, &plan.files)?;
    binding.verify(&source)?;
    Ok(code)
}

fn verify_file(root: &Dir, file: &FileRanges) -> Result<usize> {
    let bytes = read_optional(root, &file.path, 8 * 1024 * 1024).map_err(anyhow::Error::msg)?;
    match (bytes, &file.blob_oid) {
        (None, None) if file.scope == RangeScope::Deleted => Ok(0),
        (Some(bytes), Some(oid)) => {
            ensure!(
                Oid::hash_object(ObjectType::Blob, &bytes)? == Oid::from_str(oid)?,
                "formatter source differs from captured candidate: {}",
                file.path.escape_default()
            );
            Ok(bytes.len())
        }
        _ => anyhow::bail!(
            "formatter source presence differs from captured candidate: {}",
            file.path.escape_default()
        ),
    }
}

fn verify_files(root: &Dir, files: &[FileRanges]) -> Result<()> {
    let mut bytes = 0usize;
    for file in files {
        bytes = bytes
            .checked_add(verify_file(root, file)?)
            .context("snapshot byte overflow")?;
        ensure!(bytes <= 32 * 1024 * 1024, "formatter source exceeds 32 MiB");
    }
    Ok(())
}
