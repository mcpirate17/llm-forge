//! Host defaults and presentation over the existing native ledger computation.
use super::{audit, rollup};
use anyhow::{ensure, Context, Result};
use clap::{Args, Subcommand};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

#[derive(Args)]
pub struct BudgetArgs {
    /// Host whose transcripts and tracked baseline belong to this audit.
    #[arg(long, global = true, default_value = ".")]
    host: PathBuf,
    #[arg(long, global = true)]
    ledger_root: Option<PathBuf>,
    /// Default rollup input: flag, LEDGER_TRANSCRIPTS, then HOME/.claude/projects.
    #[arg(long, global = true)]
    transcripts: Option<PathBuf>,
    /// Defaults to HOST/ledger/cost_budget_baseline.json.
    #[arg(long, global = true)]
    baseline: Option<PathBuf>,
    #[arg(long, global = true, default_value_t = 7, value_parser = clap::value_parser!(u32).range(1..=3650))]
    window_days: u32,
    #[command(subcommand)]
    action: Action,
}

#[derive(Subcommand)]
enum Action {
    /// Roll up explicit inputs, or this host's transcript project by default.
    Rollup(rollup::RollupArgs),
    /// Check budget metrics as JSON; recording requires explicit --record.
    Audit {
        #[arg(long)]
        record: bool,
    },
    /// Human cost-metric table; routing remains `forge ledger report`.
    Report,
    /// Gate phase JSON: only measured regressions fail the phase.
    Gate,
}

fn transcripts(host: &Path, explicit: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(path) =
        explicit.or_else(|| std::env::var_os("LEDGER_TRANSCRIPTS").map(PathBuf::from))
    {
        ensure!(
            !path.as_os_str().is_empty(),
            "transcript directory must not be empty"
        );
        return Ok(path);
    }
    let home = std::env::var_os("HOME").context("HOME is required for default transcripts")?;
    ensure!(!home.is_empty(), "HOME must not be empty");
    let name = host
        .to_str()
        .context("host path must be UTF-8")?
        .replace('/', "-");
    Ok(PathBuf::from(home).join(".claude/projects").join(name))
}

fn default_rollup(mut args: rollup::RollupArgs, config: BudgetArgs) -> Result<i32> {
    if args.out.is_none() {
        args.out = config.ledger_root;
    }
    if args.paths.is_empty() {
        let host = config.host.canonicalize().context("resolve budget host")?;
        let input = transcripts(&host, config.transcripts)?;
        ensure!(
            input.is_dir(),
            "no transcript directory at {}; pass paths or --transcripts",
            input.display()
        );
        args.project = args.project.or_else(|| {
            input
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
        });
        args.repo = args.repo.or(Some(host));
        args.paths.push(input);
    }
    rollup::run(args)
}

fn gate(evaluation: audit::Evaluation) -> Value {
    match evaluation {
        audit::Evaluation::NoData(message) => json!({"name":"cost-budget-audit","ok":true,
            "detail":format!("NO_DATA: {message}"),"evidence":{"status":"NO_DATA","metrics":{},"message":message}}),
        audit::Evaluation::Output { value, .. } => {
            let metrics = value["metrics"]
                .as_object()
                .expect("native audit metrics are an object");
            let ok = metrics
                .values()
                .all(|metric| metric["status"] != "REGRESSION");
            let mut details = metrics
                .iter()
                .map(|(name, metric)| {
                    format!(
                        "{name}={}",
                        metric["status"].as_str().expect("native metric status")
                    )
                })
                .collect::<Vec<_>>();
            details.sort();
            json!({"name":"cost-budget-audit","ok":ok,"detail":format!("{}: {}",
                value["status"].as_str().expect("native audit status"),details.join("; ")),"evidence":value})
        }
    }
}

fn number(value: f64) -> String {
    let formatted = format!("{value:.1}");
    let (integer, fractional) = formatted.split_once('.').expect("fixed decimal format");
    let mut result = String::new();
    for (index, digit) in integer.chars().enumerate() {
        if index > 0
            && (integer.len() - index).is_multiple_of(3)
            && digit.is_ascii_digit()
            && integer.as_bytes()[index - 1].is_ascii_digit()
        {
            result.push(',');
        }
        result.push(digit);
    }
    format!("{result}.{fractional}")
}

fn report(value: &Value) -> String {
    let window = &value["window"];
    let mut lines = vec![format!(
        "cost-budget report  window {}..{} ({} day(s))  overall {}",
        window["from"].as_str().expect("native window start"),
        window["to"].as_str().expect("native window end"),
        window["days"],
        value["status"].as_str().expect("native audit status")
    )];
    let mut metrics = value["metrics"]
        .as_object()
        .expect("native metrics")
        .iter()
        .collect::<Vec<_>>();
    metrics.sort_by_key(|(name, _)| *name);
    for (name, metric) in metrics {
        let observed = metric["value"]
            .as_f64()
            .map(number)
            .unwrap_or_else(|| "-".into());
        let mut detail = match metric["baseline"].as_f64() {
            None => "no baseline".into(),
            Some(baseline) => {
                let delta = metric["delta_pct"]
                    .as_f64()
                    .map(|d| format!("{d:+.1}%"))
                    .unwrap_or_else(|| "n/a".into());
                format!("baseline {}, {delta}", number(baseline))
            }
        };
        if let Some(n) = metric["n"].as_u64().filter(|n| *n != 0) {
            detail.push_str(&format!("  n={n}"));
        }
        lines.push(format!(
            "{name:<26} {:<12} {observed}  ({detail})",
            metric["status"].as_str().expect("native metric status")
        ));
    }
    lines.join("\n")
}

pub fn run(args: BudgetArgs) -> Result<i32> {
    if let Action::Rollup(rollup) = args.action {
        return default_rollup(
            rollup,
            BudgetArgs {
                action: Action::Report,
                ..args
            },
        );
    }
    let is_gate = matches!(args.action, Action::Gate);
    let human = matches!(args.action, Action::Report);
    let record = matches!(args.action, Action::Audit { record: true });
    let host = args.host.canonicalize().context("resolve budget host")?;
    let evaluation = audit::evaluate(audit::AuditArgs {
        ledger_root: args.ledger_root,
        baseline: args
            .baseline
            .unwrap_or_else(|| host.join("ledger/cost_budget_baseline.json")),
        window_days: args.window_days,
        record,
    })?;
    if is_gate {
        println!("{}", serde_json::to_string(&gate(evaluation))?);
        return Ok(0);
    }
    match evaluation {
        audit::Evaluation::NoData(message) => {
            eprintln!("{message}");
            Ok(3)
        }
        audit::Evaluation::Output { value, code } => {
            println!(
                "{}",
                if human {
                    report(&value)
                } else {
                    serde_json::to_string(&value)?
                }
            );
            Ok(code)
        }
    }
}
