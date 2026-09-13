//! A UTC instant as fractional seconds since the epoch, with the narrow slice
//! of ISO-8601 parsing/formatting `crg_gate.rs`'s claim-expiry math needs:
//! `datetime.fromisoformat(...)` (offset required) and the `%Y-%m-%d %H:%M`,
//! `%Y-%m-%dT%H:%M`, `%H:%M` and full-isoformat renderings `ownership.py`
//! prints in denial messages and writes to the activity sidecar. Every
//! timestamp this codebase's own tooling ever writes is a UTC-aware Python
//! `datetime.isoformat()` (`+00:00`, microseconds omitted when zero); this
//! parser is deliberately a little more permissive than that (`Z`, other
//! offsets, no-colon offsets) to match what `datetime.fromisoformat` itself
//! accepts, since a claim file is data another process wrote.

use crate::civil::{civil_from_days, days_from_civil};
use std::time::{SystemTime, UNIX_EPOCH};

/// UTC epoch seconds, `now()`.
pub fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// `datetime.fromisoformat(raw)` then `.astimezone(timezone.utc)`, requiring
/// an explicit offset (mirrors `_instant`'s `tzinfo is None` check) -- `None`
/// for anything unparseable or offset-free.
pub fn parse(raw: &str) -> Option<f64> {
    let raw = raw.trim();
    let bytes = raw.as_bytes();
    if bytes.len() < 19 {
        return None;
    }
    let date = &raw[0..10];
    let sep = bytes[10];
    if sep != b'T' && sep != b't' && sep != b' ' {
        return None;
    }
    let (y, m, d) = parse_date(date)?;
    let rest = &raw[11..];
    let (h, min, s, frac, offset_str) = split_time_and_offset(rest)?;
    let offset_seconds = parse_offset(offset_str)?;
    if h > 23 || min > 59 || s > 59 || m == 0 || m > 12 || d == 0 || d > 31 {
        return None;
    }
    let days = days_from_civil(y, m, d);
    let secs_of_day = (h as i64) * 3600 + (min as i64) * 60 + (s as i64);
    let local_epoch = (days * 86_400 + secs_of_day) as f64 + frac;
    Some(local_epoch - offset_seconds as f64)
}

fn parse_date(date: &str) -> Option<(i64, u32, u32)> {
    let bytes = date.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return None;
    }
    let y: i64 = date[0..4].parse().ok()?;
    let m: u32 = date[5..7].parse().ok()?;
    let d: u32 = date[8..10].parse().ok()?;
    Some((y, m, d))
}

/// Splits `HH:MM:SS[.ffffff][offset]` into its parts. The offset is whatever
/// is left once a `Z`/`z`, or a `+`/`-` sign, is found after the seconds
/// field -- required, per `parse`'s contract.
fn split_time_and_offset(rest: &str) -> Option<(u32, u32, u32, f64, &str)> {
    let bytes = rest.as_bytes();
    if bytes.len() < 8 || bytes[2] != b':' || bytes[5] != b':' {
        return None;
    }
    let h: u32 = rest[0..2].parse().ok()?;
    let min: u32 = rest[3..5].parse().ok()?;
    let s: u32 = rest[6..8].parse().ok()?;
    let mut cursor = 8usize;
    let mut frac = 0.0f64;
    if bytes.get(cursor) == Some(&b'.') {
        let start = cursor + 1;
        let mut end = start;
        while bytes.get(end).is_some_and(u8::is_ascii_digit) {
            end += 1;
        }
        if end == start {
            return None;
        }
        let digits = &rest[start..end];
        let numerator: f64 = digits.parse().ok()?;
        frac = numerator / 10f64.powi(digits.len() as i32);
        cursor = end;
    }
    if cursor >= bytes.len() {
        return None; // offset is required
    }
    Some((h, min, s, frac, &rest[cursor..]))
}

/// `+HH:MM`, `+HHMM`, `-HH:MM`, `-HHMM`, `Z` or `z` -> signed offset seconds.
fn parse_offset(raw: &str) -> Option<i32> {
    if raw.eq_ignore_ascii_case("z") {
        return Some(0);
    }
    let bytes = raw.as_bytes();
    if bytes.is_empty() {
        return None;
    }
    let sign = match bytes[0] {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let digits: String = raw[1..].chars().filter(|c| *c != ':').collect();
    if digits.len() != 4 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let hh: i32 = digits[0..2].parse().ok()?;
    let mm: i32 = digits[2..4].parse().ok()?;
    if hh > 23 || mm > 59 {
        return None;
    }
    Some(sign * (hh * 3600 + mm * 60))
}

fn civil_hms(instant: f64) -> (i64, u32, u32, u32, u32, u32, f64) {
    let floor_secs = instant.floor();
    let frac = instant - floor_secs;
    let total = floor_secs as i64;
    let days = total.div_euclid(86_400);
    let secs_of_day = total.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    let h = (secs_of_day / 3600) as u32;
    let min = ((secs_of_day % 3600) / 60) as u32;
    let s = (secs_of_day % 60) as u32;
    (y, m, d, h, min, s, frac)
}

/// `moment.isoformat()` for a UTC-aware `moment`: microseconds included only
/// when nonzero, always `+00:00` (never `Z`).
pub fn isoformat_utc(instant: f64) -> String {
    let (y, mo, d, h, min, s, frac) = civil_hms(instant);
    let micros = (frac * 1_000_000.0).round() as u32;
    if micros == 0 {
        format!("{y:04}-{mo:02}-{d:02}T{h:02}:{min:02}:{s:02}+00:00")
    } else {
        format!("{y:04}-{mo:02}-{d:02}T{h:02}:{min:02}:{s:02}.{micros:06}+00:00")
    }
}

/// `%Y-%m-%d %H:%M`.
pub fn format_ymd_hm(instant: f64) -> String {
    let (y, mo, d, h, min, _s, _frac) = civil_hms(instant);
    format!("{y:04}-{mo:02}-{d:02} {h:02}:{min:02}")
}

/// `%Y-%m-%dT%H:%M`.
pub fn format_iso_minutes(instant: f64) -> String {
    let (y, mo, d, h, min, _s, _frac) = civil_hms(instant);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{min:02}")
}

/// `%H:%M`.
pub fn format_hm(instant: f64) -> String {
    let (_y, _mo, _d, h, min, _s, _frac) = civil_hms(instant);
    format!("{h:02}:{min:02}")
}

/// `datetime.now(UTC).isoformat(timespec="milliseconds")`:
/// `YYYY-MM-DDTHH:MM:SS.mmm+00:00`, milliseconds always shown (`.000` too)
/// and truncated, never rounded -- the stamp `conductor.context_telemetry`
/// puts at the front of every record it writes.
pub fn isoformat_millis_utc(instant: f64) -> String {
    let (y, mo, d, h, min, s, frac) = civil_hms(instant);
    let millis = (frac * 1000.0) as u32; // truncation, like isoformat's timespec
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{min:02}:{s:02}.{millis:03}+00:00")
}

/// `%Y%m%dT%H%M%S`, UTC: the compact stamp `tool_quiet::save` embeds in a
/// spill filename. Python's `time.strftime` (no explicit time tuple) renders
/// this in the process's local timezone rather than UTC; that divergence
/// never matters in production since the two implementations are never both
/// live for the same call (`forge` supersedes the Python hook body entirely
/// once native), only a test that pins the very same stamp string on both
/// sides needs the two to agree, and it does that by injecting one literal
/// value rather than by comparing each implementation's own clock.
///
/// This crate has no lib target, so `handlers.rs`'s own use of this function
/// is invisible to a `tests/*.rs` binary that pulls in `instant.rs` via
/// `#[path]` without also including `handlers.rs` -- `#[allow(dead_code)]`
/// matches `crg_gate::unused_constants_reference` and
/// `write_targets::unused_from_some_test_binaries`'s established use of the
/// same pattern in this crate.
#[allow(dead_code)]
pub fn format_compact_utc(instant: f64) -> String {
    let (y, mo, d, h, min, s, _frac) = civil_hms(instant);
    format!("{y:04}{mo:02}{d:02}T{h:02}{min:02}{s:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_utc_offset_with_microseconds() {
        let raw = "2026-09-12T14:23:01.123456+00:00";
        let instant = parse(raw).expect("parses");
        assert_eq!(isoformat_utc(instant), raw);
    }

    #[test]
    fn parses_and_reformats_without_microseconds() {
        let raw = "2026-09-12T14:23:01+00:00";
        let instant = parse(raw).expect("parses");
        assert_eq!(isoformat_utc(instant), raw);
    }

    #[test]
    fn z_suffix_is_utc() {
        let a = parse("2026-09-12T14:23:01Z").unwrap();
        let b = parse("2026-09-12T14:23:01+00:00").unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn a_positive_offset_shifts_earlier_in_utc() {
        // 14:23 local at +02:00 is 12:23 UTC.
        let local = parse("2026-09-12T14:23:00+02:00").unwrap();
        let utc = parse("2026-09-12T12:23:00+00:00").unwrap();
        assert!((local - utc).abs() < 1e-9);
    }

    #[test]
    fn missing_offset_is_rejected() {
        assert_eq!(parse("2026-09-12T14:23:01"), None);
    }

    #[test]
    fn garbage_is_rejected() {
        assert_eq!(parse("not a date"), None);
    }

    #[test]
    fn impossible_field_values_are_rejected() {
        // Each input violates exactly one field constraint; `parse` must
        // reject every one of them, which is also what pins the validation
        // chain together as independent checks (any `&&` in that chain would
        // let the single-violation inputs through).
        for raw in [
            "2026-09-12T24:00:00Z",
            "2026-09-12T00:60:00Z",
            "2026-09-12T00:00:60Z",
            "2026-00-12T00:00:00Z",
            "2026-13-12T00:00:00Z",
            "2026-09-00T00:00:00Z",
            "2026-09-32T00:00:00Z",
        ] {
            assert_eq!(parse(raw), None, "must reject {raw}");
        }
    }

    #[test]
    fn duration_arithmetic_round_trips_through_formatting() {
        let created = parse("2026-09-12T00:00:00+00:00").unwrap();
        let two_hours_later = created + 2.0 * 3600.0;
        assert_eq!(format_iso_minutes(two_hours_later), "2026-09-12T02:00");
        assert_eq!(format_hm(two_hours_later), "02:00");
        assert_eq!(format_ymd_hm(two_hours_later), "2026-09-12 02:00");
    }
}
