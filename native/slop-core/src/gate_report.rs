//! What a gate summary says: the report a person reads, and the code-review check's
//! findings and metrics.
//!
//! Only a reachable construct no test drives blocks. A module or unit the probe never
//! measured is a MEDIUM finding, so a pass is read against how much of the change was
//! measured; the rest is counted.

use crate::gate::Summary;
use anyhow::Context;
use chrono::{DateTime, Utc};
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

const LISTED: usize = 8;
const STDERR_LINES: usize = 5;
const INDENT: &str = "            ";

#[derive(Deserialize)]
struct Row {
    module: String,
    qualname: String,
    rule: String,
    lineno: usize,
    verdict: String,
    description: String,
    amplifier: Option<String>,
    #[serde(default, deserialize_with = "measured")]
    max_diff_amplified: Option<f64>,
    #[serde(default)]
    waived: bool,
    #[serde(default)]
    named_by: Vec<String>,
    duration_s: Option<f64>,
    stderr_tail: Option<String>,
    drivers_source: String,
}

/// JSON has no infinity, so the probe names a difference it cannot compare
/// ("inf", "-inf", "nan") and a measurement arrives as a number or one of those.
fn measured<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<f64>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Measured {
        Number(f64),
        Name(String),
    }
    Ok(match Option::<Measured>::deserialize(d)? {
        None => None,
        Some(Measured::Number(x)) => Some(x),
        Some(Measured::Name(name)) => Some(match name.as_str() {
            "inf" => f64::INFINITY,
            "-inf" => f64::NEG_INFINITY,
            "nan" => f64::NAN,
            _ => {
                return Err(serde::de::Error::custom(format!(
                    "max_diff_amplified {name:?} is not a measurement"
                )))
            }
        }),
    })
}

impl Row {
    fn at(&self) -> String {
        format!("{}::{}:{}", self.module, self.qualname, self.lineno)
    }

    fn amplified(&self) -> anyhow::Result<String> {
        let change = self.max_diff_amplified.with_context(|| {
            format!(
                "{}: a blocking finding without max_diff_amplified",
                self.at()
            )
        })?;
        Ok(sci(change))
    }
}

fn rows(findings: &[Value]) -> anyhow::Result<Vec<Row>> {
    findings
        .iter()
        .map(|finding| {
            Row::deserialize(finding).with_context(|| format!("a malformed finding {finding}"))
        })
        .collect()
}

/// `x` as Python's `{:.3e}` writes it: a signed exponent of at least two digits.
pub(crate) fn sci(x: f64) -> String {
    if !x.is_finite() {
        return if x.is_nan() {
            "nan"
        } else if x > 0.0 {
            "inf"
        } else {
            "-inf"
        }
        .to_owned();
    }
    let text = format!("{x:.3e}");
    let (mantissa, exponent) = text.split_once('e').expect("{:e} writes an exponent");
    let exponent: i32 = exponent.parse().expect("{:e} writes an integer exponent");
    let sign = if exponent < 0 { '-' } else { '+' };
    format!("{mantissa}e{sign}{:02}", exponent.abs())
}

/// Seconds as Python's `repr` of a float writes them.
fn seconds(x: f64) -> String {
    if x.fract() == 0.0 {
        format!("{x:.1}")
    } else {
        format!("{x}")
    }
}

fn functions(rows: &[Row]) -> BTreeSet<(&str, &str)> {
    rows.iter()
        .map(|row| (row.module.as_str(), row.qualname.as_str()))
        .collect()
}

fn list(out: &mut Vec<String>, header: String, lines: impl IntoIterator<Item = String>) {
    out.push(header);
    out.extend(
        lines
            .into_iter()
            .take(LISTED)
            .map(|line| format!("{INDENT}{line}")),
    );
}

fn render_never_called(out: &mut Vec<String>, untested: &[Row]) {
    for (source, header) in [
        (
            "index",
            "no test file anywhere names -- a coverage hole, not a driver-selection miss:",
        ),
        (
            "unrecorded",
            "no recorded passing test ran -- a coverage hole in the test record:",
        ),
        (
            "record",
            "whose recorded tests ran their lines but made no call the probe recorded -- \
             nothing was measured, this is not a coverage hole:",
        ),
    ] {
        let named: BTreeSet<(&str, &str)> = untested
            .iter()
            .filter(|row| row.drivers_source == source)
            .map(|row| (row.module.as_str(), row.qualname.as_str()))
            .collect();
        if !named.is_empty() {
            let lines = named
                .iter()
                .map(|(module, qualname)| format!("{module}::{qualname}"));
            list(out, format!("{} function(s) {header}", named.len()), lines);
        }
    }
}

fn render_incomplete(out: &mut Vec<String>, incomplete: &[Row]) {
    for row in incomplete {
        let unit = if row.qualname == row.module {
            row.module.clone()
        } else {
            format!("{}::{}", row.module, row.qualname)
        };
        let took = row
            .duration_s
            .map_or(String::new(), |took| format!(" ({}s)", seconds(took)));
        out.push(format!(
            "INCOMPLETE [{}] {unit}  {}{took}",
            row.verdict.to_lowercase(),
            row.description
        ));
        let tail: Vec<&str> = row
            .stderr_tail
            .as_deref()
            .unwrap_or("")
            .trim()
            .lines()
            .collect();
        let skip = tail.len().saturating_sub(STDERR_LINES);
        out.extend(tail[skip..].iter().map(|line| format!("{INDENT}{line}")));
    }
}

/// The report of `summary`, one line per blocking, advisory and incomplete finding,
/// the rest listed by function, and every count on the last line.
pub fn render(summary: &Summary) -> anyhow::Result<String> {
    let mut out = Vec::new();
    for row in rows(&summary.blocking)? {
        out.push(format!("BLOCKING  {}  {}", row.at(), row.description));
        if let Some(amplifier) = &row.amplifier {
            out.push(format!(
                "          reachable via {amplifier} (relative change {}) -- add a test that \
                 drives that regime, or remove the construct",
                row.amplified()?
            ));
        }
    }
    for row in rows(&summary.advisory)? {
        let mark = if row.waived {
            "waived".to_owned()
        } else {
            row.verdict.to_lowercase()
        };
        out.push(format!(
            "advisory  [{mark}] {}  {}",
            row.at(),
            row.description
        ));
    }
    render_never_called(&mut out, &rows(&summary.untested)?);
    render_incomplete(&mut out, &rows(&summary.incomplete)?);
    let unreached: BTreeSet<(String, String, String)> = rows(&summary.unreached)?
        .into_iter()
        .map(|row| (row.module, row.qualname, row.named_by.join(", ")))
        .collect();
    if !unreached.is_empty() {
        let header = format!(
            "{} function(s) a test file names but the drivers chosen for their module never \
             called -- widen the drivers, these are not coverage holes:",
            unreached.len()
        );
        let lines = unreached.iter().map(|(module, qualname, names)| {
            let names = if names.is_empty() { "?" } else { names };
            format!("{module}::{qualname}  named by {names}")
        });
        list(&mut out, header, lines);
    }
    let unmeasured = rows(&summary.unmeasured)?;
    let kinds: BTreeSet<(&str, &str, &str)> = unmeasured
        .iter()
        .map(|row| {
            (
                row.verdict.as_str(),
                row.module.as_str(),
                row.qualname.as_str(),
            )
        })
        .collect();
    if !kinds.is_empty() {
        let header = format!(
            "{} construct(s) in {} function(s) were never measured, so nothing above says \
             anything about them:",
            unmeasured.len(),
            functions(&unmeasured).len()
        );
        let lines = kinds.iter().map(|(verdict, module, qualname)| {
            format!("[{}] {module}::{qualname}", verdict.to_lowercase())
        });
        list(&mut out, header, lines);
    }
    if !summary.modules_index_driven.is_empty() {
        out.push(format!(
            "{} module(s) no recorded test ran were probed with the test files that import them",
            summary.modules_index_driven.len()
        ));
    }
    if !summary.modules_without_drivers.is_empty() {
        out.push(format!(
            "no driver tests for {} module(s); the probe cannot speak for them",
            summary.modules_without_drivers.len()
        ));
    }
    out.push(format!(
        "blocking={} advisory={} live={} probed={} incomplete={} unmeasured={} unreached={} \
         untested={} index_driven={} without_drivers={}",
        summary.blocking.len(),
        summary.advisory.len(),
        summary.live,
        summary.modules_probed,
        summary.incomplete.len(),
        summary.unmeasured.len(),
        summary.unreached.len(),
        summary.untested.len(),
        summary.modules_index_driven.len(),
        summary.modules_without_drivers.len(),
    ));
    Ok(out.join("\n") + "\n")
}

/// The check's findings, as `Finding` keyword arguments less `check_id`.
pub fn check_findings(summary: &Summary) -> anyhow::Result<Vec<Value>> {
    let mut out = Vec::new();
    for row in rows(&summary.blocking)? {
        out.push(json!({
            "rule_id": row.rule,
            "severity": "high",
            "path": row.module,
            "line": row.lineno,
            "message": format!(
                "{}: {} changes the result only under {} (relative change {}); no test drives \
                 that regime. Cover it or remove the construct.",
                row.qualname,
                row.description,
                row.amplifier.as_deref().unwrap_or("?"),
                row.amplified()?
            ),
            "evidence": {
                "qualname": row.qualname,
                "verdict": row.verdict,
                "amplifier": row.amplifier,
                "drivers_source": row.drivers_source,
            },
        }));
    }
    for row in rows(&summary.incomplete)? {
        out.push(json!({
            "rule_id": format!("probe-{}", row.verdict.to_lowercase()),
            "severity": "medium",
            "path": row.module,
            "line": row.lineno,
            "message": format!(
                "{}; a PASS here is the absence of a measurement, not the absence of a finding",
                row.description
            ),
            "help": "Re-run `python -m conductor.equivalence_gate gate --module <module>` to see \
                     the child's own output, or narrow the module's drivers.",
            "evidence": {
                "qualname": row.qualname,
                "verdict": row.verdict,
                "rule": row.rule,
                "duration_s": row.duration_s,
                "stderr_tail": row.stderr_tail,
                "drivers_source": row.drivers_source,
            },
        }));
    }
    Ok(out)
}

fn count(findings: &[Value], key: &str, value: &str) -> usize {
    findings
        .iter()
        .filter(|finding| finding.get(key).and_then(Value::as_str) == Some(value))
        .count()
}

pub fn metrics(summary: &Summary) -> Value {
    json!({
        "modules_probed": summary.modules_probed,
        "advisory": summary.advisory.len(),
        "without_drivers": summary.modules_without_drivers.len(),
        "index_driven": summary.modules_index_driven.len(),
        "timed_out": count(&summary.incomplete, "verdict", "TIMEOUT"),
        "probe_failed": count(&summary.incomplete, "verdict", "PROBE_FAILED"),
        "unreported": count(&summary.incomplete, "verdict", "UNREPORTED"),
        "untested_record": count(&summary.untested, "drivers_source", "record"),
        "untested_index": count(&summary.untested, "drivers_source", "index"),
        "untested_unrecorded": count(&summary.untested, "drivers_source", "unrecorded"),
        "unreached": summary.unreached.len(),
        "unmeasured": summary.unmeasured.len(),
        "live": summary.live,
    })
}

/// Write `text` to `dir` as `gate-<UTC stamp>.json`, renamed into place complete so a
/// reader never sees part of it.
pub fn write_artifact(dir: &Path, text: &str) -> anyhow::Result<PathBuf> {
    std::fs::create_dir_all(dir).with_context(|| dir.display().to_string())?;
    let stamp = DateTime::<Utc>::from(SystemTime::now()).format("%Y%m%dT%H%M%S%6f");
    let path = dir.join(format!("gate-{stamp}.json"));
    let part = path.with_extension("json.part");
    std::fs::write(&part, text).with_context(|| part.display().to_string())?;
    std::fs::rename(&part, &path).with_context(|| path.display().to_string())?;
    Ok(path)
}

fn parse(summary: &str) -> anyhow::Result<Summary> {
    serde_json::from_str(summary).context("an equivalence gate summary")
}

/// The report of a summary `equivalence_gate` returned.
#[pyfunction]
pub fn equivalence_report(summary: &str) -> PyResult<String> {
    parse(summary)
        .and_then(|summary| render(&summary))
        .map_err(|error| PyRuntimeError::new_err(format!("{error:#}")))
}

/// The check's findings and metrics, as JSON, for a summary `equivalence_gate`
/// returned, which is also written to `run_artifacts` for the backlog. A summary that
/// could not be written there is a finding, not a failed review.
pub fn findings(summary: &str, run_artifacts: Option<&Path>) -> anyhow::Result<(String, String)> {
    let parsed = parse(summary)?;
    let mut findings = check_findings(&parsed)?;
    if let Some(dir) = run_artifacts {
        if let Err(error) = write_artifact(dir, summary) {
            findings.push(json!({
                "rule_id": "run-artifact-unwritten",
                "severity": "medium",
                "path": dir.display().to_string(),
                "message": format!(
                    "this run's summary was not recorded ({error:#}), so the backlog will not \
                     see it"
                ),
            }));
        }
    }
    Ok((
        serde_json::to_string(&findings)?,
        metrics(&parsed).to_string(),
    ))
}

#[pyfunction]
#[pyo3(signature = (summary, run_artifacts=None))]
pub fn equivalence_findings(
    summary: &str,
    run_artifacts: Option<&str>,
) -> PyResult<(String, String)> {
    findings(summary, run_artifacts.map(Path::new))
        .map_err(|error| PyRuntimeError::new_err(format!("{error:#}")))
}
