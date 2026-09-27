//! SQLite interoperability with conductor.agent_a2a. Never creates or migrates state.

use anyhow::{bail, ensure, Context, Result};
use rusqlite::{params, Connection, OpenFlags, OptionalExtension, TransactionBehavior};
use serde_json::{json, Value};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::queue::PreparedMessage;

const MAX_METADATA_BYTES: i64 = 4096;
const MESSAGE_COLUMNS: &str = "message_id,direction,sender,recipient,body,data_json,created_at,received_at,delivery_status,status_reason,read_at";

pub struct Store {
    connection: Connection,
}

pub struct Preview {
    pub id: String,
    pub sender: String,
    pub at: String,
    pub thread: String,
    pub status: String,
    pub requires_response: bool,
    pub summary: String,
    pub raw_bytes: i64,
}

pub fn validate_identity(name: &str) -> Result<()> {
    ensure!(!name.is_empty() && name.len() <= 64 && name.as_bytes()[0].is_ascii_alphanumeric()
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b)),
        "invalid agent name {name:?}; expected 1..64 ASCII letters, digits, dots, underscores or hyphens, starting with a letter or digit");
    Ok(())
}

pub fn validate_message_id(id: &str) -> Result<()> {
    ensure!(
        !id.is_empty()
            && id.len() <= MAX_METADATA_BYTES as usize
            && !id.chars().any(char::is_control),
        "message id must contain 1..4096 bytes without control characters"
    );
    Ok(())
}

fn database_path(root: &Path, name: &str) -> Result<PathBuf> {
    validate_identity(name)?;
    let path = root.join(name).join("store.sqlite");
    // Canonical checks stop a mailbox-name symlink escaping the selected state root.
    if path.try_exists()? {
        let canonical_root = root.canonicalize()?;
        ensure!(
            path.canonicalize()?.starts_with(&canonical_root),
            "mailbox path escapes state directory"
        );
    }
    Ok(path)
}

pub fn registered_identity(root: &Path, name: &str) -> Result<()> {
    validate_identity(name)?;
    let path = root.join("agents.json");
    let file = std::fs::File::open(&path)
        .with_context(|| format!("reading registry {}", path.display()))?;
    let mut bytes = Vec::new();
    file.take(1_048_577).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= 1_048_576, "A2A registry exceeds 1 MiB");
    let registry: Value = serde_json::from_slice(&bytes).context("invalid A2A registry JSON")?;
    ensure!(
        registry["schema_version"].as_u64() == Some(1),
        "unsupported A2A registry schema"
    );
    let agents = registry["agents"]
        .as_object()
        .context("registry agents must be an object")?;
    for (identity, record) in agents {
        validate_identity(identity)?;
        ensure!(
            record.is_object(),
            "agent {identity:?} entry must be an object"
        );
        ensure!(
            record["token"]
                .as_str()
                .is_some_and(|s| s.chars().count() >= 16),
            "agent {identity:?} has no usable token"
        );
        ensure!(
            record["port"]
                .as_u64()
                .is_some_and(|p| (1..=65535).contains(&p)),
            "agent {identity:?} has no usable port"
        );
        if !record["generation"].is_null() {
            ensure!(
                record["generation"].as_str().is_some_and(|s| !s.is_empty()),
                "agent {identity:?} has no usable generation"
            );
        }
    }
    ensure!(agents.contains_key(name), "unknown identity {name:?}");
    Ok(())
}

impl Store {
    pub fn open(root: &Path, name: &str, writable: bool) -> Result<Option<Self>> {
        let path = database_path(root, name)?;
        if !path.try_exists()? {
            return Ok(None);
        }
        let flags = if writable {
            OpenFlags::SQLITE_OPEN_READ_WRITE
        } else {
            OpenFlags::SQLITE_OPEN_READ_ONLY
        };
        let connection = Connection::open_with_flags(&path, flags)
            .with_context(|| format!("opening mailbox {}", path.display()))?;
        connection.busy_timeout(Duration::from_secs(1))?;
        // Force SQLite to read the schema even when the requested table is absent.
        connection.query_row("SELECT count(*) FROM sqlite_master", [], |r| {
            r.get::<_, i64>(0)
        })?;
        Ok(Some(Self { connection }))
    }

    pub fn required(root: &Path, name: &str, writable: bool) -> Result<Self> {
        let store = Self::open(root, name, writable)?.with_context(|| {
            format!("mailbox for {name:?} is not initialized; no store was created")
        })?;
        store.validate_messages()?;
        Ok(store)
    }

    fn validate_messages(&self) -> Result<()> {
        ensure!(
            table_exists(&self.connection, "messages")?,
            "mailbox messages table is missing"
        );
        self.connection
            .prepare(&format!("SELECT {MESSAGE_COLUMNS} FROM messages LIMIT 0"))
            .context("invalid messages schema")?;
        Ok(())
    }

    /// Enqueue only into a transport-created, fully migrated store. No table is
    /// created or altered by this path; the composite keys preserve dedup.
    pub fn require_enqueue_schema(&self) -> Result<()> {
        self.validate_messages()?;
        for table in [
            "delivery_events",
            "message_state",
            "retention_events",
            "message_presentations",
        ] {
            ensure!(
                table_exists(&self.connection, table)?,
                "sender mailbox is not fully migrated: missing {table} table"
            );
        }
        self.connection
            .prepare(
                "SELECT event_id,message_id,occurred_at,status,reason FROM delivery_events LIMIT 0",
            )
            .context("invalid delivery_events schema")?;
        self.connection.prepare("SELECT direction,message_id,thread_id,summary,protocol_status,requires_response,retention_class,body_sha256,body_bytes,data_sha256,data_bytes FROM message_state LIMIT 0")
            .context("invalid message_state schema")?;
        let composite = vec!["direction".to_string(), "message_id".to_string()];
        ensure!(
            primary_key(&self.connection, "messages")? == composite,
            "sender messages table lacks the transport composite key"
        );
        ensure!(
            primary_key(&self.connection, "message_state")? == composite,
            "sender message_state table lacks the transport composite key"
        );
        ensure!(
            primary_key(&self.connection, "delivery_events")? == ["event_id".to_string()],
            "sender delivery_events table lacks its transport key"
        );
        Ok(())
    }

    pub fn enqueue(&mut self, message: &PreparedMessage, reason: &str) -> Result<Value> {
        self.connection.busy_timeout(Duration::from_secs(5))?;
        self.connection.pragma_update(None, "foreign_keys", "ON")?;
        let metadata = &message.metadata;
        let thread = metadata["thread_id"]
            .as_str()
            .context("missing compacted thread_id")?;
        let summary = metadata["summary"]
            .as_str()
            .context("missing compacted summary")?;
        let status = metadata["status"].as_str().unwrap_or("open");
        let response = metadata["actionable"]
            .as_bool()
            .context("missing compacted actionable state")?;
        let response_int = i64::from(response);
        let retention = if metadata["protocol"] == "coordination-v2" {
            "operational"
        } else {
            "pinned"
        };
        let body_hash = metadata["body_sha256"]
            .as_str()
            .context("missing compacted body digest")?;
        let body_bytes = metadata["raw_body_bytes"]
            .as_i64()
            .context("missing compacted body size")?;
        let data_hash = metadata["data_sha256"].as_str();
        let data_bytes = metadata["raw_data_bytes"]
            .as_i64()
            .context("missing compacted data size")?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "INSERT INTO messages(message_id,direction,sender,recipient,body,data_json,created_at,received_at,delivery_status,status_reason,read_at) VALUES (?1,'outbound',?2,?3,?4,?5,?6,NULL,'queued',?7,NULL)",
            params![message.id,message.sender,message.recipient,message.body,message.data_json,message.created_at,reason],
        )?;
        transaction.execute(
            "INSERT INTO message_state(direction,message_id,thread_id,summary,protocol_status,requires_response,retention_class,body_sha256,body_bytes,data_sha256,data_bytes) VALUES ('outbound',?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            params![message.id,thread,summary,status,response_int,retention,body_hash,body_bytes,data_hash,data_bytes],
        )?;
        transaction.execute("INSERT INTO delivery_events(message_id,occurred_at,status,reason) VALUES (?1,?2,'queued',?3)",
            params![message.id,message.created_at,reason])?;
        transaction.commit()?;
        Ok(
            json!({"schema_version":1,"authority":"a2a-delivery-receipt",
            "message_id":message.id,"sender":message.sender,"recipient":message.recipient,
            "created_at":message.created_at,"received_at":null,"delivery_status":"queued",
            "status_reason":reason,"thread_id":thread,"summary":summary,
            "protocol_status":status,"requires_response":response_int,
            "body_sha256":body_hash,"body_bytes":body_bytes,
            "data_sha256":data_hash,"data_bytes":data_bytes}),
        )
    }

    pub fn previews(
        &self,
        unread: bool,
        limit: usize,
        chars: usize,
    ) -> Result<(Vec<Preview>, i64, i64)> {
        let filter = if unread {
            "m.direction='inbound' AND m.read_at IS NULL"
        } else {
            "m.direction='inbound'"
        };
        let totals = format!("SELECT count(*),COALESCE(sum(length(CAST(m.body AS BLOB))+COALESCE(length(CAST(m.data_json AS BLOB)),0)),0),COALESCE(sum(typeof(m.body)!='text' OR (m.data_json IS NOT NULL AND typeof(m.data_json)!='text')),0) FROM messages m WHERE {filter}");
        // A read transaction makes counts and previews describe the same snapshot.
        let transaction = self.connection.unchecked_transaction()?;
        let (count, bytes, invalid): (i64, i64, i64) =
            transaction.query_row(&totals, [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        ensure!(
            invalid == 0,
            "mailbox contains {invalid} messages with invalid body/data types"
        );
        let lifecycle = table_exists(&transaction, "message_state")?;
        // SQLite TEXT substr stops at embedded NUL. A byte prefix preserves it
        // and bounds transferred content; Rust performs UTF-8/character slicing.
        let (summary, summary_bytes) = if lifecycle {
            (
                "CASE WHEN s.summary IS NULL OR length(CAST(s.summary AS BLOB))=0 THEN substr(CAST(m.body AS BLOB),1,?1) WHEN typeof(s.summary)='text' THEN substr(CAST(s.summary AS BLOB),1,?1) ELSE NULL END",
                "CASE WHEN s.summary IS NULL OR length(CAST(s.summary AS BLOB))=0 THEN length(CAST(m.body AS BLOB)) ELSE length(CAST(s.summary AS BLOB)) END",
            )
        } else {
            (
                "substr(CAST(m.body AS BLOB),1,?1)",
                "length(CAST(m.body AS BLOB))",
            )
        };
        let (thread, status, response, data_bytes) = if lifecycle {
            (
                "COALESCE(s.thread_id,'legacy:'||m.sender)",
                "COALESCE(s.protocol_status,'open')",
                "COALESCE(s.requires_response,1)",
                "COALESCE(s.data_bytes,length(CAST(m.data_json AS BLOB)),0)",
            )
        } else {
            (
                "'legacy:'||m.sender",
                "'open'",
                "1",
                "COALESCE(length(CAST(m.data_json AS BLOB)),0)",
            )
        };
        let join = if lifecycle {
            "LEFT JOIN message_state s ON s.direction=m.direction AND s.message_id=m.message_id"
        } else {
            ""
        };
        let fields = [
            "m.message_id",
            "m.sender",
            "COALESCE(NULLIF(m.received_at,''),m.created_at)",
            thread,
            status,
        ]
        .map(bounded_text)
        .join(",");
        let query = format!("SELECT {fields},{response},{data_bytes},{summary},length(CAST(m.body AS BLOB)),{summary_bytes} FROM messages m {join} WHERE {filter} ORDER BY m.created_at DESC,m.message_id DESC LIMIT ?2");
        let mut statement = transaction
            .prepare(&query)
            .context("invalid mailbox preview schema")?;
        let rows = statement.query_map(params![(chars * 4) as i64, limit as i64], |row| {
            preview_row(row, chars)
        })?;
        let previews = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        for row in &previews {
            ensure!(
                row.raw_bytes >= 0,
                "negative message size in lifecycle metadata"
            );
            for value in [&row.id, &row.sender, &row.at, &row.thread, &row.status] {
                ensure!(
                    value.len() <= MAX_METADATA_BYTES as usize,
                    "mailbox metadata exceeds 4096 bytes"
                );
            }
        }
        Ok((previews, count, bytes))
    }

    pub fn message(&mut self, id: &str, direction: &str, max_bytes: usize) -> Result<Value> {
        let transaction = self.connection.transaction()?;
        let size_sql = MESSAGE_COLUMNS
            .split(',')
            .map(|name| format!("COALESCE(length(CAST({name} AS BLOB)),0)"))
            .collect::<Vec<_>>()
            .join("+");
        let size: Option<i64> = transaction
            .query_row(
                &format!("SELECT {size_sql} FROM messages WHERE message_id=?1 AND direction=?2"),
                params![id, direction],
                |r| r.get(0),
            )
            .optional()?;
        let size = size.with_context(|| format!("unknown {direction} message {id:?}"))?;
        ensure!(size >= 0 && size <= max_bytes as i64, "stored message is {size} bytes, exceeding --max-bytes {max_bytes}; content was not loaded or printed");
        let row = transaction.query_row(
            &format!("SELECT {MESSAGE_COLUMNS} FROM messages WHERE message_id=?1 AND direction=?2"),
            params![id, direction],
            message_row,
        )?;
        if let Some(data) = row["data_json"].as_str() {
            let parsed: Value = serde_json::from_str(data).context("invalid stored data_json")?;
            ensure!(parsed.is_object(), "stored data_json must be a JSON object");
        }
        Ok(row)
    }

    pub fn mark_read(&mut self, id: &str) -> Result<Value> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let row: Option<(String, Option<String>)> = transaction
            .query_row(
                &format!(
                    "SELECT {},{} FROM messages WHERE message_id=?1 AND direction='inbound'",
                    bounded_text("sender"),
                    bounded_text("read_at")
                ),
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let (sender, read_at) = row.with_context(|| format!("unknown inbound message {id:?}"))?;
        ensure!(
            sender.len() <= MAX_METADATA_BYTES as usize,
            "mailbox sender exceeds 4096 bytes"
        );
        ensure!(
            read_at.is_none(),
            "no unread inbound message {id:?}; already read"
        );
        let now = crate::instant::isoformat_millis_utc(crate::instant::now());
        let changed = transaction.execute("UPDATE messages SET read_at=?1 WHERE message_id=?2 AND direction='inbound' AND read_at IS NULL", params![now,id])?;
        ensure!(
            changed == 1,
            "expected exactly one unread inbound message {id:?}, updated {changed}"
        );
        transaction.commit()?;
        Ok(json!({"message_id":id,"sender":sender,"read_at":now,"state":"read"}))
    }

    pub fn history(&self, id: Option<&str>, limit: usize) -> Result<Value> {
        // A missing event table is a recognized legacy mailbox only when the
        // base mailbox schema is valid. An unrelated/empty database is corrupt.
        self.validate_messages()?;
        if !table_exists(&self.connection, "delivery_events")? {
            return Ok(json!({"schema_version":1,"available":false,"events":[]}));
        }
        let filter = if id.is_some() {
            "WHERE message_id=?1"
        } else {
            "WHERE ?1 IS NULL"
        };
        let fields = ["message_id", "occurred_at", "status", "reason"]
            .map(bounded_text)
            .join(",");
        let mut statement = self.connection.prepare(&format!("SELECT event_id,{fields} FROM delivery_events {filter} ORDER BY event_id DESC LIMIT ?2"))?;
        let mut rows = statement.query(params![id, limit as i64])?;
        let mut events = Vec::new();
        while let Some(row) = rows.next()? {
            let message_id = row.get::<_, String>(1)?;
            let occurred_at = row.get::<_, String>(2)?;
            let status = row.get::<_, String>(3)?;
            let reason = row.get::<_, Option<String>>(4)?;
            for text in [&message_id, &occurred_at, &status]
                .into_iter()
                .chain(reason.as_ref())
            {
                ensure!(
                    text.len() <= MAX_METADATA_BYTES as usize,
                    "delivery event metadata exceeds 4096 bytes"
                );
            }
            events.push(json!({"event_id":row.get::<_,i64>(0)?,"message_id":message_id,"occurred_at":occurred_at,"status":status,"reason":reason}));
        }
        Ok(json!({"schema_version":1,"available":true,"events":events}))
    }
}

// Invalid or oversized fields produce a type error before Rust allocates their
// contents. NULL remains NULL so required/nullable column semantics stay intact.
fn bounded_text(expression: &str) -> String {
    format!("CASE WHEN ({expression}) IS NULL OR (typeof({expression})='text' AND length(CAST(({expression}) AS BLOB))<={MAX_METADATA_BYTES}) THEN ({expression}) ELSE zeroblob(0) END")
}

fn table_exists(connection: &Connection, name: &str) -> Result<bool> {
    let kind: Option<String> = connection
        .query_row(
            "SELECT type FROM sqlite_master WHERE name=?1",
            [name],
            |r| r.get(0),
        )
        .optional()?;
    match kind.as_deref() {
        Some("table") => Ok(true),
        None => Ok(false),
        _ => bail!("expected SQLite table {name:?}"),
    }
}

fn primary_key(connection: &Connection, table: &str) -> Result<Vec<String>> {
    let mut statement =
        connection.prepare("SELECT name FROM pragma_table_info(?1) WHERE pk > 0 ORDER BY pk")?;
    let names = statement.query_map([table], |row| row.get::<_, String>(0))?;
    Ok(names.collect::<std::result::Result<Vec<_>, _>>()?)
}

fn preview_row(row: &rusqlite::Row<'_>, chars: usize) -> rusqlite::Result<Preview> {
    let response: i64 = row.get(5)?;
    if response != 0 && response != 1 {
        return Err(rusqlite::Error::IntegralValueOutOfRange(5, response));
    }
    let data_bytes: i64 = row.get(6)?;
    let body_bytes: i64 = row.get(8)?;
    let raw_bytes = body_bytes
        .checked_add(data_bytes)
        .filter(|_| data_bytes >= 0 && body_bytes >= 0)
        .ok_or_else(|| rusqlite::Error::IntegralValueOutOfRange(6, data_bytes))?;
    Ok(Preview {
        id: row.get(0)?,
        sender: row.get(1)?,
        at: row.get(2)?,
        thread: row.get(3)?,
        status: row.get(4)?,
        requires_response: response == 1,
        summary: preview_summary(row, chars)?,
        raw_bytes,
    })
}

fn preview_summary(row: &rusqlite::Row<'_>, chars: usize) -> rusqlite::Result<String> {
    let prefix: Vec<u8> = row.get(7)?;
    let source_bytes: i64 = row.get(9)?;
    let text = match std::str::from_utf8(&prefix) {
        Ok(text) => text,
        Err(error) if error.error_len().is_none() && (prefix.len() as i64) < source_bytes => {
            // Only our byte limit may split the last codepoint. A malformed
            // sequence or incomplete codepoint at the actual field end fails.
            std::str::from_utf8(&prefix[..error.valid_up_to()]).map_err(preview_utf8_error)?
        }
        Err(error) => return Err(preview_utf8_error(error)),
    };
    Ok(text.chars().take(chars).collect())
}

fn preview_utf8_error(error: std::str::Utf8Error) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(7, rusqlite::types::Type::Blob, Box::new(error))
}

fn message_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    let mut value = serde_json::Map::new();
    for (index, name) in MESSAGE_COLUMNS.split(',').enumerate() {
        let nullable = matches!(
            name,
            "data_json" | "received_at" | "status_reason" | "read_at"
        );
        let cell = if nullable {
            row.get::<_, Option<String>>(index)?
                .map(Value::String)
                .unwrap_or(Value::Null)
        } else {
            Value::String(row.get(index)?)
        };
        value.insert(name.into(), cell);
    }
    Ok(Value::Object(value))
}
