//! Durable retry timing without changing the legacy messages table.
use super::{table_exists, Store};
use anyhow::{ensure, Result};
use rusqlite::{params, OptionalExtension, TransactionBehavior};
use std::time::{SystemTime, UNIX_EPOCH};

pub(super) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

pub(super) fn same_data(left: Option<&str>, right: Option<&str>) -> bool {
    if left == right {
        return true;
    }
    match (left, right) {
        (Some(left), Some(right)) => {
            let left = serde_json::from_str::<serde_json::Value>(left);
            let right = serde_json::from_str::<serde_json::Value>(right);
            matches!((left,right),(Ok(left),Ok(right)) if left == right)
        }
        _ => false,
    }
}

impl Store {
    pub fn ensure_retry_schema(&self) -> Result<()> {
        self.connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS outbound_retries (
            message_id TEXT PRIMARY KEY, attempts INTEGER NOT NULL,
            next_attempt_ms INTEGER NOT NULL);
            CREATE INDEX IF NOT EXISTS messages_outbox ON messages(direction,recipient,created_at)
                WHERE direction='outbound' AND delivery_status IN ('queued','pending');",
        )?;
        Ok(())
    }

    /// Commit retry timing, queued status and history together.
    pub fn schedule_retry(
        &mut self,
        id: &str,
        reason: &str,
        now: &str,
        retry_after_ms: u64,
    ) -> Result<()> {
        self.ensure_retry_schema()?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let attempts: u32 = tx
            .query_row(
                "SELECT attempts FROM outbound_retries WHERE message_id=?1",
                [id],
                |row| row.get(0),
            )
            .optional()?
            .unwrap_or(0);
        let base = 250_u64.saturating_mul(1 << attempts.min(10)).min(60_000);
        // Stable per-message jitter survives restart and avoids synchronized retries.
        use sha2::{Digest, Sha256};
        let digest = Sha256::digest(format!("{id}:{attempts}"));
        let jitter = u16::from_be_bytes([digest[0], digest[1]]) as u64 % (base / 4 + 1);
        let delay = (base + jitter)
            .min(60_000)
            .max(retry_after_ms.min(3_600_000));
        let due = now_ms().saturating_add(delay as i64);
        let changed = tx.execute("UPDATE messages SET delivery_status='queued',status_reason=?1 WHERE direction='outbound' AND message_id=?2", params![reason,id])?;
        ensure!(changed == 1, "unknown outbound message {id:?}");
        tx.execute("INSERT INTO outbound_retries VALUES (?1,?2,?3) ON CONFLICT(message_id) DO UPDATE SET attempts=excluded.attempts,next_attempt_ms=excluded.next_attempt_ms", params![id,attempts.saturating_add(1),due])?;
        tx.execute("INSERT INTO delivery_events(message_id,occurred_at,status,reason) VALUES (?1,?2,'queued',?3)", params![id,now,reason])?;
        tx.commit()?;
        Ok(())
    }

    pub(super) fn retry_filter(&self) -> Result<String> {
        Ok(if table_exists(&self.connection, "outbound_retries")? {
            format!(" AND NOT EXISTS (SELECT 1 FROM outbound_retries r WHERE r.message_id=m.message_id AND r.next_attempt_ms>{}) AND NOT EXISTS (SELECT 1 FROM messages older WHERE older.direction='outbound' AND older.delivery_status IN ('queued','pending') AND older.recipient=m.recipient AND (older.created_at<m.created_at OR (older.created_at=m.created_at AND older.rowid<m.rowid)))", now_ms())
        } else {
            String::new()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::same_data;
    #[test]
    fn structured_duplicates_compare_payload_not_json_layout() {
        assert!(same_data(
            Some(r#"{"a":1,"b":true}"#),
            Some(r#"{ "b": true, "a": 1 }"#)
        ));
        assert!(!same_data(Some(r#"{"a":1}"#), Some(r#"{"a":2}"#)));
        assert!(!same_data(None, Some("{}")));
        assert!(!same_data(Some("invalid"), Some("also invalid")));
    }
}
