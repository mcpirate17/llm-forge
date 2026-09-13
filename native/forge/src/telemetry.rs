//! Best-effort delegation telemetry: one JSON line per `forge hook` call, iff
//! `CONTEXT_TELEMETRY_PATH` is set.

use serde::Serialize;
use std::env;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::civil::civil_from_days;

#[derive(Serialize)]
struct DelegationEvent<'a> {
    event: &'a str,
    elapsed_ms: f64,
    delegated: bool,
    ts: String,
}

/// Appends one JSON line recording that `event` was delegated to the Python
/// dispatcher, iff `CONTEXT_TELEMETRY_PATH` names where to append it -- the same
/// env var `tooling.hooks.dispatch.adapters._telemetry_path` reads to override
/// `conductor.context_telemetry.DEFAULT_PATH`. No env var, no write: forge does not
/// invent a default path of its own, since that default lives under the `conductor`
/// package's own directory, which is a Python-package-layout fact this binary has
/// no reason to know and every reason not to hardcode.
///
/// Best-effort like the Python telemetry adapter it mirrors
/// (`adapters._telemetry` catches its own write errors to stderr rather than
/// failing the hook): a telemetry write must never turn a working hook into a
/// broken one.
pub fn record_delegation(event: &str, elapsed_ms: f64) {
    record(event, elapsed_ms, true);
}

/// Appends one JSON line recording that `event` was answered fully natively
/// -- forge never started Python for this call at all -- so telemetry can
/// distinguish "delegated" and "fully native" calls the same way `dispatch.py`
/// already distinguishes hooks it ran itself from ones it spliced in.
pub fn record_native(event: &str, elapsed_ms: f64) {
    record(event, elapsed_ms, false);
}

fn record(event: &str, elapsed_ms: f64, delegated: bool) {
    let Ok(path) = env::var("CONTEXT_TELEMETRY_PATH") else {
        return;
    };
    if path.trim().is_empty() {
        return;
    }
    let record = DelegationEvent {
        event,
        elapsed_ms,
        delegated,
        ts: iso8601_utc_millis(SystemTime::now()),
    };
    if let Err(err) = append_line(Path::new(&path), &record) {
        eprintln!("forge: context telemetry unavailable: {err}");
    }
}

fn append_line(path: &Path, record: &DelegationEvent) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    let mut line = serde_json::to_string(record).expect("DelegationEvent always serializes");
    line.push('\n');
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    file.write_all(line.as_bytes())
}

/// UTC timestamp with millisecond precision, formatted like Python's
/// `datetime.now(UTC).isoformat(timespec="milliseconds")`
/// (e.g. `2026-09-12T14:23:01.123+00:00`). See `civil.rs` for the calendar
/// arithmetic (shared with `crg_gate.rs`'s claim-expiry math) instead of a
/// `chrono` dependency for one timestamp format.
fn iso8601_utc_millis(now: SystemTime) -> String {
    let dur = now.duration_since(UNIX_EPOCH).unwrap_or_default();
    let millis_total = dur.as_millis() as i64;
    let secs = millis_total.div_euclid(1000);
    let millis = millis_total.rem_euclid(1000);
    let days = secs.div_euclid(86_400);
    let secs_of_day = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    let h = secs_of_day / 3600;
    let min = (secs_of_day % 3600) / 60;
    let s = secs_of_day % 60;
    format!("{y:04}-{m:02}-{d:02}T{h:02}:{min:02}:{s:02}.{millis:03}+00:00")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_with_millisecond_precision_and_utc_offset() {
        let ts = UNIX_EPOCH + std::time::Duration::from_millis(1_789_222_981_123);
        let formatted = iso8601_utc_millis(ts);
        assert!(formatted.ends_with(".123+00:00"), "{formatted}");
        assert_eq!(formatted.len(), "2026-09-12T14:23:01.123+00:00".len());
    }
}
