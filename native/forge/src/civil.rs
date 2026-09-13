//! Days-since-epoch <-> proleptic Gregorian calendar conversion, and the
//! inverse. Plain integer arithmetic -- Howard Hinnant's civil-calendar
//! algorithms -- instead of a `chrono` dependency, shared by `telemetry.rs`
//! (timestamp formatting) and `crg_gate.rs`'s claim-expiry math (both instant
//! parsing and formatting).
//!
//! Source: <http://howardhinnant.github.io/date_algorithms.html>
//! (`civil_from_days` / `days_from_civil`).

/// Days-since-epoch (1970-01-01) to a proleptic Gregorian (year, month, day).
pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
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

/// A proleptic Gregorian (year, month, day) to days-since-epoch (1970-01-01).
/// The inverse of [`civil_from_days`].
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64;
    let m = m as u64;
    let d = d as u64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe as i64 - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_from_days_matches_known_epoch_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
        // 2026-09-12 -> 20708 days since epoch (checked against Python's `date`).
        assert_eq!(civil_from_days(20708), (2026, 9, 12));
        // A leap-year boundary: 2024-02-29.
        assert_eq!(civil_from_days(19782), (2024, 2, 29));
    }

    #[test]
    fn days_from_civil_is_the_inverse_of_civil_from_days() {
        for days in [-100_000i64, -1, 0, 1, 19782, 20708, 100_000] {
            let (y, m, d) = civil_from_days(days);
            assert_eq!(days_from_civil(y, m, d), days, "roundtrip for {days}");
        }
    }

    #[test]
    fn days_from_civil_matches_known_dates() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2026, 9, 12), 20708);
        assert_eq!(days_from_civil(2024, 2, 29), 19782);
    }
}
