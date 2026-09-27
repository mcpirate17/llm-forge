//! JSON value helpers shared by candidate policy parsers.

use serde_json::{Map, Value};

pub(super) type Object = Map<String, Value>;
pub(super) type Result<T> = std::result::Result<T, String>;

pub(super) fn table<'a>(value: &'a Value, error: &str) -> Result<&'a Object> {
    value.as_object().ok_or_else(|| error.to_owned())
}

pub(super) fn unknown(map: &Object, allowed: &[&str], prefix: &str) -> Result<()> {
    let keys: Vec<_> = map
        .keys()
        .filter(|key| !allowed.contains(&key.as_str()))
        .cloned()
        .collect();
    if keys.is_empty() {
        Ok(())
    } else {
        Err(format!("{prefix}: {}", repr_list(&keys)))
    }
}

pub(super) fn missing(map: &Object, required: &[&str], prefix: &str) -> Result<()> {
    let keys: Vec<_> = required
        .iter()
        .filter(|key| !map.contains_key(**key))
        .map(|key| (*key).to_owned())
        .collect();
    if keys.is_empty() {
        Ok(())
    } else {
        Err(format!("{prefix}: {}", repr_list(&keys)))
    }
}

pub(super) fn repr_list(values: &[String]) -> String {
    let mut sorted = values.to_vec();
    sorted.sort();
    format!(
        "[{}]",
        sorted
            .iter()
            .map(|item| format!("'{item}'"))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

pub(super) fn repr(value: &Value) -> String {
    match value {
        Value::String(s) => format!("'{s}'"),
        Value::Null => "None".into(),
        Value::Bool(b) => {
            if *b {
                "True".into()
            } else {
                "False".into()
            }
        }
        _ => value.to_string(),
    }
}

pub(super) fn py_string(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Null => "None".into(),
        Value::Bool(b) => {
            if *b {
                "True".into()
            } else {
                "False".into()
            }
        }
        _ => value.to_string(),
    }
}

pub(super) fn strings(value: &Value, field: &str, allow_empty: bool) -> Result<Vec<String>> {
    let Some(items) = value.as_array() else {
        return Err(format!("{field} must be an array of non-empty strings"));
    };
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let Some(s) = item.as_str() else {
            return Err(format!("{field} must be an array of non-empty strings"));
        };
        if s.is_empty() {
            return Err(format!("{field} must be an array of non-empty strings"));
        }
        out.push(s.to_owned());
    }
    if !allow_empty && out.is_empty() {
        return Err(format!("{field} must not be empty"));
    }
    Ok(out)
}

pub(super) fn positive(value: &Value, field: &str, max: Option<u64>) -> Result<u64> {
    let Some(n) = value.as_u64().filter(|n| *n > 0) else {
        return Err(format!("{field} must be a positive integer"));
    };
    if max.is_some_and(|limit| n > limit) {
        return Err(format!("{field} must be <= {}, got {n}", max.unwrap_or(0)));
    }
    Ok(n)
}

pub(super) fn nonnegative(value: &Value, field: &str, max: Option<u64>) -> Result<u64> {
    let Some(n) = value.as_u64() else {
        return Err(format!("{field} must be a non-negative integer"));
    };
    if max.is_some_and(|limit| n > limit) {
        return Err(format!("{field} must be <= {}, got {n}", max.unwrap_or(0)));
    }
    Ok(n)
}

pub(super) fn percent(value: &Value, field: &str) -> Result<f64> {
    let Some(n) = value.as_f64() else {
        return Err(format!("{field} must be numeric"));
    };
    if !(0.0..=100.0).contains(&n) {
        return Err(format!("{field} must be between 0 and 100"));
    }
    Ok(n)
}

pub(super) fn boolean(value: &Value, field: &str) -> Result<bool> {
    value
        .as_bool()
        .ok_or_else(|| format!("{field} must be a boolean"))
}

pub(super) fn severity(value: &Value, field: &str) -> Result<String> {
    match value.as_str() {
        Some(s @ ("critical" | "high" | "medium" | "low" | "info")) => Ok(s.to_owned()),
        _ => Err(field.to_owned()),
    }
}

pub(super) fn date(value: &Value, field: &str) -> Result<String> {
    let Some(s) = value.as_str() else {
        return Err(format!("{field} must be an ISO date"));
    };
    parse_date(s).ok_or_else(|| format!("{field} must be an ISO date"))
}

fn parse_date(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    match bytes {
        [y @ .., b'-', m1, m2, b'-', d1, d2] if y.len() == 4 => {
            let year = digits(y)?;
            let month = digits(&[*m1, *m2])?;
            let day = digits(&[*d1, *d2])?;
            calendar_date(year, month, day)
        }
        [y1, y2, y3, y4, b'-', b'W', w1, w2, b'-', d] | [y1, y2, y3, y4, b'W', w1, w2, d] => {
            let year = digits(&[*y1, *y2, *y3, *y4])?;
            let week = digits(&[*w1, *w2])?;
            let weekday = digits(&[*d])?;
            week_date(year, week, weekday)
        }
        [y1, y2, y3, y4, b'-', b'W', w1, w2] | [y1, y2, y3, y4, b'W', w1, w2] => {
            let year = digits(&[*y1, *y2, *y3, *y4])?;
            let week = digits(&[*w1, *w2])?;
            week_date(year, week, 1)
        }
        [y1, y2, y3, y4, m1, m2, d1, d2] => {
            let year = digits(&[*y1, *y2, *y3, *y4])?;
            let month = digits(&[*m1, *m2])?;
            let day = digits(&[*d1, *d2])?;
            calendar_date(year, month, day)
        }
        _ => None,
    }
}

fn digits(bytes: &[u8]) -> Option<u32> {
    if bytes.iter().all(u8::is_ascii_digit) {
        Some(bytes.iter().fold(0, |n, b| n * 10 + u32::from(b - b'0')))
    } else {
        None
    }
}

fn calendar_date(year: u32, month: u32, day: u32) -> Option<String> {
    let leap = year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400));
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => 0,
    };
    (year > 0 && year <= 9999 && day > 0 && day <= days)
        .then(|| format!("{year:04}-{month:02}-{day:02}"))
}

fn week_date(year: u32, week: u32, weekday: u32) -> Option<String> {
    if !(1..=9999).contains(&year) || !(1..=53).contains(&week) || !(1..=7).contains(&weekday) {
        return None;
    }
    let jan4 = days_from_civil(i64::from(year), 1, 4);
    let jan4_weekday = (jan4 + 3).rem_euclid(7);
    let day = jan4 - jan4_weekday + i64::from((week - 1) * 7 + weekday - 1);
    let (calendar_year, month, calendar_day) = civil_from_days(day);
    let (week_year, _) = iso_week_year(day);
    if week_year != i64::from(year) || !(1..=9999).contains(&calendar_year) {
        return None;
    }
    Some(format!("{calendar_year:04}-{month:02}-{calendar_day:02}"))
}

fn iso_week_year(day: i64) -> (i64, i64) {
    let weekday = (day + 3).rem_euclid(7);
    let thursday = day + 3 - weekday;
    let (year, _, _) = civil_from_days(thursday);
    (year, weekday)
}

fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = year - i64::from(month <= 2);
    let era = year.div_euclid(400);
    let yoe = year - era * 400;
    let doy = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn civil_from_days(day: i64) -> (i64, i64, i64) {
    let z = day + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    (year, month, day)
}

pub(super) fn exact_keys(map: &Object, required: &[&str]) -> bool {
    map.len() == required.len() && required.iter().all(|key| map.contains_key(*key))
}

pub(super) fn value<'a>(map: &'a Object, key: &str) -> &'a Value {
    map.get(key).unwrap_or(&Value::Null)
}

pub(super) fn default(map: &Object, key: &str, fallback: Value) -> Value {
    map.get(key).cloned().unwrap_or(fallback)
}

pub(super) fn insert<T: serde::Serialize>(map: &mut Object, key: &str, value: T) {
    map.insert(
        key.to_owned(),
        serde_json::to_value(value).expect("serializable policy value"),
    );
}
