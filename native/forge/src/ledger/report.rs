//! `forge ledger report` (Phase 3 step 3, item 4): one row per tier over
//! every `task_dispatch` row under `--ledger-root`, aggregating exactly the
//! numbers item 5's real run needs to paste into a PR body -- n dispatches,
//! share of billed subagent tokens, median billed, `over_cap` count/rate,
//! `rework_rate`, `ci_red_rate`. Every rate the outcome join (`outcome.rs`)
//! can leave `null` for (no `ci_history` cache joined) is printed as `null`,
//! never `0` -- a `0%` rework rate and "we never measured rework" must never
//! look the same on this table.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::Args;
use serde::Serialize;
use serde_json::Value;

#[derive(Args)]
pub struct ReportArgs {
    /// Ledger root to read `task_dispatch/*.jsonl` under.
    #[arg(long = "ledger-root")]
    pub ledger_root: PathBuf,

    /// Only rows whose `dispatch_ts` (falling back to `first_ts`) falls in
    /// the trailing N days from now; omit to use every row on disk.
    #[arg(long = "window-days")]
    pub window_days: Option<u64>,

    /// One JSON object per tier on stdout instead of the terminal table.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct TierReportRow {
    pub tier: String,
    pub n_dispatches: u64,
    /// Share (0.0-1.0) of this window's total billed subagent tokens this
    /// tier accounts for.
    pub share_of_billed_tokens: f64,
    pub median_billed_tokens: Option<u64>,
    pub over_cap_count: u64,
    pub over_cap_rate: f64,
    /// `None` whenever not one row in this tier carries a `required_rework`
    /// value (the ci_history cache was never joined) -- never `0.0`.
    pub rework_rate: Option<f64>,
    /// Same absence rule as `rework_rate`, over `ci_red_on_first_push`.
    pub ci_red_rate: Option<f64>,
    /// Share of this tier's *warn-mode* rows (`mode == "warn"`) whose
    /// routing verdict was `deny` but were let through anyway
    /// (`applied == false`) -- `docs/routing.md`'s "warn-only mode" bullet.
    /// `None` when this tier has no warn-mode rows at all, same absence
    /// rule as `rework_rate`/`ci_red_rate`: a warn-only install that was
    /// never exercised must never look like "would never have denied".
    pub would_deny: Option<f64>,
    /// Share of this tier's warn-mode rows whose verdict would have
    /// changed the model (`decision == "allow"` with a model opinion) but
    /// was left unapplied (`applied == false`). Same `None`-when-unmeasured
    /// rule as `would_deny`.
    pub would_route: Option<f64>,
}

pub fn run(args: ReportArgs) -> Result<i32> {
    let rows = load_task_dispatch_rows(&args.ledger_root, args.window_days)?;
    let report = build_report(&rows);
    if args.json {
        for row in &report {
            println!("{}", serde_json::to_string(row)?);
        }
    } else {
        print_table(&report);
    }
    Ok(0)
}

/// Reads every `task_dispatch/*.jsonl` row under `ledger_root`, optionally
/// filtered to the trailing `window_days` by `dispatch_ts` (falling back to
/// `first_ts` when a row has no dispatch timestamp -- an agent_upsert-only
/// row per `agent_upsert.rs`'s documented debt).
fn load_task_dispatch_rows(ledger_root: &Path, window_days: Option<u64>) -> Result<Vec<Value>> {
    let dir = ledger_root.join("task_dispatch");
    let mut rows = Vec::new();
    if !dir.is_dir() {
        return Ok(rows);
    }
    let cutoff = window_days.map(|days| {
        let now_days = super::rollup::today_utc_date();
        (now_days, days)
    });
    let entries = std::fs::read_dir(&dir).with_context(|| format!("reading {}", dir.display()))?;
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        for line in text.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let row: Value = serde_json::from_str(line)
                .with_context(|| format!("parsing a line of {}", path.display()))?;
            if let Some((today, days)) = &cutoff {
                let ts = row
                    .get("dispatch_ts")
                    .and_then(Value::as_str)
                    .or_else(|| row.get("first_ts").and_then(Value::as_str));
                if !within_window(ts, today, *days) {
                    continue;
                }
            }
            rows.push(row);
        }
    }
    Ok(rows)
}

/// Day-granularity window check: `ts`'s `YYYY-MM-DD` prefix must be within
/// `days` of `today` (also `YYYY-MM-DD`). A row with no usable timestamp is
/// excluded from a windowed report -- it cannot honestly be placed in or
/// out of the window.
fn within_window(ts: Option<&str>, today: &str, days: u64) -> bool {
    let Some(ts) = ts.filter(|t| t.len() >= 10) else {
        return false;
    };
    let Some(ts_days) = days_since_epoch_str(&ts[..10]) else {
        return false;
    };
    let Some(today_days) = days_since_epoch_str(today) else {
        return false;
    };
    today_days.saturating_sub(ts_days) <= days as i64
}

/// Parses a `YYYY-MM-DD` date into days-since-epoch (1970-01-01), Howard
/// Hinnant's civil-calendar algorithm -- the same one `rollup.rs`'s
/// `civil_from_days` inverts, duplicated here rather than imported for the
/// same reason `rollup.rs` gives: `ledger/mod.rs` is compiled standalone via
/// `#[path]` in `tests/ledger_fixtures.rs`, which cannot see a private
/// helper in a sibling module either way, so a shared helper would need its
/// own `pub(crate)` surface for four lines of arithmetic.
fn days_since_epoch_str(date: &str) -> Option<i64> {
    let mut parts = date.splitn(3, '-');
    let y: i64 = parts.next()?.parse().ok()?;
    let m: i64 = parts.next()?.parse().ok()?;
    let d: i64 = parts.next()?.parse().ok()?;
    let y2 = if m <= 2 { y - 1 } else { y };
    let era = if y2 >= 0 { y2 } else { y2 - 399 } / 400;
    let yoe = y2 - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146_097 + doe - 719_468)
}

fn build_report(rows: &[Value]) -> Vec<TierReportRow> {
    let mut by_tier: BTreeMap<String, Vec<&Value>> = BTreeMap::new();
    for row in rows {
        let tier = row
            .get("tier")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string();
        by_tier.entry(tier).or_default().push(row);
    }

    let total_billed: u64 = rows
        .iter()
        .filter_map(|r| r.get("billed_tokens").and_then(Value::as_u64))
        .sum();

    by_tier
        .into_iter()
        .map(|(tier, tier_rows)| build_tier_row(tier, &tier_rows, total_billed))
        .collect()
}

fn build_tier_row(tier: String, rows: &[&Value], total_billed: u64) -> TierReportRow {
    let n_dispatches = rows.len() as u64;
    let mut billed: Vec<u64> = rows
        .iter()
        .filter_map(|r| r.get("billed_tokens").and_then(Value::as_u64))
        .collect();
    billed.sort_unstable();
    let tier_billed_sum: u64 = billed.iter().sum();
    let share_of_billed_tokens = if total_billed == 0 {
        0.0
    } else {
        tier_billed_sum as f64 / total_billed as f64
    };
    let median_billed_tokens = median(&billed);

    let over_cap_count = rows
        .iter()
        .filter(|r| r.get("over_cap").and_then(Value::as_bool) == Some(true))
        .count() as u64;
    let over_cap_rate = rate(over_cap_count, n_dispatches);

    let rework_rate = bool_field_rate(rows, "required_rework");
    let ci_red_rate = bool_field_rate(rows, "ci_red_on_first_push");
    let would_deny = warn_mode_rate(rows, |r| {
        r.get("decision").and_then(Value::as_str) == Some("deny")
            && r.get("applied").and_then(Value::as_bool) == Some(false)
    });
    let would_route = warn_mode_rate(rows, |r| {
        r.get("decision").and_then(Value::as_str) == Some("allow")
            && r.get("applied").and_then(Value::as_bool) == Some(false)
    });

    TierReportRow {
        tier,
        n_dispatches,
        share_of_billed_tokens,
        median_billed_tokens,
        over_cap_count,
        over_cap_rate,
        rework_rate,
        ci_red_rate,
        would_deny,
        would_route,
    }
}

/// `None` when zero rows in this tier carry a non-null value for `field` --
/// the join never ran, so there is no rate to report, not a `0.0` one.
fn bool_field_rate(rows: &[&Value], field: &str) -> Option<f64> {
    let known: Vec<bool> = rows
        .iter()
        .filter_map(|r| r.get(field).and_then(Value::as_bool))
        .collect();
    if known.is_empty() {
        return None;
    }
    let true_count = known.iter().filter(|b| **b).count();
    Some(true_count as f64 / known.len() as f64)
}

/// Same "`None` unless measured" convention as `bool_field_rate`, but the
/// population is warn-mode rows specifically (`mode == "warn"`): a row this
/// offline sweep built from a bare transcript, or one written in `enforce`
/// mode, has no opinion on what warn mode *would have* done, so it is
/// excluded from both the numerator and the denominator rather than
/// counted as a `false`.
fn warn_mode_rate(rows: &[&Value], predicate: impl Fn(&Value) -> bool) -> Option<f64> {
    let warn_rows: Vec<&&Value> = rows
        .iter()
        .filter(|r| r.get("mode").and_then(Value::as_str) == Some("warn"))
        .collect();
    if warn_rows.is_empty() {
        return None;
    }
    let true_count = warn_rows.iter().filter(|r| predicate(r)).count();
    Some(true_count as f64 / warn_rows.len() as f64)
}

fn rate(count: u64, total: u64) -> f64 {
    if total == 0 {
        0.0
    } else {
        count as f64 / total as f64
    }
}

fn median(sorted: &[u64]) -> Option<u64> {
    if sorted.is_empty() {
        return None;
    }
    let mid = sorted.len() / 2;
    if sorted.len() % 2 == 1 {
        Some(sorted[mid])
    } else {
        Some((sorted[mid - 1] + sorted[mid]) / 2)
    }
}

/// Ten columns, kept under 100 characters wide (`docs/routing.md`'s
/// terminal-table requirement) -- verified at 94 chars for a representative
/// row in `report.rs`'s tests.
fn print_table(report: &[TierReportRow]) {
    println!(
        "{:<8} {:>5} {:>7} {:>10} {:>8} {:>9} {:>11} {:>11} {:>8} {:>8}",
        "tier",
        "n",
        "share",
        "median",
        "over_cap",
        "cap_rate",
        "rework_rt",
        "ci_red_rt",
        "wd_deny",
        "wd_route"
    );
    for row in report {
        println!(
            "{:<8} {:>5} {:>6.1}% {:>10} {:>8} {:>8.1}% {:>11} {:>11} {:>8} {:>8}",
            row.tier,
            row.n_dispatches,
            row.share_of_billed_tokens * 100.0,
            row.median_billed_tokens
                .map(|v| v.to_string())
                .unwrap_or_else(|| "null".to_string()),
            row.over_cap_count,
            row.over_cap_rate * 100.0,
            fmt_pct_opt(row.rework_rate),
            fmt_pct_opt(row.ci_red_rate),
            fmt_pct_opt(row.would_deny),
            fmt_pct_opt(row.would_route),
        );
    }
}

fn fmt_pct_opt(value: Option<f64>) -> String {
    match value {
        Some(v) => format!("{:.1}%", v * 100.0),
        None => "null".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(
        tier: &str,
        billed: u64,
        over_cap: bool,
        rework: Option<bool>,
        ci_red: Option<bool>,
    ) -> Value {
        let mut v = serde_json::json!({
            "tier": tier,
            "billed_tokens": billed,
            "over_cap": over_cap,
        });
        if let Some(r) = rework {
            v["required_rework"] = serde_json::json!(r);
        }
        if let Some(c) = ci_red {
            v["ci_red_on_first_push"] = serde_json::json!(c);
        }
        v
    }

    /// A warn-mode `task_dispatch` row (`mode`/`decision`/`applied` all
    /// set), for `would_deny`/`would_route`'s tests -- `row()` above never
    /// sets these three, so its rows fall outside the warn-mode population
    /// by construction, same as an offline-sweep or enforce-mode row would.
    fn warn_row(tier: &str, decision: &str, applied: bool) -> Value {
        serde_json::json!({
            "tier": tier,
            "billed_tokens": 100,
            "over_cap": false,
            "mode": "warn",
            "decision": decision,
            "applied": applied,
        })
    }

    #[test]
    fn absent_outcome_data_reports_null_never_zero() {
        let rows = vec![row("haiku", 1000, false, None, None)];
        let refs: Vec<&Value> = rows.iter().collect();
        let report = build_report(&rows);
        assert_eq!(report.len(), 1);
        assert_eq!(report[0].rework_rate, None);
        assert_eq!(report[0].ci_red_rate, None);
        let _ = refs;
    }

    #[test]
    fn over_cap_and_share_are_computed_per_tier() {
        let rows = vec![
            row("haiku", 100, false, Some(false), Some(false)),
            row("sonnet", 300, true, Some(true), Some(true)),
            row("sonnet", 100, false, Some(false), Some(false)),
        ];
        let report = build_report(&rows);
        let haiku = report.iter().find(|r| r.tier == "haiku").unwrap();
        let sonnet = report.iter().find(|r| r.tier == "sonnet").unwrap();
        assert_eq!(haiku.n_dispatches, 1);
        assert!((haiku.share_of_billed_tokens - 0.2).abs() < 1e-9);
        assert_eq!(sonnet.n_dispatches, 2);
        assert_eq!(sonnet.over_cap_count, 1);
        assert!((sonnet.over_cap_rate - 0.5).abs() < 1e-9);
        assert_eq!(sonnet.rework_rate, Some(0.5));
        assert_eq!(sonnet.ci_red_rate, Some(0.5));
    }

    #[test]
    fn median_handles_even_and_odd_counts() {
        assert_eq!(median(&[]), None);
        assert_eq!(median(&[5]), Some(5));
        assert_eq!(median(&[1, 2, 3, 4]), Some(2)); // (2+3)/2
        assert_eq!(median(&[1, 2, 3]), Some(2));
    }

    #[test]
    fn no_warn_mode_rows_leaves_would_deny_and_would_route_null() {
        let rows = vec![row("haiku", 100, false, None, None)];
        let report = build_report(&rows);
        assert_eq!(report[0].would_deny, None);
        assert_eq!(report[0].would_route, None);
    }

    #[test]
    fn would_deny_counts_only_unapplied_warn_mode_denies() {
        let rows = vec![
            warn_row("sonnet", "deny", false),
            warn_row("sonnet", "allow", true),
            row("sonnet", 100, false, None, None), // outside the warn population
        ];
        let report = build_report(&rows);
        let sonnet = report.iter().find(|r| r.tier == "sonnet").unwrap();
        // Two warn-mode rows in the denominator, one bare row excluded.
        assert_eq!(sonnet.would_deny, Some(0.5));
        assert_eq!(sonnet.would_route, Some(0.0));
    }

    #[test]
    fn would_route_counts_unapplied_warn_mode_allows() {
        let rows = vec![
            warn_row("opus", "allow", false), // would have rerouted
            warn_row("opus", "allow", true),  // a plain no-op allow
            warn_row("opus", "deny", false),  // a would-deny, not a would-route
        ];
        let report = build_report(&rows);
        let opus = report.iter().find(|r| r.tier == "opus").unwrap();
        assert_eq!(opus.would_deny, Some(1.0 / 3.0));
        assert_eq!(opus.would_route, Some(1.0 / 3.0));
    }

    #[test]
    fn print_table_stays_under_100_columns_wide() {
        let rows = vec![
            warn_row("sonnet", "deny", false),
            row("haiku", 12345, false, Some(false), None),
        ];
        let report = build_report(&rows);
        // print_table only writes to stdout; render the same two format
        // strings here to check width without capturing process stdout.
        let header = format!(
            "{:<8} {:>5} {:>7} {:>10} {:>8} {:>9} {:>11} {:>11} {:>8} {:>8}",
            "tier",
            "n",
            "share",
            "median",
            "over_cap",
            "cap_rate",
            "rework_rt",
            "ci_red_rt",
            "wd_deny",
            "wd_route"
        );
        assert!(
            header.len() < 100,
            "header is {} columns wide",
            header.len()
        );
        for row in &report {
            let line = format!(
                "{:<8} {:>5} {:>6.1}% {:>10} {:>8} {:>8.1}% {:>11} {:>11} {:>8} {:>8}",
                row.tier,
                row.n_dispatches,
                row.share_of_billed_tokens * 100.0,
                row.median_billed_tokens
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "null".to_string()),
                row.over_cap_count,
                row.over_cap_rate * 100.0,
                fmt_pct_opt(row.rework_rate),
                fmt_pct_opt(row.ci_red_rate),
                fmt_pct_opt(row.would_deny),
                fmt_pct_opt(row.would_route),
            );
            assert!(
                line.len() < 100,
                "row is {} columns wide: {line}",
                line.len()
            );
        }
    }
}
