use super::policy::{source_path, Policy};
use crate::{code_tools, patch, source_file::SourceRoot};
use anyhow::{ensure, Context, Result};
use conductor_native::syntax::{self, Language};
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::PathBuf;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Source {
    path: PathBuf,
    language: Option<String>,
    symbol: Option<String>,
    max_results: Option<usize>,
    max_bytes: Option<usize>,
}

pub fn inspect(policy: &Policy, input: &str, fold: bool) -> Result<Value> {
    let args: Source = serde_json::from_str(input)?;
    source_path(&args.path)?;
    let source = SourceRoot::open(&policy.source_root)?.read(&args.path)?;
    let language = args
        .language
        .as_deref()
        .map(Language::from_name)
        .transpose()?
        .or_else(|| Language::from_path(&args.path))
        .context("unsupported language")?;
    let inspection = syntax::inspect(&source.content, language)?;
    let budget = args.max_bytes.unwrap_or(32768);
    if fold {
        let value = json!({"source_sha256":inspection.source_sha256,"language":language,
            "context_only":true,"source":inspection.fold(&source.content)?});
        ensure!(
            serde_json::to_vec(&value)?.len() <= budget,
            "folded source exceeds output budget"
        );
        return Ok(value);
    }
    let mut count = args.max_results.unwrap_or(100);
    loop {
        let value = code_tools::project(&inspection, args.symbol.as_deref(), count);
        if serde_json::to_vec(&value)?.len() <= budget {
            return Ok(value);
        }
        ensure!(count > 1, "selected signature exceeds output budget");
        count /= 2;
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Edit {
    path: PathBuf,
    expected_sha256: String,
    language: Option<String>,
    #[serde(default)]
    apply: bool,
    search: Option<String>,
    replacement: Option<String>,
    expected_matches: Option<usize>,
    patch: Option<String>,
    max_offset_lines: Option<usize>,
    git_snapshot: Option<String>,
}

pub fn edit(policy: &Policy, name: &str, input: &str) -> Result<Value> {
    let args: Edit = serde_json::from_str(input)?;
    policy.permit_write(&args.path)?;
    let root = SourceRoot::open(&policy.source_root)?;
    let plan = if name == "code_restore" {
        patch::prepare_restore(
            &root,
            &args.path,
            &args.expected_sha256,
            args.git_snapshot
                .as_deref()
                .context("missing Git recovery id")?,
        )?
    } else {
        let edit = match name {
            "code_replace" => patch::Edit::Replace {
                search: args.search.context("missing search")?,
                replacement: args.replacement.context("missing replacement")?,
                expected_matches: args.expected_matches.unwrap_or(1),
            },
            "code_diff" => patch::Edit::UnifiedDiff {
                patch: args.patch.context("missing diff")?,
                max_offset_lines: args.max_offset_lines.unwrap_or(0),
            },
            _ => anyhow::bail!("unregistered edit handler"),
        };
        patch::prepare(
            &root,
            &patch::Request {
                path: args.path,
                expected_sha256: args.expected_sha256,
                language: args.language,
                edit,
            },
        )?
    };
    Ok(serde_json::to_value(if args.apply {
        plan.apply(&root)?
    } else {
        plan.report()
    })?)
}
