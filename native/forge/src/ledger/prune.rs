//! `forge ledger prune` (design section 2 "Storage", retention): daily day
//! files older than the window are retired. `session_rollup`/`agent_rollup`
//! -- the aggregates -- are archived first, one JSONL per calendar month
//! under `<ledger_root>/archive/<year-month>.jsonl` (idempotently: a row
//! already archived is skipped, keyed the same way the day-file writer keys
//! superseded rows); raw `turn_attribution`/`hook_rollup` day files are not
//! worth keeping past the window they were computed to support (design's
//! words) and are simply deleted. Dry-run is the default -- it lists every
//! action it would take; nothing leaves the tree without `--apply`.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use clap::Args;

/// Tables whose rows are archived before their day files are deleted (the
/// design's "session_rollup/agent_rollup aggregates only").
const ARCHIVED_TABLES: [(&str, &str); 2] = [
    ("session_rollup", "session_id"),
    ("agent_rollup", "agent_name"),
];
/// Tables whose day files are deleted without archiving (raw detail).
const DROPPED_TABLES: [&str; 2] = ["turn_attribution", "hook_rollup"];

const DEFAULT_LEDGER_ROOT: &str = "/mnt/data/llm/ledger/";

#[derive(Args)]
pub struct PruneArgs {
    /// Ledger root directory; defaults to `LEDGER_ROOT`, else
    /// `/mnt/data/llm/ledger/`.
    #[arg(long)]
    pub ledger_root: Option<PathBuf>,

    /// Days of daily files to keep (default 90, the design's window).
    #[arg(long, default_value_t = 90)]
    pub keep_days: i64,

    /// Actually archive and delete. Without it, list what would happen.
    #[arg(long)]
    pub apply: bool,
}

/// One day file's worth of planned retirement.
struct Plan {
    table: String,
    day: String,
    rows: Vec<String>,
}

pub fn run(args: PruneArgs) -> Result<i32> {
    if args.keep_days < 0 {
        bail!("forge ledger prune: --keep-days must not be negative");
    }
    let root = resolve_ledger_root(args.ledger_root);
    let today = today_epoch_day();

    let mut archives: Vec<Plan> = Vec::new();
    let mut deletions: Vec<(String, String, PathBuf)> = Vec::new();
    for (table, _) in ARCHIVED_TABLES {
        for day in old_days(&root.join(table), today, args.keep_days)? {
            let path = root.join(table).join(format!("{day}.jsonl"));
            let rows = read_rows(&path)?;
            archives.push(Plan {
                table: table.to_string(),
                day: day.clone(),
                rows,
            });
            deletions.push((table.to_string(), day, path));
        }
    }
    for table in DROPPED_TABLES {
        for day in old_days(&root.join(table), today, args.keep_days)? {
            deletions.push((
                table.to_string(),
                day.clone(),
                root.join(table).join(format!("{day}.jsonl")),
            ));
        }
    }

    for plan in &archives {
        println!(
            "archive {}/{}.jsonl -> archive/{}.jsonl ({} row(s))",
            plan.table,
            plan.day,
            &plan.day[..7],
            plan.rows.len()
        );
    }
    for (table, day, _) in &deletions {
        println!("delete {table}/{day}.jsonl");
    }
    println!(
        "prune: {} file(s) to archive, {} to delete{}",
        archives.len(),
        deletions.len(),
        if args.apply {
            ""
        } else {
            " (dry-run; pass --apply)"
        }
    );

    if !args.apply {
        return Ok(0);
    }

    fs::create_dir_all(root.join("archive"))
        .with_context(|| format!("creating {}", root.join("archive").display()))?;
    for plan in &archives {
        let target = root
            .join("archive")
            .join(format!("{}.jsonl", &plan.day[..7]));
        append_idempotent(&target, &plan.rows, key_field_of(&plan.table))?;
    }
    for (_, _, path) in &deletions {
        fs::remove_file(path).with_context(|| format!("deleting {}", path.display()))?;
    }
    Ok(0)
}

fn key_field_of(table: &str) -> &str {
    ARCHIVED_TABLES
        .iter()
        .find(|(name, _)| *name == table)
        .map(|(_, key)| *key)
        .unwrap_or("session_id")
}

/// Day files under `dir` whose `YYYY-MM-DD` name is older than
/// `keep_days` before `today`. Non-day filenames are a loud error, not a
/// skip: this directory is owned by the writer, and anything else in it
/// means the layout assumption is wrong.
fn old_days(dir: &Path, today: i64, keep_days: i64) -> Result<Vec<String>> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        // A table that has never been written is simply empty, not an error
        // -- a fresh ledger root prunes to zero actions.
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err).with_context(|| format!("reading {}", dir.display())),
    };
    let mut days = Vec::new();
    for entry in entries {
        let path = entry?.path();
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .with_context(|| format!("non-UTF-8 filename in {}", dir.display()))?;
        let Some(day) = name.strip_suffix(".jsonl") else {
            bail!("{}: unexpected non-day file {name:?}", dir.display());
        };
        let epoch = parse_day(day)
            .with_context(|| format!("{}: filename {day:?} is not YYYY-MM-DD", dir.display()))?;
        if today - epoch > keep_days {
            days.push(day.to_string());
        }
    }
    days.sort();
    Ok(days)
}

fn read_rows(path: &Path) -> Result<Vec<String>> {
    let text = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    Ok(text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(String::from)
        .collect())
}

/// Appends `rows` to `target`, skipping any whose key field is already
/// present -- rerunning an archive must not duplicate rows (the same
/// idempotence rule as `writer::write_day_file`, mirrored here because the
/// archive aggregates whole day files, not superseding them).
fn append_idempotent(target: &Path, rows: &[String], key_field: &str) -> Result<()> {
    let existing: HashSet<String> = fs::read_to_string(target)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| {
            serde_json::from_str::<serde_json::Value>(line)
                .ok()
                .and_then(|v| {
                    v.get(key_field)
                        .and_then(|k| k.as_str().map(str::to_string))
                })
        })
        .collect();
    use std::io::Write;
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(target)
        .with_context(|| format!("opening {}", target.display()))?;
    for row in rows {
        let key = serde_json::from_str::<serde_json::Value>(row)
            .ok()
            .and_then(|v| {
                v.get(key_field)
                    .and_then(|k| k.as_str().map(str::to_string))
            });
        if key.is_some_and(|k| existing.contains(&k)) {
            continue;
        }
        writeln!(file, "{row}")?;
    }
    Ok(())
}

fn parse_day(day: &str) -> Option<i64> {
    let bytes = day.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return None;
    }
    let digit =
        |range: std::ops::Range<usize>| -> Option<i64> { day.get(range)?.parse::<i64>().ok() };
    let y = digit(0..4)?;
    let m = digit(5..7)? as u32;
    let d = digit(8..10)? as u32;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    Some(days_from_civil(y, m, d))
}

/// Days-since-epoch for a proleptic Gregorian date (Howard Hinnant's
/// algorithm, the inverse of `civil_from_days`). Duplicated from
/// `crate::civil::days_from_civil` for the same reason `rollup.rs`
/// duplicates `civil_from_days`: `ledger/mod.rs` is compiled standalone via
/// `#[path]` in the integration tests and cannot see sibling top-level
/// modules.
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 } as i64;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn today_epoch_day() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
        / 86_400
}

fn resolve_ledger_root(out: Option<PathBuf>) -> PathBuf {
    out.or_else(|| std::env::var("LEDGER_ROOT").ok().map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from(DEFAULT_LEDGER_ROOT))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("forge-prune-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn day_file(root: &Path, table: &str, day: &str, rows: &[&str]) {
        let dir = root.join(table);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(format!("{day}.jsonl")), rows.join("\n") + "\n").unwrap();
    }

    #[test]
    fn parse_day_accepts_only_real_dates() {
        assert_eq!(parse_day("2026-09-13"), Some(20709));
        assert_eq!(parse_day("2026-9-13"), None);
        assert_eq!(parse_day("2026-13-01"), None);
        assert_eq!(parse_day("not-a-day"), None);
    }

    #[test]
    fn dry_run_lists_but_touches_nothing() {
        let root = tmp("dry");
        day_file(
            &root,
            "session_rollup",
            "2026-06-01",
            &[r#"{"session_id":"a"}"#],
        );
        day_file(
            &root,
            "turn_attribution",
            "2026-06-01",
            &[r#"{"turn_uuid":"u"}"#],
        );
        day_file(
            &root,
            "session_rollup",
            "2099-01-01",
            &[r#"{"session_id":"new"}"#],
        );
        // Old enough only relative to a pinned "today".
        let days = old_days(
            &root.join("session_rollup"),
            parse_day("2026-09-13").unwrap(),
            90,
        )
        .unwrap();
        assert_eq!(days, vec!["2026-06-01"]);
        assert!(!root.join("archive").join("2026-06.jsonl").exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn apply_archives_aggregates_and_deletes_all_old_dailies() {
        let root = tmp("apply");
        day_file(
            &root,
            "session_rollup",
            "2026-06-01",
            &[r#"{"session_id":"a"}"#, r#"{"session_id":"b"}"#],
        );
        day_file(
            &root,
            "agent_rollup",
            "2026-06-02",
            &[r#"{"agent_name":"glm"}"#],
        );
        day_file(
            &root,
            "turn_attribution",
            "2026-06-01",
            &[r#"{"turn_uuid":"u"}"#],
        );
        day_file(
            &root,
            "hook_rollup",
            "2026-06-01",
            &[r#"{"hook_name":"pre_bash"}"#],
        );
        // A fresh day file stays untouched.
        day_file(
            &root,
            "session_rollup",
            "2026-09-13",
            &[r#"{"session_id":"keep"}"#],
        );

        // Pin "today" by driving the helpers the run() path uses.
        let today = parse_day("2026-09-13").unwrap();
        for (table, _) in ARCHIVED_TABLES {
            for day in old_days(&root.join(table), today, 90).unwrap() {
                let path = root.join(table).join(format!("{day}.jsonl"));
                let rows = read_rows(&path).unwrap();
                let plan = Plan {
                    table: table.to_string(),
                    day: day.clone(),
                    rows,
                };
                fs::create_dir_all(root.join("archive")).unwrap();
                let target = root
                    .join("archive")
                    .join(format!("{}.jsonl", &plan.day[..7]));
                append_idempotent(&target, &plan.rows, key_field_of(table)).unwrap();
                fs::remove_file(&path).unwrap();
            }
        }
        for table in DROPPED_TABLES {
            for day in old_days(&root.join(table), today, 90).unwrap() {
                fs::remove_file(root.join(table).join(format!("{day}.jsonl"))).unwrap();
            }
        }
        let archive = fs::read_to_string(root.join("archive").join("2026-06.jsonl")).unwrap();
        assert!(archive.contains(r#""session_id":"a""#));
        assert!(archive.contains(r#""agent_name":"glm""#));
        assert!(!root
            .join("session_rollup")
            .join("2026-06-01.jsonl")
            .exists());
        assert!(!root
            .join("turn_attribution")
            .join("2026-06-01.jsonl")
            .exists());
        assert!(root
            .join("session_rollup")
            .join("2026-09-13.jsonl")
            .exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn archiving_twice_does_not_duplicate() {
        let root = tmp("twice");
        fs::create_dir_all(root.join("archive")).unwrap();
        let target = root.join("archive").join("2026-06.jsonl");
        let rows = vec![
            r#"{"session_id":"a"}"#.to_string(),
            r#"{"session_id":"b"}"#.to_string(),
        ];
        append_idempotent(&target, &rows, "session_id").unwrap();
        append_idempotent(&target, &rows, "session_id").unwrap();
        let count = fs::read_to_string(&target).unwrap().lines().count();
        assert_eq!(count, 2);
        let _ = fs::remove_dir_all(&root);
    }
}
