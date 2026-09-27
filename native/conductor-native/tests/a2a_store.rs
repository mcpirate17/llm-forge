//! Shared SQLite behavior exercised by both the Python transport and Forge.

use conductor_native::a2a_store::{MessageInput, Store};
use rusqlite::Connection;
use serde_json::json;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

struct StateRoot(PathBuf);

impl StateRoot {
    fn new(label: &str) -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "conductor-a2a-store-{label}-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn database(&self) -> PathBuf {
        self.0.join("tester-a/store.sqlite")
    }
}

impl Drop for StateRoot {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

fn state(thread: &str, supersedes: &[&str]) -> serde_json::Value {
    json!({
        "thread_id":thread,"summary":"status","status":"open","actionable":true,
        "protocol":"coordination-v2","body_sha256":"a".repeat(64),
        "raw_body_bytes":4,"data_sha256":null,"raw_data_bytes":0,
        "supersedes":supersedes
    })
}

#[test]
fn invalid_supersession_rolls_back_message_and_prior_state() {
    let root = StateRoot::new("supersession");
    let mut store = Store::initialize(&root.0, "tester-a").unwrap();
    store
        .record_inbound_with_state(
            MessageInput {
                id: "old",
                sender: "tester-b",
                recipient: "tester-a",
                body: "body",
                data_json: None,
                now: "2026-09-27T00:00:00.000+00:00",
            },
            Some(&state("thread-other", &[])),
        )
        .unwrap();
    let error = store
        .record_inbound_with_state(
            MessageInput {
                id: "invalid",
                sender: "tester-b",
                recipient: "tester-a",
                body: "body",
                data_json: None,
                now: "2026-09-27T00:00:01.000+00:00",
            },
            Some(&state("thread-new", &["old"])),
        )
        .unwrap_err();
    assert!(error.to_string().contains("another sender/thread"));
    let connection = Connection::open(root.database()).unwrap();
    let count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM messages WHERE message_id='invalid'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
    let status: String = connection
        .query_row(
            "SELECT protocol_status FROM message_state WHERE message_id='old'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(status, "open");
}

#[test]
fn lifecycle_and_preview_share_the_durable_store() {
    let root = StateRoot::new("lifecycle");
    let mut store = Store::initialize(&root.0, "tester-a").unwrap();
    let created = "2026-09-27T00:00:00.000+00:00";
    store
        .record_inbound("one", "tester-b", "tester-a", "hello", None, created)
        .unwrap();
    let (preview, total) = store.preview_rows(true, false, 8, 140).unwrap();
    assert_eq!(total, 1);
    assert_eq!(preview[0]["summary"], "hello");
    assert_eq!(
        store.mark_presented(&["one".to_owned()], created).unwrap(),
        1
    );
    assert_eq!(store.preview_rows(true, true, 8, 140).unwrap().1, 0);
    assert!(store
        .resolve("one", created)
        .unwrap_err()
        .to_string()
        .contains("cannot resolve unread"));
    store.mark_read("one", created).unwrap();
    store.resolve("one", created).unwrap();
    store
        .set_hold("one", Some("  independent\n review "))
        .unwrap();
    let connection = Connection::open(root.database()).unwrap();
    let (status, hold): (String, String) = connection
        .query_row(
            "SELECT protocol_status,hold_reason FROM message_state WHERE message_id='one'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        (status.as_str(), hold.as_str()),
        ("resolved", "independent review")
    );
}

#[test]
fn outbound_retry_order_and_events_remain_durable() {
    let root = StateRoot::new("outbound");
    let mut store = Store::initialize(&root.0, "tester-a").unwrap();
    for (id, at) in [
        ("first", "2026-09-27T00:00:00.000+00:00"),
        ("second", "2026-09-27T00:00:01.000+00:00"),
    ] {
        store
            .record_outbound(
                MessageInput {
                    id,
                    sender: "tester-a",
                    recipient: "tester-b",
                    body: "body",
                    data_json: None,
                    now: at,
                },
                None,
            )
            .unwrap();
    }
    store
        .mark_outbound(
            "first",
            "queued",
            Some("offline"),
            "2026-09-27T00:00:02.000+00:00",
            None,
        )
        .unwrap();
    let pending = store.queued_rows(Some("tester-b"), &[], None).unwrap();
    assert_eq!(
        pending
            .iter()
            .map(|row| row["message_id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["first", "second"]
    );
    store
        .mark_outbound(
            "first",
            "delivered",
            None,
            "2026-09-27T00:00:03.000+00:00",
            Some("2026-09-27T00:00:03.000+00:00"),
        )
        .unwrap();
    assert_eq!(store.queued_rows(None, &[], None).unwrap().len(), 1);
    let history = store.history(Some("first"), 10).unwrap();
    assert_eq!(history["events"].as_array().unwrap().len(), 2);
}
