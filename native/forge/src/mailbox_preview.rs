//! Bounded read-only mailbox presentation shared by CLI and session briefs.
use anyhow::{bail, Result};
use conductor_native::a2a_store::{self, Preview, Store};
use serde_json::{json, Value};
use std::path::Path;

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

pub fn envelope(
    snapshot: (Vec<Preview>, i64, i64),
    agent: &str,
    unread: bool,
    chars: usize,
    max_chars: usize,
) -> Result<Value> {
    let (mut rows, total, bytes) = snapshot;
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

/// Read only. Missing stores are optional; no initialization or acknowledgment.
pub fn brief(host: &Path, identity: &str) -> Result<Option<Value>> {
    a2a_store::validate_identity(identity)?;
    Store::open(&host.join(".agents/a2a"), identity, false)?
        .map(|store| envelope(store.previews(true, 8, 140)?, identity, true, 140, 1200))
        .transpose()
}
