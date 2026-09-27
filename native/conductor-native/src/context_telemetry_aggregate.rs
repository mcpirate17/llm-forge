//! Python-independent aggregation of context telemetry JSONL logs.
//!
//! The Python entry point supplies a clock reading and, for rich reports, the
//! historical temporary-file label. All filtering and report decisions live here.

use std::cmp::Reverse;
use std::collections::HashMap;
use std::fmt;
use std::fs::File;
use std::io::{self, BufRead, BufReader};

use chrono::{DateTime, Duration, NaiveDate, NaiveDateTime, Utc};
use serde::Serialize;
use serde_json::{json, Map, Value};

#[derive(Debug)]
pub enum ReportError {
    Io(io::Error),
    InvalidSince(String),
    InvalidTimestamp(String),
    NaiveTimestamp(String),
}

impl fmt::Display for ReportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "{error}"),
            Self::InvalidSince(since) => {
                write!(
                    f,
                    "--since must look like 30m, 2h or 1d (got {})",
                    python_repr(since)
                )
            }
            Self::InvalidTimestamp(stamp) => write!(f, "invalid clock timestamp: {stamp}"),
            Self::NaiveTimestamp(stamp) => write!(
                f,
                "can't compare offset-naive and offset-aware datetimes: {stamp}"
            ),
        }
    }
}

fn python_repr(value: &str) -> String {
    let quote = if value.contains('\'') && !value.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut rendered = String::from(quote);
    for character in value.chars() {
        match character {
            '\\' => rendered.push_str("\\\\"),
            '\n' => rendered.push_str("\\n"),
            '\r' => rendered.push_str("\\r"),
            '\t' => rendered.push_str("\\t"),
            character if character == quote => {
                rendered.push('\\');
                rendered.push(character);
            }
            character => rendered.push(character),
        }
    }
    rendered.push(quote);
    rendered
}

impl From<io::Error> for ReportError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

#[derive(Debug, Default, Serialize)]
struct SummaryRow {
    event: String,
    tool: String,
    count: u128,
    output_bytes: u128,
    output_tokens_estimate: u128,
    over_bound: u128,
    over_bound_bytes: u128,
    share: f64,
}

#[derive(Default)]
struct Summary {
    positions: HashMap<(String, String), usize>,
    rows: Vec<SummaryRow>,
}

fn python_string(value: Option<&Value>) -> String {
    match value {
        None => "?".to_owned(),
        Some(Value::String(text)) => text.clone(),
        Some(Value::Null) => "None".to_owned(),
        Some(Value::Bool(value)) => if *value { "True" } else { "False" }.to_owned(),
        Some(value) => value.to_string(),
    }
}

fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(number) => number.as_f64().is_some_and(|number| number != 0.0),
        Value::String(text) => !text.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(items) => !items.is_empty(),
    }
}

fn json_nonnegative(value: Option<&Value>) -> u128 {
    match value {
        Some(Value::Number(number)) => number.to_string().parse().unwrap_or(0),
        Some(Value::String(text)) if text.bytes().all(|byte| byte.is_ascii_digit()) => {
            text.parse().unwrap_or(0)
        }
        _ => 0,
    }
}

fn numeric_integer(value: Option<&Value>) -> Option<i128> {
    match value? {
        Value::Bool(value) => Some(i128::from(*value)),
        Value::Number(number) => number
            .to_string()
            .parse::<i128>()
            .ok()
            .or_else(|| number.as_f64().map(|value| value as i128)),
        _ => None,
    }
}

fn integer_or_zero(value: Option<&Value>) -> i128 {
    match value {
        Some(Value::String(text)) => text.parse().unwrap_or(0),
        value => numeric_integer(value).unwrap_or(0),
    }
}

impl Summary {
    fn add(&mut self, item: &Map<String, Value>, bound_bytes: i128) {
        let event = python_string(item.get("event"));
        let tool = python_string(item.get("tool"));
        let index = *self
            .positions
            .entry((event.clone(), tool.clone()))
            .or_insert_with(|| {
                self.rows.push(SummaryRow {
                    event,
                    tool,
                    ..SummaryRow::default()
                });
                self.rows.len() - 1
            });
        let row = &mut self.rows[index];
        let output_bytes = json_nonnegative(item.get("output_bytes"));
        row.count += 1;
        row.output_bytes += output_bytes;
        row.output_tokens_estimate += json_nonnegative(item.get("output_tokens_estimate"));
        if (output_bytes as i128) > bound_bytes {
            row.over_bound += 1;
            row.over_bound_bytes += ((output_bytes as i128) - bound_bytes) as u128;
        }
    }

    fn finish(mut self, files: &[String], bound_bytes: i128) -> Value {
        self.rows.sort_by_key(|row| Reverse(row.output_bytes));
        let output_bytes: u128 = self.rows.iter().map(|row| row.output_bytes).sum();
        for row in &mut self.rows {
            row.share = if output_bytes == 0 {
                0.0
            } else {
                rounded(row.output_bytes as f64 / output_bytes as f64, 4)
            };
        }
        let hook_context_bytes: u128 = self
            .rows
            .iter()
            .filter(|row| row.event == "HookContext")
            .map(|row| row.output_bytes)
            .sum();
        let events: u128 = self.rows.iter().map(|row| row.count).sum();
        json!({
            "schema_version": "llm.context-telemetry.summary.v1",
            "files": files,
            "bound_bytes": bound_bytes,
            "events": events,
            "output_bytes": output_bytes,
            "hook_context_bytes": hook_context_bytes,
            "rows": self.rows,
        })
    }
}

fn scan(
    paths: &[String],
    skip_missing: bool,
    mut add: impl FnMut(&Map<String, Value>) -> Result<(), ReportError>,
) -> Result<(), ReportError> {
    for path in paths {
        if skip_missing && !std::path::Path::new(path).is_file() {
            continue;
        }
        let file = match File::open(path) {
            Ok(file) => file,
            Err(error) if skip_missing && error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        let mut reader = BufReader::new(file);
        let mut line = Vec::new();
        while reader.read_until(b'\n', &mut line)? != 0 {
            let parsed = if skip_missing {
                serde_json::from_str::<Value>(&String::from_utf8_lossy(&line))
            } else {
                serde_json::from_slice::<Value>(&line)
            };
            if let Ok(Value::Object(item)) = parsed {
                add(&item)?;
            }
            line.clear();
        }
    }
    Ok(())
}

pub fn summarize(paths: &[String], bound_bytes: i128) -> io::Result<Value> {
    let mut summary = Summary::default();
    scan(paths, false, |item| {
        summary.add(item, bound_bytes);
        Ok(())
    })
    .map_err(|error| match error {
        ReportError::Io(error) => error,
        error => io::Error::other(error.to_string()),
    })?;
    Ok(summary.finish(paths, bound_bytes))
}

fn parse_stamp(stamp: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(stamp)
        .map(|value| value.with_timezone(&Utc))
        .or_else(|_| {
            DateTime::parse_from_rfc3339(&stamp.replace(' ', "T"))
                .map(|value| value.with_timezone(&Utc))
        })
        .ok()
}

pub fn parse_since(since: &str, now_iso: &str) -> Result<String, ReportError> {
    let trimmed = since.trim();
    let Some(unit) = trimmed.chars().last() else {
        return Err(ReportError::InvalidSince(since.to_owned()));
    };
    let digits = &trimmed[..trimmed.len() - unit.len_utf8()];
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(ReportError::InvalidSince(since.to_owned()));
    }
    let factor: i64 = match unit {
        's' => 1,
        'm' => 60,
        'h' => 3_600,
        'd' => 86_400,
        _ => return Err(ReportError::InvalidSince(since.to_owned())),
    };
    let amount = digits
        .parse::<i64>()
        .map_err(|_| ReportError::InvalidSince(since.to_owned()))?;
    let seconds = amount
        .checked_mul(factor)
        .ok_or_else(|| ReportError::InvalidSince(since.to_owned()))?;
    let now =
        parse_stamp(now_iso).ok_or_else(|| ReportError::InvalidTimestamp(now_iso.to_owned()))?;
    let cutoff = now
        .checked_sub_signed(Duration::seconds(seconds))
        .ok_or_else(|| ReportError::InvalidSince(since.to_owned()))?;
    Ok(cutoff.to_rfc3339())
}

fn event_time(item: &Map<String, Value>) -> Result<Option<DateTime<Utc>>, ReportError> {
    let Some(stamp) = item.get("timestamp").and_then(Value::as_str) else {
        return Ok(None);
    };
    if let Some(time) = parse_stamp(stamp) {
        return Ok(Some(time));
    }
    let naive = ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%d %H:%M:%S%.f"]
        .iter()
        .any(|pattern| NaiveDateTime::parse_from_str(stamp, pattern).is_ok())
        || NaiveDate::parse_from_str(stamp, "%Y-%m-%d").is_ok();
    if naive {
        return Err(ReportError::NaiveTimestamp(stamp.to_owned()));
    }
    Ok(None)
}

fn percentile(values: &[f64], pct: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut ordered = values.to_vec();
    ordered.sort_by(f64::total_cmp);
    let rank = (ordered.len() - 1) as f64 * pct;
    let low = rank as usize;
    let high = (low + 1).min(ordered.len() - 1);
    ordered[low] + (ordered[high] - ordered[low]) * (rank - low as f64)
}

fn rounded(value: f64, digits: usize) -> f64 {
    // Decimal formatting rounds the original binary float directly. Scaling
    // first can move a representational edge across a tie (e.g. 2.675).
    format!("{value:.digits$}").parse().unwrap_or(value)
}

#[derive(Default)]
struct Rich {
    sessions: Vec<(String, u128, i128, i128)>,
    session_positions: HashMap<String, usize>,
    hooks: Vec<(String, Vec<f64>)>,
    hook_positions: HashMap<String, usize>,
    instructions_resends: u128,
    instructions_bytes: i128,
    hashes: HashMap<String, u128>,
    templates: Vec<(String, String, u128, i128)>,
    template_positions: HashMap<(String, String), usize>,
}

impl Rich {
    fn add(&mut self, item: &Map<String, Value>) {
        let session = item
            .get("session_id")
            .filter(|value| truthy(value))
            .map_or_else(|| "unknown".to_owned(), |value| python_string(Some(value)));
        let session_index = *self
            .session_positions
            .entry(session.clone())
            .or_insert_with(|| {
                self.sessions.push((session, 0, 0, 0));
                self.sessions.len() - 1
            });
        let bucket = &mut self.sessions[session_index];
        bucket.1 += 1;
        if let Some(output_bytes) = numeric_integer(item.get("output_bytes")) {
            bucket.2 += output_bytes;
            if item.get("event").and_then(Value::as_str) == Some("HookContext") {
                bucket.3 += output_bytes;
            }
        }

        if item.get("event").and_then(Value::as_str) == Some("HookTiming") {
            if let Some(ms) = item.get("elapsed_ms").and_then(|value| match value {
                Value::Number(number) => number.as_f64(),
                Value::Bool(value) => Some(f64::from(u8::from(*value))),
                _ => None,
            }) {
                let hook = python_string(item.get("tool"));
                let index = *self.hook_positions.entry(hook.clone()).or_insert_with(|| {
                    self.hooks.push((hook, Vec::new()));
                    self.hooks.len() - 1
                });
                self.hooks[index].1.push(ms);
            }
        }

        if item.get("category").and_then(Value::as_str) == Some("instructions") {
            self.instructions_resends += 1;
            self.instructions_bytes += integer_or_zero(item.get("output_bytes"));
            if let Some(hash) = item.get("content_hash").filter(|value| truthy(value)) {
                *self.hashes.entry(python_string(Some(hash))).or_default() += 1;
            }
        }

        if item.get("event").and_then(Value::as_str) == Some("HookContext") {
            let hook = python_string(item.get("tool"));
            let event = python_string(item.get("hook_event"));
            let index = *self
                .template_positions
                .entry((hook.clone(), event.clone()))
                .or_insert_with(|| {
                    self.templates.push((hook, event, 0, 0));
                    self.templates.len() - 1
                });
            self.templates[index].2 += 1;
            self.templates[index].3 += integer_or_zero(item.get("output_bytes"));
        }
    }

    fn finish(mut self, top: i64) -> (Value, Value, Value, Value) {
        let mut sessions = Map::new();
        for (id, events, output_bytes, hook_context_bytes) in self.sessions {
            sessions.insert(id, json!({"events":events,"output_bytes":output_bytes,"hook_context_bytes":hook_context_bytes}));
        }
        let mut by_hook = Map::new();
        let mut total_ms = 0.0;
        for (hook, values) in self.hooks {
            let sum = rounded(values.iter().sum::<f64>(), 3);
            total_ms += sum;
            by_hook.insert(
                hook,
                json!({
                    "count": values.len(), "total_ms": sum,
                    "p50_ms": rounded(percentile(&values, 0.5), 3),
                    "p90_ms": rounded(percentile(&values, 0.9), 3),
                }),
            );
        }
        let hook_ms = json!({"total_ms": rounded(total_ms, 3), "by_hook": by_hook});
        let repeat_resends: u128 = self
            .hashes
            .values()
            .map(|count| count.saturating_sub(1))
            .sum();
        let instructions = json!({
            "resends": self.instructions_resends,
            "bytes": self.instructions_bytes,
            "distinct_content": self.hashes.len(),
            "repeat_resends": repeat_resends,
        });
        self.templates.sort_by_key(|row| Reverse(row.3));
        let keep = if top >= 0 {
            (top as usize).min(self.templates.len())
        } else {
            self.templates
                .len()
                .saturating_sub(top.unsigned_abs() as usize)
        };
        let templates: Vec<Value> = self
            .templates
            .into_iter()
            .take(keep)
            .map(|(hook, hook_event, count, output_bytes)| {
                json!({
                    "hook":hook,"hook_event":hook_event,"count":count,"output_bytes":output_bytes,
                })
            })
            .collect();
        (
            Value::Object(sessions),
            hook_ms,
            instructions,
            Value::Array(templates),
        )
    }
}

pub fn summarize_report(
    paths: &[String],
    bound_bytes: i128,
    since: Option<&str>,
    top: i64,
    now_iso: &str,
    report_file: &str,
) -> Result<Value, ReportError> {
    let cutoff = since
        .map(|value| parse_since(value, now_iso))
        .transpose()?
        .and_then(|value| parse_stamp(&value));
    let mut summary = Summary::default();
    let mut rich = Rich::default();
    scan(paths, true, |item| {
        if let Some(cutoff) = cutoff {
            if event_time(item)?.is_none_or(|when| when < cutoff) {
                return Ok(());
            }
        }
        summary.add(item, bound_bytes);
        rich.add(item);
        Ok(())
    })?;
    let mut report = summary.finish(&[report_file.to_owned()], bound_bytes);
    let (sessions, hook_ms, instructions, templates) = rich.finish(top);
    let object = report.as_object_mut().expect("summary object");
    object.insert(
        "since".to_owned(),
        json!(since
            .filter(|value| !value.is_empty())
            .unwrap_or("all-time")),
    );
    object.insert("sessions".to_owned(), sessions);
    object.insert("hook_ms".to_owned(), hook_ms);
    object.insert("instructions".to_owned(), instructions);
    object.insert("top_message_templates".to_owned(), templates);
    Ok(report)
}

fn required<'a>(value: &'a Value, key: &str) -> Result<&'a Value, String> {
    value
        .get(key)
        .ok_or_else(|| format!("missing report field: {key}"))
}

fn int_text(value: &Value) -> Result<String, String> {
    value
        .as_i64()
        .map(|n| n.to_string())
        .or_else(|| value.as_u64().map(|n| n.to_string()))
        .or_else(|| value.as_str().map(str::to_owned))
        .ok_or_else(|| format!("expected integer, got {value}"))
}

fn grouped(value: &Value) -> Result<String, String> {
    let raw = int_text(value)?;
    let (sign, digits) = raw
        .strip_prefix('-')
        .map_or(("", raw.as_str()), |digits| ("-", digits));
    let mut reversed = String::new();
    for (index, digit) in digits.chars().rev().enumerate() {
        if index > 0 && index % 3 == 0 {
            reversed.push(',');
        }
        reversed.push(digit);
    }
    Ok(format!(
        "{sign}{}",
        reversed.chars().rev().collect::<String>()
    ))
}

fn left(value: &str, width: usize) -> String {
    format!(
        "{value}{}",
        " ".repeat(width.saturating_sub(value.chars().count()))
    )
}

fn right(value: &str, width: usize) -> String {
    format!(
        "{}{value}",
        " ".repeat(width.saturating_sub(value.chars().count()))
    )
}

fn field(value: &Value, key: &str) -> Result<String, String> {
    let value = required(value, key)?;
    Ok(value
        .as_str()
        .map_or_else(|| python_string(Some(value)), str::to_owned))
}

pub fn format_summary(summary: &Value) -> Result<String, String> {
    let mut lines = vec![format!(
        "events={} output_bytes={} hook_context_bytes={} bound={}",
        int_text(required(summary, "events")?)?,
        grouped(required(summary, "output_bytes")?)?,
        grouped(required(summary, "hook_context_bytes")?)?,
        int_text(required(summary, "bound_bytes")?)?,
    )];
    lines.push(format!(
        "{}{}{}{}{}{}{}",
        left("event", 14),
        left("tool", 22),
        right("count", 7),
        right("bytes", 13),
        right("share", 7),
        right(">bound", 8),
        right("bytes>bound", 13)
    ));
    let rows = required(summary, "rows")?
        .as_array()
        .ok_or("rows must be an array")?;
    for row in rows {
        let share = required(row, "share")?
            .as_f64()
            .ok_or("share must be a number")?;
        let percentage = format!("{:.1}%", share * 100.0);
        lines.push(format!(
            "{}{}{}{}{}{}{}",
            left(&field(row, "event")?, 14),
            left(&field(row, "tool")?, 22),
            right(&int_text(required(row, "count")?)?, 7),
            right(&grouped(required(row, "output_bytes")?)?, 13),
            right(&percentage, 7),
            right(&int_text(required(row, "over_bound")?)?, 8),
            right(&grouped(required(row, "over_bound_bytes")?)?, 13)
        ));
    }
    Ok(lines.join("\n"))
}

pub fn format_rich_summary(report: &Value) -> Result<String, String> {
    let mut lines = vec![
        format_summary(report)?,
        format!("since={}", field(report, "since")?),
    ];
    let sessions = required(report, "sessions")?
        .as_object()
        .ok_or("sessions must be an object")?;
    lines.push(format!("sessions={}", sessions.len()));
    for (id, totals) in sessions {
        lines.push(format!(
            "  {} events={} output_bytes={} hook_context_bytes={}",
            left(id, 20),
            right(&int_text(required(totals, "events")?)?, 6),
            right(&grouped(required(totals, "output_bytes")?)?, 10),
            right(&grouped(required(totals, "hook_context_bytes")?)?, 8)
        ));
    }
    let hook_ms = required(report, "hook_ms")?;
    let total_ms = required(hook_ms, "total_ms")?
        .as_f64()
        .ok_or("total_ms must be a number")?;
    lines.push(format!("hook_ms total={total_ms:.1}"));
    let hooks = required(hook_ms, "by_hook")?
        .as_object()
        .ok_or("by_hook must be an object")?;
    let mut ranked: Vec<_> = hooks.iter().collect();
    ranked.sort_by(|a, b| {
        b.1["total_ms"]
            .as_f64()
            .unwrap_or(0.0)
            .total_cmp(&a.1["total_ms"].as_f64().unwrap_or(0.0))
    });
    for (hook, stats) in ranked {
        let total = required(stats, "total_ms")?
            .as_f64()
            .ok_or("total_ms must be a number")?;
        let p50 = required(stats, "p50_ms")?
            .as_f64()
            .ok_or("p50_ms must be a number")?;
        let p90 = required(stats, "p90_ms")?
            .as_f64()
            .ok_or("p90_ms must be a number")?;
        lines.push(format!(
            "  {} count={} total_ms={} p50_ms={} p90_ms={}",
            left(hook, 28),
            right(&int_text(required(stats, "count")?)?, 5),
            right(&format!("{total:.1}"), 9),
            right(&format!("{p50:.1}"), 7),
            right(&format!("{p90:.1}"), 7)
        ));
    }
    let instructions = required(report, "instructions")?;
    lines.push(format!(
        "instructions resends={} bytes={} distinct_content={} repeat_resends={}",
        int_text(required(instructions, "resends")?)?,
        grouped(required(instructions, "bytes")?)?,
        int_text(required(instructions, "distinct_content")?)?,
        int_text(required(instructions, "repeat_resends")?)?
    ));
    lines.push("top_message_templates:".to_owned());
    let templates = required(report, "top_message_templates")?
        .as_array()
        .ok_or("top_message_templates must be an array")?;
    for row in templates {
        lines.push(format!(
            "  {}{}count={} bytes={}",
            left(&field(row, "hook")?, 24),
            left(&field(row, "hook_event")?, 16),
            right(&int_text(required(row, "count")?)?, 5),
            right(&grouped(required(row, "output_bytes")?)?, 10)
        ));
    }
    Ok(lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::rounded;

    #[test]
    fn decimal_rounding_keeps_binary_edges_on_the_correct_side() {
        assert_eq!(rounded(2.675, 2), 2.67);
        assert_eq!(rounded(1.005, 2), 1.0);
    }
}
