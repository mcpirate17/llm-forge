//! The backlog: gate runs and sweeps folded into one ledger of work items, and the
//! report of what is new, carried and fixed.
//!
//! A run speaks only for what it measured: the modules, or the units of a module, its
//! children reported on, less any function a construct of which went unmeasured. A
//! previous item there and absent now is fixed; one elsewhere is kept as it was, so a
//! gate run over two modules cannot erase the backlog of a sweep over two hundred.

use crate::gate::{Measured, Summary};
use crate::ledger::{self, Finding, Item};
use anyhow::Context;
use chrono::{DateTime, Utc};
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::Path;
use std::time::SystemTime;

const SCHEMA_VERSION: u32 = 2;
const LISTED: usize = 40;

#[derive(Deserialize)]
struct Stored {
    #[serde(default)]
    items: Vec<Item>,
}

/// The ledger's items; a missing or empty file is an empty ledger.
fn load(path: &Path) -> anyhow::Result<Vec<Item>> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error).with_context(|| path.display().to_string()),
    };
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }
    let stored: Stored = serde_json::from_str(&text).with_context(|| path.display().to_string())?;
    Ok(stored.items)
}

/// Every summary's findings, what they measured, and the functions they did not.
#[derive(Default)]
struct Folded {
    findings: Vec<Finding>,
    measured: Vec<Measured>,
    unknown: HashSet<(String, String)>,
}

impl Folded {
    fn speaks_for(&self, module: &str, qualname: &str) -> bool {
        self.measured
            .iter()
            .any(|measured| measured.covers(module, qualname))
            && !self
                .unknown
                .contains(&(module.to_owned(), qualname.to_owned()))
    }
}

fn fold(summaries: &[Summary]) -> anyhow::Result<Folded> {
    let mut out = Folded::default();
    for summary in summaries {
        for finding in summary
            .blocking
            .iter()
            .chain(&summary.advisory)
            .chain(&summary.untested)
        {
            out.findings.push(
                Finding::deserialize(finding)
                    .with_context(|| format!("a malformed finding {finding}"))?,
            );
        }
        out.measured.extend(summary.measured.iter().cloned());
        for finding in summary.incomplete.iter().chain(&summary.unmeasured) {
            let field = |key: &str| finding.get(key).and_then(Value::as_str).map(str::to_owned);
            if let (Some(module), Some(qualname)) = (field("module"), field("qualname")) {
                out.unknown.insert((module, qualname));
            }
        }
    }
    Ok(out)
}

fn meaning(verdict: &str) -> &str {
    match verdict {
        "REACHABLE_BUT_UNTESTED" => "changes behaviour in a regime no test drives",
        "NOT_EXERCISED" => "no driver test made a call to it the probe saw",
        "NO_DIFFERENCE_OBSERVED" => "removing it changed nothing this probe could observe",
        "WITHIN_NUMERIC_NOISE" => "removing it moved only the last bits",
        "NONDETERMINISTIC" => "the function disagrees with itself, so nothing is attributable",
        other => other,
    }
}

fn row(item: &Item) -> String {
    let rules = item
        .rules
        .iter()
        .take(3)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    let more = match item.rules.len() {
        n if n > 3 => format!(" +{}", n - 3),
        _ => String::new(),
    };
    format!(
        "- [ ] `{}:{}` **{}** — {}  \n      `{}` · {rules}{more}",
        item.module,
        item.line,
        item.qualname,
        meaning(&item.verdict),
        item.id
    )
}

fn section(out: &mut Vec<String>, title: &str, lines: Vec<String>) {
    if lines.is_empty() {
        return;
    }
    out.push(format!("## {title} ({})", lines.len()));
    out.push(String::new());
    let more = lines.len().saturating_sub(LISTED);
    out.extend(lines.into_iter().take(LISTED));
    if more > 0 {
        out.push(format!("\n_...and {more} more._"));
    }
    out.push(String::new());
}

/// Everything a report says, from one fold against the previous ledger.
struct Backlog<'l> {
    runs: usize,
    measured_modules: usize,
    new: Vec<Item>,
    carried: Vec<Item>,
    fixed: Vec<&'l Item>,
    items: &'l [Item],
}

fn render(backlog: &Backlog) -> String {
    let now = DateTime::<Utc>::from(SystemTime::now()).format("%Y-%m-%d %H:%M UTC");
    let ledger_modules: BTreeSet<&str> = backlog
        .items
        .iter()
        .map(|item| item.module.as_str())
        .collect();
    let mut out = vec![
        "# Equivalence probe backlog".to_owned(),
        String::new(),
        format!(
            "Generated {now} from {} run(s) that measured {} module(s); the ledger holds {} \
             item(s) over {} module(s).",
            backlog.runs,
            backlog.measured_modules,
            backlog.items.len(),
            ledger_modules.len()
        ),
        String::new(),
        "Items are functions, not findings: a sweep emits one finding per ablation, and".to_owned(),
        "the same function usually appears under several rules.".to_owned(),
        String::new(),
        "| verdict | new | carried | fixed |".to_owned(),
        "|---|---|---|---|".to_owned(),
    ];
    let new = ledger::tally(&backlog.new);
    let carried = ledger::tally(&backlog.carried);
    let fixed = ledger::tally(backlog.fixed.iter().copied());
    let verdicts: BTreeSet<&String> = new
        .keys()
        .chain(carried.keys())
        .chain(fixed.keys())
        .collect();
    let count = |tally: &BTreeMap<String, usize>, verdict: &String| {
        tally.get(verdict).copied().unwrap_or(0)
    };
    for verdict in verdicts {
        out.push(format!(
            "| {verdict} | {} | {} | {} |",
            count(&new, verdict),
            count(&carried, verdict),
            count(&fixed, verdict)
        ));
    }
    out.push(String::new());
    section(&mut out, "New", backlog.new.iter().map(row).collect());
    section(
        &mut out,
        "Carried",
        backlog.carried.iter().map(row).collect(),
    );
    let gone = backlog
        .fixed
        .iter()
        .map(|item| {
            format!(
                "- `{}` **{}** — {}",
                item.module, item.qualname, item.verdict
            )
        })
        .collect();
    section(&mut out, "Fixed since the last sweep", gone);
    if backlog.new.is_empty() && backlog.fixed.is_empty() {
        out.push("Nothing new and nothing fixed since the last sweep.".to_owned());
        out.push(String::new());
    }
    out.join("\n") + "\n"
}

/// The ledger after `folded`: what it did not speak for kept, the rest replaced.
fn merged(previous: &[Item], folded: &Folded, items: &[Item]) -> Vec<Item> {
    let ids: HashSet<&str> = items.iter().map(|item| item.id.as_str()).collect();
    let mut out: Vec<Item> = previous
        .iter()
        .filter(|item| {
            !folded.speaks_for(&item.module, &item.qualname) && !ids.contains(item.id.as_str())
        })
        .cloned()
        .chain(items.iter().cloned())
        .collect();
    out.sort_by(|a, b| {
        (&a.module, &a.qualname, &a.verdict).cmp(&(&b.module, &b.qualname, &b.verdict))
    });
    out
}

fn save(path: &Path, items: &[Item], measured: &[Measured]) -> anyhow::Result<()> {
    let scope: BTreeSet<&str> = items.iter().map(|item| item.module.as_str()).collect();
    let payload = json!({
        "schema_version": SCHEMA_VERSION,
        "generated_at": DateTime::<Utc>::from(SystemTime::now()).to_rfc3339(),
        // Every module the ledger speaks for, and, apart, what this fold measured.
        "scope": scope,
        "last_sweep_scope": measured,
        "items": items,
    });
    write(path, &(serde_json::to_string_pretty(&payload)? + "\n"))
}

fn write(path: &Path, text: &str) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| parent.display().to_string())?;
    }
    std::fs::write(path, text).with_context(|| path.display().to_string())
}

/// Fold the summaries at `paths` into the ledger at `ledger` and write the report to
/// `report`; with `dry_run`, write nothing and return the report.
pub fn backlog(
    paths: &[String],
    ledger_path: &Path,
    report: &Path,
    dry_run: bool,
) -> anyhow::Result<String> {
    let summaries = paths
        .iter()
        .map(|path| {
            let text = std::fs::read_to_string(path).with_context(|| path.clone())?;
            serde_json::from_str::<Summary>(&text).with_context(|| path.clone())
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let folded = fold(&summaries)?;
    let items = ledger::aggregate(&folded.findings);
    let previous = load(ledger_path)?;
    let known: Vec<String> = previous
        .iter()
        .filter(|item| folded.speaks_for(&item.module, &item.qualname))
        .map(|item| item.id.clone())
        .collect();
    let diff = ledger::diff(&known, &items);
    let fixed_ids: HashSet<&str> = diff.fixed.iter().map(String::as_str).collect();
    let measured_modules: BTreeSet<&str> =
        folded.measured.iter().map(|m| m.module.as_str()).collect();
    let all = merged(&previous, &folded, &items);
    let backlog = Backlog {
        runs: summaries.len(),
        measured_modules: measured_modules.len(),
        fixed: previous
            .iter()
            .filter(|item| fixed_ids.contains(item.id.as_str()))
            .collect(),
        new: diff.new,
        carried: diff.carried,
        items: &all,
    };
    let text = render(&backlog);
    if dry_run {
        return Ok(text);
    }
    write(report, &text)?;
    save(ledger_path, &all, &folded.measured)?;
    Ok(format!(
        "{} findings -> {} items ({} new, {} carried, {} fixed)\n  {}\n  {}",
        folded.findings.len(),
        items.len(),
        backlog.new.len(),
        backlog.carried.len(),
        backlog.fixed.len(),
        report.display(),
        ledger_path.display()
    ))
}

/// Fold gate summaries (the JSON files at `summaries`) into the ledger and report.
#[pyfunction]
#[pyo3(signature = (summaries, ledger, report, dry_run=false))]
pub fn equivalence_backlog(
    py: Python<'_>,
    summaries: Vec<String>,
    ledger: &str,
    report: &str,
    dry_run: bool,
) -> PyResult<String> {
    py.detach(|| backlog(&summaries, Path::new(ledger), Path::new(report), dry_run))
        .map_err(|error| PyRuntimeError::new_err(format!("{error:#}")))
}
