//! Read-only source checks and ablation proposals through existing native algorithms.
use crate::{
    source_checks::{scan as scan_source, Check},
    source_file::{validate_relative, SourceRoot},
};
use anyhow::{ensure, Context, Result};
use clap::{Args, Subcommand};
use serde_json::{json, Value};
use slop_core::{ablation, hot_loop_scan, rust_scan, style_scan, test_value_scan, verbosity_scan};
use std::collections::BTreeSet;
use std::path::PathBuf;

const MAX_BYTES: usize = 4 * 1024 * 1024;

#[derive(Args)]
pub struct AnalysisArgs {
    #[arg(long, default_value = ".", global = true)]
    root: PathBuf,
    #[arg(long, default_value_t = 65536, global = true)]
    max_bytes: usize,
    #[command(subcommand)]
    action: Action,
}

#[derive(Subcommand)]
enum Action {
    /// Show the exact native check and ablation registries.
    Rules,
    /// Read selected files, report findings and source hashes; exit 1 on any finding.
    Scan(ScanArgs),
    /// Generate automatic source edits for inspection; does not execute or apply mutants.
    Ablations(AblationArgs),
}

#[derive(Args)]
struct ScanArgs {
    #[arg(required = true, num_args = 1..=256)]
    files: Vec<PathBuf>,
    /// Default: all applicable checks. Hot-loop checks require --hot-path.
    #[arg(long, value_enum, value_delimiter = ',')]
    checks: Vec<Check>,
    #[arg(long)]
    hot_path: Vec<String>,
    #[arg(long, default_value_t = 500)]
    max_results: usize,
}

#[derive(Args)]
struct AblationArgs {
    file: PathBuf,
    #[arg(long)]
    expected_hash: Option<String>,
    /// Restrict to these named rules. Omitted: the default registry.
    #[arg(long)]
    rule: Vec<String>,
    #[arg(long)]
    extra: Vec<String>,
    #[arg(long)]
    qualname: Option<String>,
    #[arg(long, default_value_t = 100)]
    max_results: usize,
}

fn registry() -> Value {
    json!({"schema_version":1, "authority":"native-static-analysis",
        "checks":{"style":style_scan::style_scan_rules(),"rust":rust_scan::rust_scan_rules(),
            "fallback":["failure/silent-fallback"],"test-value":test_value_scan::test_value_scan_rules(),
            "verbosity":verbosity_scan::verbosity_scan_rules(),"hot-loop":["perf/python-tensor-loop"]},
        "ablations":{"default":slop_core::rules::DEFAULT.iter().map(|(n,_)| n).collect::<Vec<_>>(),
            "optional":slop_core::rules::OPTIONAL.iter().map(|(n,_)| n).collect::<Vec<_>>()}})
}

fn checks_for(path: &str, requested: &[Check], hot_paths: &[String]) -> Result<Vec<Check>> {
    let is_rust = path.ends_with(".rs");
    ensure!(
        is_rust || path.ends_with(".py"),
        "analysis supports .py and .rs files"
    );
    let allowed = if is_rust {
        vec![Check::Rust]
    } else {
        vec![
            Check::Style,
            Check::Fallback,
            Check::TestValue,
            Check::Verbosity,
            Check::HotLoop,
        ]
    };
    let mut selected: Vec<_> = allowed
        .into_iter()
        .filter(|c| requested.is_empty() || requested.contains(c))
        .collect();
    selected.retain(|c| *c != Check::HotLoop || hot_loop_scan::in_hot_path(path, hot_paths));
    ensure!(
        !selected.is_empty(),
        "no requested check applies to {path}; hot-loop checks need a matching --hot-path"
    );
    Ok(selected)
}

fn scan(root: &SourceRoot, args: ScanArgs) -> Result<(Value, u8)> {
    ensure!(
        (1..=4096).contains(&args.max_results),
        "max-results must be 1..4096"
    );
    ensure!(args.hot_path.len() <= 64, "at most 64 hot paths");
    for path in &args.hot_path {
        validate_relative(std::path::Path::new(path))?;
    }
    let files: BTreeSet<_> = args.files.into_iter().collect();
    ensure!(
        !files.is_empty() && files.len() <= 256,
        "analysis accepts 1..256 files"
    );
    let mut sources = Vec::new();
    let mut findings = Vec::new();
    let mut total = 0;
    let mut bytes = 0;
    for file in files {
        let path = file.to_str().context("source path must be UTF-8")?;
        let checks = checks_for(path, &args.checks, &args.hot_path)?;
        let snapshot = root.read(&file)?;
        bytes += snapshot.content().len();
        ensure!(
            bytes <= 16 * 1024 * 1024,
            "analysis exceeds 16 MiB input budget"
        );
        let rows = scan_source(path, snapshot.content(), &checks)?;
        total += rows.len();
        findings.extend(rows.into_iter().take(args.max_results - findings.len()));
        sources.push(json!({"path":path,"source_sha256":snapshot.sha256(),"checks":checks}));
    }
    Ok((
        json!({"schema_version":1,"authority":"native-static-analysis","sources":sources,
        "findings":findings,"finding_count":total,"truncated":total>findings.len()}),
        u8::from(total > 0),
    ))
}

fn ablations(root: &SourceRoot, args: AblationArgs) -> Result<Value> {
    ensure!(
        (1..=4096).contains(&args.max_results),
        "max-results must be 1..4096"
    );
    ensure!(
        args.rule.len() <= 64 && args.extra.len() <= 64,
        "at most 64 rule selectors"
    );
    ensure!(
        args.file.extension().is_some_and(|e| e == "py"),
        "ablations require a Python source file"
    );
    let snapshot = root.read(&args.file)?;
    if let Some(hash) = &args.expected_hash {
        ensure!(hash == snapshot.sha256(), "source hash changed");
    }
    let rules = ablation::select(
        (!args.rule.is_empty()).then_some(args.rule.as_slice()),
        &args.extra,
    )
    .map_err(anyhow::Error::msg)?;
    let found: Vec<_> = ablation::collect(snapshot.content(), &rules)
        .map_err(anyhow::Error::msg)?
        .into_iter()
        .filter(|a| args.qualname.as_ref().is_none_or(|q| q == &a.qualname))
        .collect();
    let mut proposals = Vec::new();
    for candidate in found.iter().take(args.max_results) {
        // A malformed generated candidate is a tool failure, never evidence that a rule mattered.
        ablation::apply(snapshot.content(), candidate)
            .map_err(anyhow::Error::msg)
            .with_context(|| {
                format!("invalid ablation {}:{}", candidate.qualname, candidate.rule)
            })?;
        proposals.push(candidate);
    }
    Ok(
        json!({"schema_version":1,"authority":"native-source-proposals","executed":false,
        "path":args.file,"source_sha256":snapshot.sha256(),"ablations":proposals,
        "candidate_count":found.len(),"truncated":found.len()>proposals.len()}),
    )
}

pub fn run(args: AnalysisArgs) -> Result<u8> {
    ensure!(
        (512..=MAX_BYTES).contains(&args.max_bytes),
        "max-bytes must be 512..4194304"
    );
    let (value, code) = match args.action {
        Action::Rules => (registry(), 0),
        Action::Scan(scan_args) => scan(&SourceRoot::open(&args.root)?, scan_args)?,
        Action::Ablations(ablation_args) => {
            (ablations(&SourceRoot::open(&args.root)?, ablation_args)?, 0)
        }
    };
    let output = serde_json::to_vec(&value)?;
    ensure!(
        output.len() <= args.max_bytes,
        "analysis exceeds output budget; reduce max-results or raise max-bytes"
    );
    use std::io::Write;
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(&output)?;
    stdout.write_all(b"\n")?;
    Ok(code)
}
