//! Character-bounded JSON presentation matching conductor.a2a_cli.

use super::store::{Preview, Store};
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

pub fn compact_json(value: &Value) -> Result<String> {
    let mut value = value.clone();
    value.sort_all_objects();
    Ok(serde_json::to_string(&value)?)
}

fn summary(value: &str, chars: usize) -> String {
    let collapsed = conductor_native::normalize_a2a_text(value);
    if collapsed.chars().count() <= chars {
        return collapsed;
    }
    let prefix: String = collapsed.chars().take(chars - 1).collect();
    format!("{}…", prefix.trim_end())
}

fn message(row: &Preview, chars: usize) -> Value {
    json!({"id":row.id,"from":row.sender,"at":row.at,"thread":row.thread,
        "status":row.status,"requires_response":row.requires_response,
        "summary":summary(&row.summary,chars),"raw_bytes":row.raw_bytes})
}

pub fn inbox(
    store: &Store,
    agent: &str,
    unread: bool,
    limit: usize,
    chars: usize,
    max_chars: usize,
) -> Result<Value> {
    let (mut rows, total, bytes) = store.previews(unread, limit, chars)?;
    let mut summary_chars = chars;
    loop {
        let messages: Vec<_> = rows.iter().map(|row| message(row, summary_chars)).collect();
        let payload = json!({"schema_version":1,"authority":"bounded-a2a-inbox","agent":agent,
            "unread_only":unread,"total":total,"shown":messages.len(),"omitted":total-messages.len() as i64,
            "raw_bytes_not_injected":bytes,"messages":messages});
        if compact_json(&payload)?.chars().count() <= max_chars {
            return Ok(payload);
        }
        if summary_chars > 32 {
            summary_chars = 32.max(summary_chars / 2);
        } else if rows.pop().is_none() {
            bail!("--max-chars {max_chars} is too small for the compact inbox envelope");
        }
    }
}

fn text<'a>(value: &'a Value, field: &str) -> Result<&'a str> {
    value[field]
        .as_str()
        .with_context(|| format!("missing text field {field}"))
}

pub fn render_inbox(payload: &Value) -> Result<String> {
    let mut lines = vec![format!(
        "A2A compact agent={} total={} shown={} omitted={}",
        text(payload, "agent")?,
        payload["total"],
        payload["shown"],
        payload["omitted"]
    )];
    for row in payload["messages"]
        .as_array()
        .context("missing inbox messages")?
    {
        let response = if row["requires_response"].as_bool() == Some(true) {
            " response=yes"
        } else {
            ""
        };
        lines.push(format!(
            "[{}] {} from={} thread={}{response}\n  {}",
            text(row, "status")?,
            text(row, "id")?,
            text(row, "from")?,
            text(row, "thread")?,
            text(row, "summary")?
        ));
    }
    if payload["omitted"].as_i64().unwrap_or(0) != 0 {
        lines.push(format!(
            "(+{} more; rerun or show by message id)",
            payload["omitted"]
        ));
    }
    lines.push(format!(
        "raw bytes withheld from context: {}",
        payload["raw_bytes_not_injected"]
    ));
    Ok(lines.join("\n"))
}

pub fn render_message(row: &Value) -> Result<String> {
    let mut output = format!(
        "{} {} from={} to={} at={}\n{}",
        text(row, "direction")?,
        text(row, "message_id")?,
        text(row, "sender")?,
        text(row, "recipient")?,
        text(row, "created_at")?,
        text(row, "body")?
    );
    if let Some(data) = row["data_json"].as_str().filter(|s| !s.is_empty()) {
        output.push_str(&format!("\ndata={data}"));
    }
    Ok(output)
}
