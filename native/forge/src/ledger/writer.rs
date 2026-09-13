//! Storage for the cost ledger's derived tables (`docs/design/cost_ledger.md`
//! section 2 "Storage"): append-only JSONL under
//! `<ledger_root>/<table>/<utc-date>.jsonl`, one `serde_json` line per
//! record -- the same append-a-line shape `telemetry.rs::append_line` already
//! uses, no new dependency.
//!
//! Idempotence (design step 2, item 2): rerunning a rollup over the same
//! input must replace that input's rows for that day, not duplicate them.
//! Chosen over a side `.index` file: rewrite the day file filtered by key,
//! so there is exactly one source of truth on disk (the day file itself)
//! and no second file that can drift out of sync with it. The cost is
//! O(day-file-size) per write instead of an index's O(1) lookup, accepted
//! at this table's expected daily volume (one file per table per day,
//! reread and rewritten once per `forge ledger rollup` invocation that
//! touches that day).

use std::collections::HashSet;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use serde_json::Value;

/// Writes `new_rows` into `<ledger_root>/<table>/<day>.jsonl`, replacing any
/// existing row whose `key_field` value matches one of `new_rows`'s, keeping
/// every other existing row untouched. A line that fails to parse as JSON,
/// or that lacks `key_field`, is kept as-is rather than silently dropped --
/// this writer only ever removes a row it can positively identify as
/// superseded.
pub fn write_day_file(
    ledger_root: &Path,
    table: &str,
    day: &str,
    key_field: &str,
    new_rows: &[Value],
) -> Result<()> {
    if new_rows.is_empty() {
        return Ok(());
    }
    let dir = ledger_root.join(table);
    fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let file_path = dir.join(format!("{day}.jsonl"));

    let new_keys: HashSet<&str> = new_rows
        .iter()
        .filter_map(|row| row.get(key_field).and_then(Value::as_str))
        .collect();

    let mut kept_lines: Vec<String> = Vec::new();
    if file_path.exists() {
        let existing = fs::read_to_string(&file_path)
            .with_context(|| format!("reading {}", file_path.display()))?;
        for line in existing.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let supersede = serde_json::from_str::<Value>(line)
                .ok()
                .and_then(|v| v.get(key_field).and_then(Value::as_str).map(str::to_string))
                .is_some_and(|key| new_keys.contains(key.as_str()));
            if !supersede {
                kept_lines.push(line.to_string());
            }
        }
    }

    let mut out = String::new();
    for line in &kept_lines {
        out.push_str(line);
        out.push('\n');
    }
    for row in new_rows {
        out.push_str(&serde_json::to_string(row)?);
        out.push('\n');
    }

    // Write-then-rename so a reader never observes a half-written day file.
    let tmp_path = dir.join(format!("{day}.jsonl.tmp-{}", std::process::id()));
    fs::write(&tmp_path, out).with_context(|| format!("writing {}", tmp_path.display()))?;
    fs::rename(&tmp_path, &file_path)
        .with_context(|| format!("renaming {} -> {}", tmp_path.display(), file_path.display()))?;
    Ok(())
}
