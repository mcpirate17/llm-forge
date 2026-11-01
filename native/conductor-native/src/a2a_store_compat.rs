//! Transport compatibility queries on the shared A2A store.

use super::{insert_state, message_row, state_fields, Store, MESSAGE_COLUMNS};
use anyhow::{ensure, Context, Result};
use rusqlite::{
    params, params_from_iter, types::Value as SqlValue, OptionalExtension, TransactionBehavior,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

impl Store {
    /// Python transport retry enumeration; Forge's bounded `next_outbound`
    /// remains the streaming path for native delivery.
    pub fn queued_rows(
        &self,
        recipient: Option<&str>,
        excluded: &[String],
        limit: Option<i64>,
    ) -> Result<Vec<Value>> {
        let mut query = format!("SELECT {MESSAGE_COLUMNS} FROM messages WHERE direction='outbound' AND delivery_status IN ('queued','pending')");
        let mut parameters: Vec<SqlValue> = Vec::new();
        if let Some(recipient) = recipient {
            query.push_str(" AND recipient=?");
            parameters.push(SqlValue::Text(recipient.to_owned()));
        }
        if !excluded.is_empty() {
            query.push_str(" AND recipient NOT IN (");
            query.push_str(&vec!["?"; excluded.len()].join(","));
            query.push(')');
            parameters.extend(excluded.iter().cloned().map(SqlValue::Text));
        }
        query.push_str(" ORDER BY created_at,rowid");
        if let Some(limit) = limit {
            query.push_str(" LIMIT ?");
            parameters.push(SqlValue::Integer(limit));
        }
        let mut statement = self.connection.prepare(&query)?;
        let rows = statement.query_map(params_from_iter(parameters.iter()), message_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn inbound_rows(&self, unread_only: bool, limit: i64) -> Result<Vec<Value>> {
        let condition = if unread_only {
            " AND read_at IS NULL"
        } else {
            ""
        };
        let query = format!("SELECT {MESSAGE_COLUMNS} FROM messages WHERE direction='inbound'{condition} ORDER BY created_at DESC LIMIT ?1");
        let mut statement = self.connection.prepare(&query)?;
        let rows = statement.query_map([limit], message_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn fetch_row(&self, id: &str, direction: &str) -> Result<Value> {
        self.connection
            .query_row(
                &format!(
                    "SELECT {MESSAGE_COLUMNS} FROM messages WHERE message_id=?1 AND direction=?2"
                ),
                params![id, direction],
                message_row,
            )
            .optional()?
            .with_context(|| format!("unknown message {id:?}"))
    }

    pub fn mark_presented(&mut self, ids: &[String], now: &str) -> Result<usize> {
        if ids.is_empty() {
            return Ok(0);
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut changed = 0;
        for id in ids {
            changed += transaction.execute(
                "INSERT OR IGNORE INTO message_presentations(direction,message_id,presented_at) VALUES ('inbound',?1,?2)",
                params![id, now],
            )?;
        }
        transaction.commit()?;
        Ok(changed)
    }

    pub fn resolve(&mut self, id: &str, now: &str) -> Result<Value> {
        self.resolve_inner(id, now, false)
    }

    /// Resolve without transferring bodies or data. Legacy rows without lifecycle
    /// metadata require at most 1 MiB to construct their pinned provenance record.
    pub fn resolve_receipt(&mut self, id: &str, now: &str) -> Result<Value> {
        self.resolve_inner(id, now, true)
    }

    fn resolve_inner(&mut self, id: &str, now: &str, receipt: bool) -> Result<Value> {
        super::validate_message_id(id)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let has_state: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM message_state WHERE direction='inbound' AND message_id=?1)",
            [id],
            |found| found.get(0),
        )?;
        let row = resolution_row(&transaction, id, receipt, has_state)?;
        ensure!(
            !row["read_at"].is_null(),
            "cannot resolve unread message {id:?}"
        );
        if !has_state {
            let state = legacy_state(id, &row)?;
            insert_state(&transaction, "inbound", id, &state_fields(&state)?)?;
        }
        transaction.execute(
            "UPDATE message_state SET resolved_at=COALESCE(resolved_at,?1),protocol_status='resolved' WHERE direction='inbound' AND message_id=?2",
            params![now, id],
        )?;
        let output = if receipt {
            let resolved_at: String = transaction.query_row(
                &format!(
                    "SELECT {} FROM message_state WHERE direction='inbound' AND message_id=?1",
                    super::bounded_text("resolved_at")
                ),
                [id],
                |row| row.get(0),
            )?;
            ensure!(
                resolved_at.len() <= 128,
                "invalid stored resolution timestamp"
            );
            json!({"schema_version":1,"message_id":id,"sender":row["sender"],
                "state":"resolved","resolved_at":resolved_at})
        } else {
            row
        };
        transaction.commit()?;
        Ok(output)
    }

    pub fn set_hold(&mut self, id: &str, reason: Option<&str>) -> Result<()> {
        let normalized = reason.map(crate::normalize_a2a_text);
        if let Some(reason) = normalized.as_deref() {
            ensure!(
                !reason.is_empty() && reason.chars().count() <= 240,
                "hold reason must be 1..240 characters"
            );
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let exists: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM messages WHERE message_id=?1 AND direction='inbound')",
            [id],
            |row| row.get(0),
        )?;
        ensure!(exists, "unknown message {id:?}");
        let changed = transaction.execute(
            "UPDATE message_state SET hold_reason=?1 WHERE direction='inbound' AND message_id=?2",
            params![normalized, id],
        )?;
        ensure!(
            changed == 1,
            "message {id:?} has no lifecycle metadata; legacy messages remain pinned"
        );
        transaction.commit()?;
        Ok(())
    }

    pub fn counts(&self) -> Result<Value> {
        let mut statement = self
            .connection
            .prepare("SELECT delivery_status, COUNT(*) FROM messages GROUP BY delivery_status")?;
        let mut counts = serde_json::Map::new();
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        for row in rows {
            let (status, count) = row?;
            counts.insert(status, json!(count));
        }
        Ok(Value::Object(counts))
    }
}

fn resolution_row(
    transaction: &rusqlite::Transaction<'_>,
    id: &str,
    receipt: bool,
    has_state: bool,
) -> Result<Value> {
    if receipt && has_state {
        let query = format!(
            "SELECT {},{} FROM messages WHERE direction='inbound' AND message_id=?1",
            super::bounded_text("sender"),
            "read_at IS NOT NULL"
        );
        let (sender, read): (String, bool) = transaction
            .query_row(&query, [id], |row| Ok((row.get(0)?, row.get(1)?)))
            .optional()?
            .with_context(|| format!("unknown message {id:?}"))?;
        ensure!(
            sender.len() <= super::MAX_METADATA_BYTES as usize,
            "sender exceeds 4096 bytes"
        );
        return Ok(json!({"sender":sender,"read_at":if read { json!(true) } else { Value::Null }}));
    }
    if receipt {
        let size = super::message_size(transaction, id, "inbound")?;
        ensure!((0..=1_048_576).contains(&size),
            "legacy resolution requires loading {size} bytes; maximum is 1048576; message unchanged");
    }
    transaction.query_row(
        &format!("SELECT {MESSAGE_COLUMNS} FROM messages WHERE message_id=?1 AND direction='inbound'"),
        [id], message_row,
    ).optional()?.with_context(|| format!("unknown message {id:?}"))
}

fn legacy_state(id: &str, row: &Value) -> Result<Value> {
    let sender = row["sender"].as_str().context("legacy sender missing")?;
    let body = row["body"].as_str().context("legacy body missing")?;
    let data = row["data_json"].as_str();
    let collapsed = crate::normalize_a2a_text(body);
    let summary = if collapsed.chars().count() > 240 {
        format!(
            "{}...",
            collapsed.chars().take(237).collect::<String>().trim_end()
        )
    } else {
        collapsed
    };
    let summary = if summary.is_empty() {
        format!("message {id}")
    } else {
        summary
    };
    Ok(json!({
        "thread_id":format!("legacy:{sender}"),
        "summary":summary,"protocol_status":"open","requires_response":1,
        "retention_class":"pinned","body_sha256":format!("{:x}", Sha256::digest(body.as_bytes())),
        "body_bytes":body.len(),"data_sha256":data.map(|value| format!("{:x}",Sha256::digest(value.as_bytes()))),
        "data_bytes":data.map_or(0,str::len)
    }))
}
