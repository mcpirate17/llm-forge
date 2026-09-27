//! Bounded Python inbox envelope query on the shared SQLite connection.

use super::Store;
use anyhow::Result;
use rusqlite::{params, Row};
use serde_json::{json, Value};

impl Store {
    pub fn preview_rows(
        &self,
        unread_only: bool,
        unpresented_only: bool,
        limit: i64,
        preview_chars: i64,
    ) -> Result<(Vec<Value>, i64)> {
        let mut where_clause = String::from("m.direction='inbound'");
        if unread_only {
            where_clause.push_str(" AND m.read_at IS NULL");
        }
        if unpresented_only {
            where_clause.push_str(" AND p.message_id IS NULL");
        }
        let query = format!(
            r"
            SELECT
                m.message_id, m.direction, m.sender, m.recipient,
                m.created_at, m.received_at, m.delivery_status, m.read_at,
                substr(replace(replace(m.body, char(10), ' '), char(13), ' '),
                       1, ?1) AS body,
                length(CAST(m.body AS BLOB)) AS body_bytes,
                COALESCE(s.thread_id, 'legacy:' || m.sender) AS thread_id,
                COALESCE(NULLIF(substr(s.summary, 1, ?2), ''),
                         substr(replace(replace(m.body, char(10), ' '), char(13), ' '),
                                1, ?3)) AS summary,
                COALESCE(s.protocol_status, 'open') AS protocol_status,
                COALESCE(s.requires_response, 1) AS requires_response,
                COALESCE(s.retention_class, 'pinned') AS retention_class,
                p.presented_at,
                s.resolved_at, s.superseded_at, s.hold_reason,
                COALESCE(s.body_sha256, '') AS body_sha256,
                COALESCE(s.data_bytes, length(CAST(m.data_json AS BLOB)), 0)
                    AS data_bytes,
                SUM(length(CAST(m.body AS BLOB))
                    + COALESCE(length(CAST(m.data_json AS BLOB)), 0)) OVER ()
                    AS total_raw_bytes,
                COUNT(*) OVER () AS total_count
            FROM messages AS m
            LEFT JOIN message_state AS s
              ON s.direction=m.direction AND s.message_id=m.message_id
            LEFT JOIN message_presentations AS p
              ON p.direction=m.direction AND p.message_id=m.message_id
            WHERE {where_clause}
            ORDER BY m.created_at DESC, m.message_id DESC
            LIMIT ?4
        "
        );
        let mut statement = self.connection.prepare(&query)?;
        let rows = statement.query_map(
            params![preview_chars, preview_chars, preview_chars, limit],
            preview_row,
        )?;
        let fetched = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        let total = fetched
            .first()
            .and_then(|value| value["total_count"].as_i64())
            .unwrap_or(0);
        Ok((fetched, total))
    }
}

fn preview_row(row: &Row<'_>) -> rusqlite::Result<Value> {
    Ok(json!({
        "message_id":row.get::<_,String>(0)?,
        "direction":row.get::<_,String>(1)?,
        "sender":row.get::<_,String>(2)?,
        "recipient":row.get::<_,String>(3)?,
        "created_at":row.get::<_,String>(4)?,
        "received_at":row.get::<_,Option<String>>(5)?,
        "delivery_status":row.get::<_,String>(6)?,
        "read_at":row.get::<_,Option<String>>(7)?,
        "body":row.get::<_,String>(8)?,
        "body_bytes":row.get::<_,i64>(9)?,
        "thread_id":row.get::<_,String>(10)?,
        "summary":row.get::<_,String>(11)?,
        "protocol_status":row.get::<_,String>(12)?,
        "requires_response":row.get::<_,i64>(13)?,
        "retention_class":row.get::<_,String>(14)?,
        "presented_at":row.get::<_,Option<String>>(15)?,
        "resolved_at":row.get::<_,Option<String>>(16)?,
        "superseded_at":row.get::<_,Option<String>>(17)?,
        "hold_reason":row.get::<_,Option<String>>(18)?,
        "body_sha256":row.get::<_,String>(19)?,
        "data_bytes":row.get::<_,i64>(20)?,
        "total_raw_bytes":row.get::<_,i64>(21)?,
        "total_count":row.get::<_,i64>(22)?,
    }))
}
