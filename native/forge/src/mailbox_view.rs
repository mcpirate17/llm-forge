//! Character-bounded JSON presentation matching conductor.a2a_cli.

use super::store::Store;
use anyhow::{Context, Result};
pub use forge::mailbox_preview::compact_json;
use forge::mailbox_preview::envelope;
use serde_json::Value;

pub fn inbox(
    store: &Store,
    agent: &str,
    unread: bool,
    limit: usize,
    chars: usize,
    max_chars: usize,
) -> Result<Value> {
    envelope(
        store.previews(unread, limit, chars)?,
        agent,
        unread,
        chars,
        max_chars,
    )
}

pub fn presentation(
    store: &Store,
    agent: &str,
    replay_unread: bool,
    limit: usize,
    chars: usize,
    max_chars: usize,
) -> Result<Value> {
    envelope(
        store.presentation_previews(replay_unread, limit, chars)?,
        agent,
        true,
        chars,
        max_chars,
    )
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
