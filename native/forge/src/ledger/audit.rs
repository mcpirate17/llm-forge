//! `forge ledger audit`: design step 5 (`docs/design/cost_ledger.md`
//! section 4, section 6 step 5). Computes the three budget-ratchet metrics
//! over a trailing window of `hook_rollup`/`session_rollup`/`agent_rollup`
//! day files and compares them to a recorded baseline receipt, in the same
//! held/not-passed idiom `mutation_patch_audit.py` already uses for a
//! generated campaign: `RATCHET_HELD` is a real status, never rounded up to
//! `PASS`.
//!
//! Status per metric (design section 4's three prose rules, reconciled
//! against its own worked example -- `docs/design/cost_ledger.md` section 6
//! step 5's exit is "first run produces `RATCHET_HELD` baseline receipts",
//! which only holds if a value exactly equal to a freshly recorded baseline
//! is *not* `PASS`; `PASS` therefore means a strict improvement, not merely
//! "inside tolerance"):
//!   - no recorded baseline for this metric -> `NO_BASELINE`
//!   - value strictly better (lower) than baseline -> `PASS`
//!   - baseline <= value <= baseline * (1 + tolerance) -> `RATCHET_HELD`
//!   - value > baseline * (1 + tolerance) -> `REGRESSION`
//!   - the metric's table contributed zero rows in the window -> `NO_DATA`
//!
//! A window with zero rows across all three tables is a harder failure
//! (misconfigured `--ledger-root`, or `forge ledger rollup` never ran) than
//! one metric alone having no data: that case fails loud as the whole
//! command's `NO_DATA`, exit 3, never a silent pass.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use clap::Args;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const DEFAULT_LEDGER_ROOT: &str = "/mnt/data/llm/ledger/";
const DEFAULT_WINDOW_DAYS: u32 = 7;
const DEFAULT_TOLERANCE_PCT: f64 = 5.0;

const METRIC_NAMES: [&str; 3] = [
    "median_hook_ms",
    "resend_bytes_per_session",
    "tokens_per_landed_pr",
];

#[derive(Args)]
pub struct AuditArgs {
    /// Ledger root directory; defaults to `LEDGER_ROOT`, else
    /// `/mnt/data/llm/ledger/` (same resolution order as `forge ledger
    /// rollup`).
    #[arg(long)]
    pub ledger_root: Option<PathBuf>,

    /// Baseline receipt file. Read (if it exists) to compare against on a
    /// plain check; overwritten only when `--record` is passed.
    #[arg(long)]
    pub baseline: PathBuf,

    /// Trailing window size in days, ending today (UTC).
    #[arg(long, default_value_t = DEFAULT_WINDOW_DAYS)]
    pub window_days: u32,

    /// Record the metrics computed from this window as the new baseline
    /// instead of comparing against the existing one. The only way the
    /// baseline file changes -- never a side effect of a plain check.
    #[arg(long)]
    pub record: bool,
}

// ---------------------------------------------------------------------------
// Row shapes read back off disk. Deliberately independent of
// `rollup::HookRollupRow` / `rollup::SessionRollupRow` / `agent::AgentRollupRow`
// (which derive `Serialize` only, not `Deserialize`): the audit reads a
// strict subset of each row's fields, so a narrow local shape is both
// sufficient and immune to unrelated fields those writers might add later.
#[derive(Debug, Clone, Deserialize)]
struct HookRow {
    n_calls: u64,
    p50_ms: Option<f64>,
}

#[derive(Debug, Clone, Deserialize)]
struct SessionRow {
    resend_bytes: u64,
}

#[derive(Debug, Clone, Deserialize)]
struct AgentRow {
    total_tokens: u64,
    n_landed_prs: u64,
    join_method: String,
}

// ---------------------------------------------------------------------------
// Output shapes.

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Window {
    from: String,
    to: String,
    days: u32,
}

#[derive(Debug, Clone, Serialize)]
struct MetricResult {
    value: Option<f64>,
    n: u64,
    baseline: Option<f64>,
    delta_pct: Option<f64>,
    status: String,
}

#[derive(Debug, Clone, Serialize)]
struct AuditOutput {
    window: Window,
    metrics: BTreeMap<String, MetricResult>,
    status: String,
}

// ---------------------------------------------------------------------------
// Baseline file shape (design section 4, "`--record` writes the baseline
// file").

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct BaselineMetric {
    value: f64,
    n: u64,
    tolerance_pct: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BaselineFile {
    recorded_utc: String,
    window: Window,
    metrics: BTreeMap<String, BaselineMetric>,
    ledger_root_sha: String,
}

// ---------------------------------------------------------------------------

pub fn run(args: AuditArgs) -> Result<i32> {
    let ledger_root = resolve_ledger_root(args.ledger_root);
    let today = today_utc_date();
    let days = window_days(&today, args.window_days);
    let from = days.first().cloned().unwrap_or_else(|| today.clone());
    let to = days.last().cloned().unwrap_or_else(|| today.clone());
    let window = Window {
        from,
        to,
        days: args.window_days,
    };

    let (hooks, hook_files) = read_table::<HookRow>(&ledger_root, "hook_rollup", &days)?;
    let (sessions, session_files) =
        read_table::<SessionRow>(&ledger_root, "session_rollup", &days)?;
    let (agents, agent_files) = read_table::<AgentRow>(&ledger_root, "agent_rollup", &days)?;

    if hooks.is_empty() && sessions.is_empty() && agents.is_empty() {
        eprintln!(
            "forge ledger audit: window {}..{} has zero rows across hook_rollup, \
session_rollup and agent_rollup under {} -- run `forge ledger rollup` first",
            window.from,
            window.to,
            ledger_root.display()
        );
        return Ok(3);
    }

    let raw = compute_raw_metrics(&hooks, &sessions, &agents);

    if args.record {
        let mut files: Vec<PathBuf> = Vec::new();
        files.extend(hook_files);
        files.extend(session_files);
        files.extend(agent_files);
        let ledger_root_sha = sha256_of_files(&files)?;
        let mut metrics = BTreeMap::new();
        for name in METRIC_NAMES {
            if let Some((value, n)) = raw.get(name).copied().flatten_pair() {
                metrics.insert(
                    name.to_string(),
                    BaselineMetric {
                        value,
                        n,
                        tolerance_pct: DEFAULT_TOLERANCE_PCT,
                    },
                );
            } else {
                eprintln!(
                    "forge ledger audit --record: {name} has no data in this window, \
omitted from the recorded baseline (debt, not a failure)"
                );
            }
        }
        let baseline_file = BaselineFile {
            recorded_utc: now_iso8601_utc(),
            window,
            metrics,
            ledger_root_sha,
        };
        let body = serde_json::to_string_pretty(&baseline_file)?;
        if let Some(parent) = args.baseline.parent() {
            fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
        }
        fs::write(&args.baseline, format!("{body}\n"))
            .with_context(|| format!("writing {}", args.baseline.display()))?;
        println!("{body}");
        return Ok(0);
    }

    let recorded = read_baseline(&args.baseline)?;
    let mut metrics = BTreeMap::new();
    for name in METRIC_NAMES {
        let (value, n) = raw.get(name).copied().unwrap_or((None, 0));
        let baseline_metric = recorded.as_ref().and_then(|b| b.metrics.get(name));
        metrics.insert(name.to_string(), metric_result(value, n, baseline_metric));
    }
    let status = overall_status(&metrics);
    let output = AuditOutput {
        window,
        metrics,
        status: status.clone(),
    };
    println!("{}", serde_json::to_string(&output)?);
    Ok(if status == "PASS" || status == "RATCHET_HELD" {
        0
    } else {
        1
    })
}

/// Small helper so `--record`'s loop can write `if let Some((value, n)) =
/// ...` without a second match arm for "no data at all for this name".
trait FlattenPair {
    fn flatten_pair(self) -> Option<(f64, u64)>;
}
impl FlattenPair for Option<(Option<f64>, u64)> {
    fn flatten_pair(self) -> Option<(f64, u64)> {
        match self {
            Some((Some(value), n)) => Some((value, n)),
            _ => None,
        }
    }
}

fn metric_result(
    value: Option<f64>,
    n: u64,
    baseline_metric: Option<&BaselineMetric>,
) -> MetricResult {
    let baseline = baseline_metric.map(|b| b.value);
    let (status, delta_pct) = match (value, baseline_metric) {
        (None, _) => ("NO_DATA".to_string(), None),
        (Some(_), None) => ("NO_BASELINE".to_string(), None),
        (Some(value), Some(b)) => {
            let delta_pct = if b.value == 0.0 {
                None
            } else {
                Some((value - b.value) / b.value * 100.0)
            };
            let allowed = b.value * (1.0 + b.tolerance_pct / 100.0);
            let status = if value > allowed {
                "REGRESSION"
            } else if value < b.value {
                "PASS"
            } else {
                "RATCHET_HELD"
            };
            (status.to_string(), delta_pct)
        }
    };
    MetricResult {
        value,
        n,
        baseline,
        delta_pct,
        status,
    }
}

/// Worst-first so one metric's `NO_DATA`/`REGRESSION` cannot be hidden by
/// the other two passing. `NO_DATA` outranks `NO_BASELINE`: a metric with
/// no rows this window is a live measurement gap (the brief's "records
/// `NO_DATA` ... as debt"), while `NO_BASELINE` is an expected, one-time
/// state for a metric never recorded yet.
fn overall_status(metrics: &BTreeMap<String, MetricResult>) -> String {
    const RANK: [&str; 5] = [
        "REGRESSION",
        "NO_DATA",
        "NO_BASELINE",
        "RATCHET_HELD",
        "PASS",
    ];
    let mut worst = "PASS";
    let mut worst_rank = RANK.len();
    for m in metrics.values() {
        if let Some(rank) = RANK.iter().position(|s| *s == m.status) {
            if rank < worst_rank {
                worst_rank = rank;
                worst = RANK[rank];
            }
        }
    }
    worst.to_string()
}

/// `(value, n)` per metric name, `value = None` when that metric's table
/// contributed zero usable rows in the window.
fn compute_raw_metrics(
    hooks: &[HookRow],
    sessions: &[SessionRow],
    agents: &[AgentRow],
) -> BTreeMap<&'static str, (Option<f64>, u64)> {
    let mut out = BTreeMap::new();

    let hook_pairs: Vec<(f64, u64)> = hooks
        .iter()
        .filter_map(|h| h.p50_ms.map(|p50| (p50, h.n_calls.max(1))))
        .collect();
    let hook_n: u64 = hook_pairs.iter().map(|(_, w)| w).sum();
    out.insert("median_hook_ms", (weighted_median(&hook_pairs), hook_n));

    let resend_n = sessions.len() as u64;
    let resend_value = if sessions.is_empty() {
        None
    } else {
        let total: u64 = sessions.iter().map(|s| s.resend_bytes).sum();
        Some(total as f64 / sessions.len() as f64)
    };
    out.insert("resend_bytes_per_session", (resend_value, resend_n));

    let qualifying: Vec<&AgentRow> = agents
        .iter()
        .filter(|a| a.join_method != "unjoined")
        .collect();
    let landed_n: u64 = qualifying.iter().map(|a| a.n_landed_prs).sum();
    let tokens_value = if landed_n == 0 {
        None
    } else {
        let total_tokens: u64 = qualifying.iter().map(|a| a.total_tokens).sum();
        Some(total_tokens as f64 / landed_n as f64)
    };
    out.insert("tokens_per_landed_pr", (tokens_value, landed_n));

    out
}

/// Weighted median of `(value, weight)` pairs: sort by value, walk
/// cumulative weight, return the value at which cumulative weight first
/// reaches half the total. Matches the design's "median of `p50_ms` across
/// `hook_rollup` rows weighted by `n_calls`" verbatim -- a call-heavy hook's
/// p50 counts proportionally more than a rarely-called one's.
fn weighted_median(pairs: &[(f64, u64)]) -> Option<f64> {
    if pairs.is_empty() {
        return None;
    }
    let mut sorted = pairs.to_vec();
    sorted.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let total: u64 = sorted.iter().map(|(_, w)| w).sum();
    if total == 0 {
        return None;
    }
    let half = total as f64 / 2.0;
    let mut cumulative: u64 = 0;
    for (value, weight) in &sorted {
        cumulative += weight;
        if cumulative as f64 >= half {
            return Some(*value);
        }
    }
    sorted.last().map(|(v, _)| *v)
}

fn read_table<T: DeserializeOwned>(
    ledger_root: &Path,
    table: &str,
    days: &[String],
) -> Result<(Vec<T>, Vec<PathBuf>)> {
    let mut rows = Vec::new();
    let mut files = Vec::new();
    for day in days {
        let path = ledger_root.join(table).join(format!("{day}.jsonl"));
        if !path.is_file() {
            continue;
        }
        let text =
            fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        for (line_no, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let row: T = serde_json::from_str(line)
                .with_context(|| format!("parsing {} line {}", path.display(), line_no + 1))?;
            rows.push(row);
        }
        files.push(path);
    }
    Ok((rows, files))
}

fn read_baseline(path: &Path) -> Result<Option<BaselineFile>> {
    if !path.is_file() {
        return Ok(None);
    }
    let text = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let parsed: BaselineFile = serde_json::from_str(&text)
        .with_context(|| format!("parsing baseline {}", path.display()))?;
    Ok(Some(parsed))
}

fn sha256_of_files(paths: &[PathBuf]) -> Result<String> {
    let mut sorted: Vec<&PathBuf> = paths.iter().collect();
    sorted.sort();
    let mut hasher = Sha256::new();
    for path in sorted {
        let bytes = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        hasher.update(path.to_string_lossy().as_bytes());
        hasher.update(b"\0");
        hasher.update(&bytes);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn resolve_ledger_root(out: Option<PathBuf>) -> PathBuf {
    out.or_else(|| std::env::var("LEDGER_ROOT").ok().map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from(DEFAULT_LEDGER_ROOT))
}

/// Every day string (`YYYY-MM-DD`) in the trailing window ending `today`,
/// oldest first. Duplicated `civil_from_days` from `rollup.rs` rather than
/// imported: `ledger/mod.rs` is compiled standalone via `#[path]` for the
/// binary crate's integration tests (see `rollup.rs`'s own copy's doc
/// comment), so a sibling top-level module cannot be reached from here.
fn window_days(today: &str, window_days: u32) -> Vec<String> {
    let today_ord = days_from_ymd(today);
    let span = window_days.max(1) as i64;
    (0..span)
        .rev()
        .map(|offset| {
            let (y, m, d) = civil_from_days(today_ord - offset);
            format!("{y:04}-{m:02}-{d:02}")
        })
        .collect()
}

fn days_from_ymd(date: &str) -> i64 {
    let parts: Vec<i64> = date.split('-').filter_map(|p| p.parse().ok()).collect();
    let (y, m, d) = (parts[0], parts[1], parts[2]);
    days_from_civil(y, m as u32, d as u32)
}

/// Inverse of `civil_from_days` (Howard Hinnant's civil-calendar algorithm).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy as u64;
    era * 146_097 + doe as i64 - 719_468
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn today_utc_date() -> String {
    let days = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
        / 86_400;
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}")
}

fn now_iso8601_utc() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let days = secs.div_euclid(86_400);
    let sod = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    let (h, min, s) = (sod / 3600, (sod % 3600) / 60, sod % 60);
    format!("{y:04}-{m:02}-{d:02}T{h:02}:{min:02}:{s:02}Z")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hook(p50: f64, n_calls: u64) -> HookRow {
        HookRow {
            n_calls,
            p50_ms: Some(p50),
        }
    }

    fn session(resend_bytes: u64) -> SessionRow {
        SessionRow { resend_bytes }
    }

    fn agent(total_tokens: u64, n_landed_prs: u64, join_method: &str) -> AgentRow {
        AgentRow {
            total_tokens,
            n_landed_prs,
            join_method: join_method.to_string(),
        }
    }

    #[test]
    fn weighted_median_favors_the_heavier_call_count() {
        // One hook called 100 times at 2ms, another called once at 50ms: the
        // median must land on the 2ms population, not split the difference.
        let pairs = vec![(2.0, 100), (50.0, 1)];
        assert_eq!(weighted_median(&pairs), Some(2.0));
    }

    #[test]
    fn compute_raw_metrics_excludes_unjoined_agent_rows() {
        let hooks = vec![hook(1.0, 10)];
        let sessions = vec![session(100)];
        let agents = vec![
            agent(300_000, 2, "session_url"),
            agent(999_999, 5, "unjoined"),
        ];
        let raw = compute_raw_metrics(&hooks, &sessions, &agents);
        let (value, n) = raw["tokens_per_landed_pr"];
        assert_eq!(value, Some(150_000.0));
        assert_eq!(n, 2);
    }

    fn baseline_metric(value: f64) -> BaselineMetric {
        BaselineMetric {
            value,
            n: 10,
            tolerance_pct: 5.0,
        }
    }

    #[test]
    fn value_equal_to_baseline_is_ratchet_held_not_pass() {
        // The design's own worked example: recording a baseline from the
        // current window, then immediately checking against it, must not
        // read as an improvement.
        let result = metric_result(Some(10.0), 5, Some(&baseline_metric(10.0)));
        assert_eq!(result.status, "RATCHET_HELD");
    }

    #[test]
    fn strictly_lower_value_passes() {
        let result = metric_result(Some(9.0), 5, Some(&baseline_metric(10.0)));
        assert_eq!(result.status, "PASS");
    }

    #[test]
    fn value_within_tolerance_above_baseline_is_ratchet_held() {
        let result = metric_result(Some(10.4), 5, Some(&baseline_metric(10.0)));
        assert_eq!(result.status, "RATCHET_HELD");
    }

    #[test]
    fn value_beyond_tolerance_is_regression() {
        let result = metric_result(Some(10.6), 5, Some(&baseline_metric(10.0)));
        assert_eq!(result.status, "REGRESSION");
    }

    #[test]
    fn missing_baseline_is_no_baseline() {
        let result = metric_result(Some(10.0), 5, None);
        assert_eq!(result.status, "NO_BASELINE");
    }

    #[test]
    fn missing_value_is_no_data() {
        let result = metric_result(None, 0, Some(&baseline_metric(10.0)));
        assert_eq!(result.status, "NO_DATA");
    }

    #[test]
    fn overall_status_is_worst_of_the_three() {
        let mut metrics = BTreeMap::new();
        metrics.insert(
            "a".to_string(),
            metric_result(Some(9.0), 5, Some(&baseline_metric(10.0))), // PASS
        );
        metrics.insert(
            "b".to_string(),
            metric_result(Some(10.0), 5, Some(&baseline_metric(10.0))), // RATCHET_HELD
        );
        metrics.insert(
            "c".to_string(),
            metric_result(Some(20.0), 5, Some(&baseline_metric(10.0))), // REGRESSION
        );
        assert_eq!(overall_status(&metrics), "REGRESSION");
    }

    #[test]
    fn window_days_covers_the_requested_span_ending_today() {
        let days = window_days("2026-09-13", 3);
        assert_eq!(days, vec!["2026-09-11", "2026-09-12", "2026-09-13"]);
    }

    #[test]
    fn civil_day_roundtrip() {
        let ord = days_from_civil(2026, 9, 13);
        assert_eq!(civil_from_days(ord), (2026, 9, 13));
    }
}
