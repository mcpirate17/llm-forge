//! Shared durable A2A SQLite store for the Python transport and Forge mailbox.

#[path = "a2a_store_compat.rs"]
mod compat;
#[path = "a2a_store_preview.rs"]
mod preview;

use anyhow::{bail, ensure, Context, Result};
use rusqlite::{
    params, Connection, OpenFlags, OptionalExtension, Transaction, TransactionBehavior,
};
use serde_json::{json, Value};
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub struct PreparedMessage {
    pub id: String,
    pub sender: String,
    pub recipient: String,
    pub body: String,
    pub data_json: Option<String>,
    pub created_at: String,
    pub metadata: Value,
}

pub struct MessageInput<'a> {
    pub id: &'a str,
    pub sender: &'a str,
    pub recipient: &'a str,
    pub body: &'a str,
    pub data_json: Option<&'a str>,
    pub now: &'a str,
}

pub struct PendingOutbound {
    pub id: String,
    pub sender: String,
    pub recipient: String,
    pub body: String,
    pub data_json: Option<String>,
}

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

struct StateFields<'a> {
    thread: &'a str,
    summary: &'a str,
    status: &'a str,
    response: i64,
    retention: &'a str,
    body_hash: &'a str,
    body_bytes: i64,
    data_hash: Option<&'a str>,
    data_bytes: i64,
}

fn state_fields(metadata: &Value) -> Result<StateFields<'_>> {
    let field = |native: &str, python: &str| metadata.get(native).or_else(|| metadata.get(python));
    let thread = metadata["thread_id"]
        .as_str()
        .context("missing thread_id")?;
    let summary = metadata["summary"].as_str().context("missing summary")?;
    let status = field("status", "protocol_status")
        .and_then(Value::as_str)
        .unwrap_or("open");
    let response = field("actionable", "requires_response")
        .and_then(|value| value.as_bool().map(i64::from).or_else(|| value.as_i64()))
        .context("missing actionable state")?;
    ensure!((0..=1).contains(&response), "invalid actionable state");
    let retention = field("retention_class", "retention_class")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            if metadata["protocol"] == "coordination-v2" {
                "operational"
            } else {
                "pinned"
            }
        });
    let body_hash = metadata["body_sha256"]
        .as_str()
        .context("missing body digest")?;
    let body_bytes = field("raw_body_bytes", "body_bytes")
        .and_then(Value::as_i64)
        .context("missing body size")?;
    let data_hash = metadata["data_sha256"].as_str();
    let data_bytes = field("raw_data_bytes", "data_bytes")
        .and_then(Value::as_i64)
        .context("missing data size")?;
    Ok(StateFields {
        thread,
        summary,
        status,
        response,
        retention,
        body_hash,
        body_bytes,
        data_hash,
        data_bytes,
    })
}

fn insert_state(
    tx: &Transaction<'_>,
    direction: &str,
    id: &str,
    state: &StateFields<'_>,
) -> Result<()> {
    tx.execute("INSERT INTO message_state(direction,message_id,thread_id,summary,protocol_status,requires_response,retention_class,body_sha256,body_bytes,data_sha256,data_bytes) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
        params![direction,id,state.thread,state.summary,state.status,state.response,state.retention,state.body_hash,state.body_bytes,state.data_hash,state.data_bytes])?;
    Ok(())
}

fn insert_inbound_state(
    tx: &Transaction<'_>,
    id: &str,
    sender: &str,
    now: &str,
    metadata: &Value,
) -> Result<()> {
    let state = state_fields(metadata)?;
    insert_state(tx, "inbound", id, &state)?;
    if let Some(supersedes) = metadata["supersedes"].as_array() {
        for target in supersedes {
            let target = target.as_str().context("invalid supersession target")?;
            let changed = tx.execute("UPDATE message_state SET superseded_at=COALESCE(superseded_at,?1),protocol_status='superseded' WHERE direction='inbound' AND message_id=?2 AND thread_id=?3 AND tombstoned_at IS NULL AND EXISTS (SELECT 1 FROM messages prior WHERE prior.direction='inbound' AND prior.message_id=message_state.message_id AND prior.sender=?4)",
                params![now,target,state.thread,sender])?;
            ensure!(
                changed == 1,
                "supersedes target {target:?} is missing or belongs to another sender/thread"
            );
        }
    }
    Ok(())
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
    /// Create the durable schema shared by Python transport and Forge. Read-only
    /// mailbox commands continue to use `open` without creating a store.
    pub fn initialize(root: &Path, name: &str) -> Result<Self> {
        validate_identity(name)?;
        std::fs::create_dir_all(root)?;
        let directory = root.join(name);
        std::fs::create_dir_all(&directory)?;
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))?;
        let path = database_path(root, name)?;
        let connection = Connection::open(&path)?;
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS messages (
                message_id TEXT NOT NULL,
                direction TEXT NOT NULL CHECK(direction IN ('inbound','outbound')),
                sender TEXT NOT NULL, recipient TEXT NOT NULL, body TEXT NOT NULL,
                data_json TEXT, created_at TEXT NOT NULL, received_at TEXT,
                delivery_status TEXT NOT NULL, status_reason TEXT, read_at TEXT,
                PRIMARY KEY (direction,message_id));
             CREATE INDEX IF NOT EXISTS messages_inbox ON messages(direction,read_at,created_at);
             CREATE TABLE IF NOT EXISTS delivery_events (
                event_id INTEGER PRIMARY KEY, message_id TEXT NOT NULL,
                occurred_at TEXT NOT NULL, status TEXT NOT NULL, reason TEXT);
             CREATE TABLE IF NOT EXISTS message_state (
                direction TEXT NOT NULL, message_id TEXT NOT NULL, thread_id TEXT NOT NULL,
                summary TEXT NOT NULL, protocol_status TEXT NOT NULL,
                requires_response INTEGER NOT NULL CHECK(requires_response IN (0,1)),
                retention_class TEXT NOT NULL CHECK(retention_class IN ('pinned','operational')),
                resolved_at TEXT, superseded_at TEXT, hold_reason TEXT, tombstoned_at TEXT,
                body_sha256 TEXT NOT NULL, body_bytes INTEGER NOT NULL,
                data_sha256 TEXT, data_bytes INTEGER NOT NULL,
                PRIMARY KEY (direction,message_id),
                FOREIGN KEY (direction,message_id) REFERENCES messages(direction,message_id) ON DELETE CASCADE);
             CREATE INDEX IF NOT EXISTS message_state_context ON message_state(direction,thread_id);
             CREATE INDEX IF NOT EXISTS message_state_retention ON message_state(
                retention_class,hold_reason,tombstoned_at,resolved_at,superseded_at);
             CREATE TABLE IF NOT EXISTS retention_events (
                event_id TEXT PRIMARY KEY, direction TEXT NOT NULL, message_id TEXT NOT NULL,
                policy_version INTEGER NOT NULL, manifest_json TEXT NOT NULL,
                manifest_sha256 TEXT NOT NULL, compacted_at TEXT NOT NULL,
                UNIQUE(direction,message_id),
                FOREIGN KEY (direction,message_id) REFERENCES messages(direction,message_id) ON DELETE RESTRICT);
             CREATE TABLE IF NOT EXISTS message_presentations (
                direction TEXT NOT NULL, message_id TEXT NOT NULL, presented_at TEXT NOT NULL,
                PRIMARY KEY (direction,message_id),
                FOREIGN KEY (direction,message_id) REFERENCES messages(direction,message_id) ON DELETE CASCADE);",
        )?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        let store = Self { connection };
        store.require_enqueue_schema()?;
        Ok(store)
    }

    /// Deduplicate inbound delivery by (direction,message_id). Supersession and
    /// insertion commit together, so an invalid edge leaves no received fact.
    pub fn record_inbound(
        &mut self,
        id: &str,
        sender: &str,
        recipient: &str,
        body: &str,
        data_json: Option<&str>,
        now: &str,
    ) -> Result<()> {
        self.record_inbound_with_state(
            MessageInput {
                id,
                sender,
                recipient,
                body,
                data_json,
                now,
            },
            None,
        )
    }

    pub fn record_inbound_with_state(
        &mut self,
        input: MessageInput<'_>,
        selected_state: Option<&Value>,
    ) -> Result<()> {
        let MessageInput {
            id,
            sender,
            recipient,
            body,
            data_json,
            now,
        } = input;
        validate_message_id(id)?;
        ensure!(
            sender.len() <= 4096 && recipient.len() <= 4096,
            "sender or recipient exceeds 4096 bytes"
        );
        ensure!(body.len() <= 262_144, "body exceeds 262144 bytes");
        ensure!(
            data_json.is_none_or(|data| data.len() <= 1_048_576),
            "data exceeds 1048576 bytes"
        );
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let inserted = tx.execute(
            "INSERT OR IGNORE INTO messages(message_id,direction,sender,recipient,body,data_json,created_at,received_at,delivery_status) VALUES (?1,'inbound',?2,?3,?4,?5,?6,?6,'delivered')",
            params![id,sender,recipient,body,data_json,now],
        )?;
        if inserted == 0 {
            return Ok(());
        }
        let row = json!({"message_id":id,"direction":"inbound","sender":sender,
            "recipient":recipient,"body":body,"data_json":data_json,"created_at":now,
            "received_at":now,"delivery_status":"delivered","status_reason":null,"read_at":null});
        let computed;
        let metadata = if let Some(state) = selected_state {
            state
        } else {
            computed = crate::compact_a2a_message(&row).map_err(|error| {
                anyhow::anyhow!("message compaction metadata is invalid: {error}")
            })?;
            &computed
        };
        insert_inbound_state(&tx, id, sender, now, metadata)?;
        tx.commit()?;
        Ok(())
    }

    pub fn record_outbound(
        &mut self,
        input: MessageInput<'_>,
        selected_state: Option<&Value>,
    ) -> Result<()> {
        let MessageInput {
            id,
            sender,
            recipient,
            body,
            data_json,
            now,
        } = input;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "INSERT INTO messages(message_id,direction,sender,recipient,body,data_json,created_at,delivery_status) VALUES (?1,'outbound',?2,?3,?4,?5,?6,'pending')",
            params![id, sender, recipient, body, data_json, now],
        )?;
        let computed;
        let metadata = if let Some(state) = selected_state {
            state
        } else {
            let row = json!({"message_id":id,"direction":"outbound","sender":sender,
                "recipient":recipient,"body":body,"data_json":data_json,"created_at":now,
                "received_at":null,"delivery_status":"pending","status_reason":null,"read_at":null});
            computed = crate::compact_a2a_message(&row).map_err(|error| {
                anyhow::anyhow!("message compaction metadata is invalid: {error}")
            })?;
            &computed
        };
        insert_state(&transaction, "outbound", id, &state_fields(metadata)?)?;
        transaction.commit()?;
        Ok(())
    }

    /// Return one oldest retryable message. The sender delivery lock serializes
    /// compliant writers; both metadata and content are bounded before loading.
    pub fn next_outbound(
        &self,
        recipient: Option<&str>,
        excluded: &[String],
    ) -> Result<Option<PendingOutbound>> {
        self.validate_messages()?;
        let mut sql = String::from("SELECT message_id,sender,recipient,length(CAST(body AS BLOB)),COALESCE(length(CAST(data_json AS BLOB)),0) FROM messages WHERE direction='outbound' AND delivery_status IN ('queued','pending')");
        let mut parameters: Vec<&dyn rusqlite::ToSql> = Vec::new();
        let recipient_parameter = recipient.unwrap_or("");
        if recipient.is_some() {
            sql.push_str(" AND recipient=?");
            parameters.push(&recipient_parameter);
        }
        if !excluded.is_empty() {
            sql.push_str(" AND recipient NOT IN (");
            sql.push_str(&vec!["?"; excluded.len()].join(","));
            sql.push(')');
            parameters.extend(excluded.iter().map(|value| value as &dyn rusqlite::ToSql));
        }
        sql.push_str(" ORDER BY created_at,rowid LIMIT 1");
        let metadata: Option<(String, String, String, i64, i64)> = self
            .connection
            .query_row(&sql, parameters.as_slice(), |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            })
            .optional()?;
        let Some((id, sender, recipient, body_bytes, data_bytes)) = metadata else {
            return Ok(None);
        };
        for value in [&id, &sender, &recipient] {
            ensure!(value.len() <= 4096, "outbound metadata exceeds 4096 bytes");
        }
        ensure!(
            (0..=262_144).contains(&body_bytes),
            "outbound body exceeds 262144 bytes"
        );
        ensure!(
            (0..=1_048_576).contains(&data_bytes),
            "outbound data exceeds 1048576 bytes"
        );
        let (body, data_json): (String, Option<String>) = self.connection.query_row(
            "SELECT body,data_json FROM messages WHERE direction='outbound' AND message_id=?1",
            [&id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        ensure!(body.len() <= 262_144, "outbound body changed while loading");
        ensure!(
            data_json
                .as_ref()
                .is_none_or(|data| data.len() <= 1_048_576),
            "outbound data changed while loading"
        );
        Ok(Some(PendingOutbound {
            id,
            sender,
            recipient,
            body,
            data_json,
        }))
    }

    pub fn has_outbound(&self, recipient: Option<&str>) -> Result<bool> {
        let count: i64 = self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM messages WHERE direction='outbound' AND delivery_status IN ('queued','pending') AND (?1 IS NULL OR recipient=?1))",
            [recipient],
            |row| row.get(0),
        )?;
        Ok(count != 0)
    }

    pub fn mark_outbound(
        &mut self,
        id: &str,
        status: &str,
        reason: Option<&str>,
        now: &str,
        received_at: Option<&str>,
    ) -> Result<()> {
        ensure!(
            matches!(status, "delivered" | "failed" | "queued"),
            "invalid outbound status {status:?}"
        );
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = transaction.execute(
            "UPDATE messages SET delivery_status=?1,status_reason=?2,received_at=?3 WHERE message_id=?4 AND direction='outbound'",
            params![status, reason, received_at, id],
        )?;
        ensure!(changed == 1, "unknown outbound message {id:?}");
        transaction.execute(
            "INSERT INTO delivery_events(message_id,occurred_at,status,reason) VALUES (?1,?2,?3,?4)",
            params![id, now, status, reason],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn outbound_receipt(&self, id: &str) -> Result<Value> {
        let row: Value = self.connection.query_row(
            "SELECT m.message_id,m.sender,m.recipient,m.created_at,m.received_at,m.delivery_status,m.status_reason,s.thread_id,s.summary,s.protocol_status,s.requires_response,s.body_sha256,s.body_bytes,s.data_sha256,s.data_bytes FROM messages m JOIN message_state s ON s.direction=m.direction AND s.message_id=m.message_id WHERE m.direction='outbound' AND m.message_id=?1",
            [id],
            |row| Ok(json!({
                "schema_version":1,"authority":"a2a-delivery-receipt",
                "message_id":row.get::<_,String>(0)?,"sender":row.get::<_,String>(1)?,
                "recipient":row.get::<_,String>(2)?,"created_at":row.get::<_,String>(3)?,
                "received_at":row.get::<_,Option<String>>(4)?,
                "delivery_status":row.get::<_,String>(5)?,
                "status_reason":row.get::<_,Option<String>>(6)?,
                "thread_id":row.get::<_,String>(7)?,"summary":row.get::<_,String>(8)?,
                "protocol_status":row.get::<_,String>(9)?,
                "requires_response":row.get::<_,i64>(10)?,
                "body_sha256":row.get::<_,String>(11)?,"body_bytes":row.get::<_,i64>(12)?,
                "data_sha256":row.get::<_,Option<String>>(13)?,"data_bytes":row.get::<_,i64>(14)?,
            })),
        ).with_context(|| format!("unknown outbound message {id:?}"))?;
        Ok(row)
    }
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
        let state = state_fields(&message.metadata)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "INSERT INTO messages(message_id,direction,sender,recipient,body,data_json,created_at,received_at,delivery_status,status_reason,read_at) VALUES (?1,'outbound',?2,?3,?4,?5,?6,NULL,'queued',?7,NULL)",
            params![message.id,message.sender,message.recipient,message.body,message.data_json,message.created_at,reason],
        )?;
        insert_state(&transaction, "outbound", &message.id, &state)?;
        transaction.execute("INSERT INTO delivery_events(message_id,occurred_at,status,reason) VALUES (?1,?2,'queued',?3)",
            params![message.id,message.created_at,reason])?;
        transaction.commit()?;
        Ok(
            json!({"schema_version":1,"authority":"a2a-delivery-receipt",
            "message_id":message.id,"sender":message.sender,"recipient":message.recipient,
            "created_at":message.created_at,"received_at":null,"delivery_status":"queued",
            "status_reason":reason,"thread_id":state.thread,"summary":state.summary,
            "protocol_status":state.status,"requires_response":state.response,
            "body_sha256":state.body_hash,"body_bytes":state.body_bytes,
            "data_sha256":state.data_hash,"data_bytes":state.data_bytes}),
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

    pub fn mark_read(&mut self, id: &str, now: &str) -> Result<Value> {
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
