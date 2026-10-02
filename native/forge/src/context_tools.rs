//! Native context composition, actual usage reports, and deferred tool discovery.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{bail, ensure, Context, Result};
use clap::{Args, Subcommand, ValueEnum};
use conductor_native::context_projection::{compose, Fragment};
use conductor_native::context_telemetry_aggregate::{format_rich_summary, summarize_report};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

#[derive(Args)]
pub struct ContextArgs {
    #[command(subcommand)]
    action: ContextCommand,
}

#[derive(Subcommand)]
enum ContextCommand {
    /// Report actual provider usage, request deduplication, and context overhead.
    Report(ReportArgs),
    /// Compose stable instructions and bounded dynamic fragments without an LLM.
    Compose(ComposeArgs),
    /// Search an MCP tools/list snapshot; retrieve only requested definitions.
    Tools(ToolsArgs),
}

#[derive(Args)]
struct ReportArgs {
    #[arg(required = true)]
    paths: Vec<PathBuf>,
    #[arg(long)]
    since: Option<String>,
    #[arg(long, default_value_t = 10)]
    top: i64,
    #[arg(long, default_value_t = 8000)]
    bound_bytes: i128,
    #[arg(long)]
    json: bool,
}

#[derive(Args)]
struct ComposeArgs {
    #[arg(long)]
    fragments: PathBuf,
    #[arg(long, default_value_t = 16000)]
    max_bytes: usize,
    /// Conservative UTF-8 byte token upper bound; exact provider usage is separate.
    #[arg(long)]
    max_tokens: Option<usize>,
    /// Persist full omitted fragments in this directory, keyed by content hash.
    #[arg(long)]
    spill_dir: PathBuf,
}

#[derive(Clone, Copy, ValueEnum)]
enum Detail {
    Names,
    Summary,
    Schema,
}

#[derive(Args)]
struct ToolsArgs {
    #[arg(long)]
    catalog: PathBuf,
    #[arg(long, default_value = "")]
    query: String,
    #[arg(long, value_enum, default_value_t = Detail::Names)]
    detail: Detail,
    #[arg(long, default_value_t = 20)]
    max_results: usize,
    #[arg(long, default_value_t = 8000)]
    max_bytes: usize,
}

pub fn run(args: ContextArgs) -> Result<u8> {
    match args.action {
        ContextCommand::Report(args) => {
            let paths = args
                .paths
                .iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect::<Vec<_>>();
            let stamp = crate::instant::isoformat_millis_utc(crate::instant::now());
            let report = summarize_report(
                &paths,
                args.bound_bytes,
                args.since.as_deref(),
                args.top,
                &stamp,
                "native-context-report",
            )?;
            println!(
                "{}",
                if args.json {
                    serde_json::to_string(&report)?
                } else {
                    format_rich_summary(&report).map_err(anyhow::Error::msg)?
                }
            );
            Ok(0)
        }
        ContextCommand::Compose(args) => {
            ensure!(
                args.max_bytes >= 512 && args.max_tokens.is_none_or(|n| n >= 512),
                "context budget must be >=512"
            );
            let fragments: Vec<Fragment> = serde_json::from_value(read_json(&args.fragments)?)?;
            let result = compose(
                &fragments,
                args.max_tokens
                    .map_or(args.max_bytes, |n| n.min(args.max_bytes)),
                Some(&args.spill_dir),
            );
            println!("{}", serde_json::to_string(&result)?);
            Ok(if result.protected_overflow { 2 } else { 0 })
        }
        ContextCommand::Tools(args) => {
            println!(
                "{}",
                serde_json::to_string(&project_tools(&read_json(&args.catalog)?, &args)?)?
            );
            Ok(0)
        }
    }
}

fn read_json(path: &Path) -> Result<Value> {
    let mut bytes = Vec::new();
    File::open(path)
        .with_context(|| format!("opening {}", path.display()))?
        .take(16 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= 16 * 1024 * 1024,
        "context input exceeds 16 MiB"
    );
    serde_json::from_slice(&bytes).with_context(|| format!("parsing {}", path.display()))
}

fn project_tools(catalog: &Value, args: &ToolsArgs) -> Result<Value> {
    ensure!(
        (512..=32768).contains(&args.max_bytes) && (1..=100).contains(&args.max_results),
        "tools budget: max-bytes 512..32768, max-results 1..100"
    );
    let envelope = catalog.get("result").unwrap_or(catalog);
    let rows = envelope
        .get("tools")
        .unwrap_or(envelope)
        .as_array()
        .context("catalog must be an MCP tools/list response or tool array")?;
    let mut tools = BTreeMap::new();
    for tool in rows {
        let name = tool
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty() && name.len() <= 256)
            .context("tool name must be a nonempty string <=256 bytes")?;
        if let Some(previous) = tools.insert(name, tool) {
            ensure!(
                crate::json_canon::canonical_json(previous)
                    == crate::json_canon::canonical_json(tool),
                "conflicting tool definitions for {name}"
            );
        }
    }
    let version = format!(
        "{:x}",
        Sha256::digest(crate::json_canon::canonical_json(&json!(tools
            .values()
            .collect::<Vec<_>>())))
    );
    let terms: Vec<_> = args
        .query
        .split_whitespace()
        .map(str::to_lowercase)
        .collect();
    let mut matched: Vec<_> = tools
        .iter()
        .filter(|(name, tool)| {
            let text = format!(
                "{} {}",
                name,
                tool.get("description")
                    .and_then(Value::as_str)
                    .unwrap_or("")
            )
            .to_lowercase();
            terms.iter().all(|term| text.contains(term))
        })
        .collect();
    matched.sort_by_key(|(name, _)| {
        (
            !name.to_lowercase().starts_with(&args.query.to_lowercase()),
            **name,
        )
    });
    let total = matched.len();
    let mut selected = Vec::new();
    for (name, tool) in matched.into_iter().take(args.max_results) {
        selected.push(match args.detail {
            Detail::Names => json!(name),
            Detail::Summary => json!({"name":name,"description":tool.get("description")}),
            Detail::Schema => {
                ensure!(
                    tool.get("inputSchema").is_some_and(Value::is_object),
                    "tool {name} has no inputSchema object"
                );
                (*tool).clone()
            }
        });
    }
    let complete = envelope.get("nextCursor").is_none_or(Value::is_null);
    loop {
        let output = json!({"catalog_version":version,"catalog_complete":complete,"search_mode":"lexical","matched_total":total,"omitted":total-selected.len(),"tools":selected});
        if serde_json::to_vec(&output)?.len() <= args.max_bytes {
            return Ok(output);
        }
        if selected.pop().is_none() {
            bail!("tool envelope cannot fit max-bytes");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(detail: Detail) -> ToolsArgs {
        ToolsArgs {
            catalog: PathBuf::new(),
            query: String::new(),
            detail,
            max_results: 20,
            max_bytes: 512,
        }
    }

    #[test]
    fn catalog_order_is_stable_and_large_schemas_are_omitted_whole() {
        let a = json!({"name":"a","inputSchema":{"type":"object"}});
        let b = json!({"name":"b","description":"x".repeat(1000),"inputSchema":{"type":"object"}});
        let first = project_tools(&json!([a, b]), &args(Detail::Schema)).unwrap();
        let second = project_tools(&json!([b, a]), &args(Detail::Schema)).unwrap();
        assert_eq!(first, second);
        assert!(serde_json::to_vec(&first).unwrap().len() <= 512);
        assert_eq!(first["omitted"], 1);
        assert_eq!(first["tools"][0]["inputSchema"]["type"], "object");
    }

    #[test]
    fn conflicting_tools_are_rejected_and_partial_catalog_is_declared() {
        assert!(project_tools(
            &json!([{"name":"a"},{"name":"a","description":"different"}]),
            &args(Detail::Names)
        )
        .is_err());
        let value = project_tools(
            &json!({"result":{"tools":[{"name":"a"}],"nextCursor":"page2"}}),
            &args(Detail::Names),
        )
        .unwrap();
        assert_eq!(value["catalog_complete"], false);
    }
}
