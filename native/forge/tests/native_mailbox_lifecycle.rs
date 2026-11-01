//! Native lifecycle receipts never inject message bodies into command output.
#[allow(dead_code)]
mod support;
use conductor_native::a2a_store::Store;
use rusqlite::Connection;
use serde_json::Value;
use std::process::Output;

struct Fixture(support::Host);
impl Fixture {
    fn new() -> Self {
        let fixture = Self(support::Host::new());
        Store::initialize(&fixture.root(), "recipient")
            .unwrap()
            .record_inbound(
                "one",
                "sender",
                "recipient",
                &"private body ".repeat(1000),
                None,
                "2026-10-31T00:00:00.000+00:00",
            )
            .unwrap();
        fixture
    }
    fn root(&self) -> std::path::PathBuf {
        self.0 .0.join(".agents/a2a")
    }
    fn database(&self) -> Connection {
        Connection::open(self.root().join("recipient/store.sqlite")).unwrap()
    }
    fn cli(&self, args: &[&str]) -> Output {
        self.0
            .command()
            .env("PATH", "")
            .env("CONDUCTOR_PYTHON", "/bin/false")
            .arg("mailbox")
            .args(args)
            .output()
            .unwrap()
    }
    fn resolved(&self) -> Value {
        parsed(self.cli(&["resolve", "--as-name", "recipient", "one"]))
    }
    fn read(&self) {
        parsed(self.cli(&["read", "--as-name", "recipient", "one"]));
    }
}
fn parsed(output: Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.len() < 1024);
    assert!(!String::from_utf8_lossy(&output.stdout).contains("private body"));
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn resolution_requires_acknowledgment_and_returns_the_original_durable_timestamp() {
    let fixture = Fixture::new();
    let refused = fixture.cli(&["resolve", "--as-name", "recipient", "one"]);
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("cannot resolve unread"));
    assert!(refused.stdout.is_empty());
    fixture.read();
    let first = fixture.resolved();
    assert_eq!(first["state"], "resolved");
    assert_eq!(first["message_id"], "one");
    assert_eq!(first["sender"], "sender");
    assert_eq!(first, fixture.resolved());
    let row: (String, String, String) = fixture
        .database()
        .query_row(
            "SELECT m.body,s.protocol_status,s.resolved_at FROM messages m JOIN message_state s
         USING(direction,message_id) WHERE m.message_id='one'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(row.0, "private body ".repeat(1000));
    assert_eq!(row.1, "resolved");
    assert_eq!(row.2, first["resolved_at"]);
}

#[test]
fn holds_normalize_and_validate_before_changing_retention_state() {
    let fixture = Fixture::new();
    let result = parsed(fixture.cli(&[
        "hold",
        "--as-name",
        "recipient",
        "one",
        "--reason",
        "  independent\n review  ",
    ]));
    assert_eq!(result["hold"], "independent review");
    for reason in [" ".to_string(), "🦀".repeat(241)] {
        assert!(!fixture
            .cli(&["hold", "--as-name", "recipient", "one", "--reason", &reason])
            .status
            .success());
        let hold: String = fixture
            .database()
            .query_row(
                "SELECT hold_reason FROM message_state WHERE message_id='one'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(hold, "independent review");
    }
    assert!(
        parsed(fixture.cli(&["hold", "--as-name", "recipient", "one", "--clear"]))["hold"]
            .is_null()
    );
    let hold: Option<String> = fixture
        .database()
        .query_row(
            "SELECT hold_reason FROM message_state WHERE message_id='one'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(hold.is_none());
}

#[test]
fn self_send_lifecycle_changes_only_the_inbound_fact() {
    let fixture = Fixture::new();
    let db = fixture.database();
    db.execute_batch(
        "INSERT INTO messages SELECT message_id,'outbound',sender,recipient,body,
        data_json,created_at,received_at,delivery_status,status_reason,read_at FROM messages;
        INSERT INTO message_state SELECT 'outbound',message_id,thread_id,summary,protocol_status,
        requires_response,retention_class,resolved_at,superseded_at,hold_reason,tombstoned_at,
        body_sha256,body_bytes,data_sha256,data_bytes FROM message_state;",
    )
    .unwrap();
    fixture.read();
    fixture.resolved();
    parsed(fixture.cli(&[
        "hold",
        "--as-name",
        "recipient",
        "one",
        "--reason",
        "inspect",
    ]));
    let row: (String, Option<String>, Option<String>) = db.query_row(
        "SELECT protocol_status,resolved_at,hold_reason FROM message_state WHERE direction='outbound'", [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    ).unwrap();
    assert_eq!(row, ("open".into(), None, None));
}

#[test]
fn legacy_resolution_builds_pinned_provenance_and_rejects_oversized_materialization() {
    let fixture = Fixture::new();
    fixture
        .database()
        .execute("DELETE FROM message_state", [])
        .unwrap();
    assert!(!fixture
        .cli(&["hold", "--as-name", "recipient", "one", "--clear"])
        .status
        .success());
    fixture.read();
    fixture.resolved();
    let row: (String, i64, i64, String) = fixture
        .database()
        .query_row(
            "SELECT retention_class,requires_response,body_bytes,body_sha256 FROM message_state",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!((row.0.as_str(), row.1, row.2), ("pinned", 1, 13000));
    assert_eq!(row.3.len(), 64);
    let db = fixture.database();
    db.execute("DELETE FROM message_state", []).unwrap();
    db.execute("UPDATE messages SET body=?1", ["x".repeat(1_048_577)])
        .unwrap();
    let output = fixture.cli(&["resolve", "--as-name", "recipient", "one"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("maximum is 1048576"));
    assert_eq!(
        db.query_row("SELECT count(*) FROM message_state", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        db.query_row("SELECT length(body) FROM messages", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1_048_577
    );
}

#[test]
fn corrupt_metadata_unknown_messages_and_invalid_flags_do_not_change_state() {
    let fixture = Fixture::new();
    fixture.read();
    for args in [
        vec!["hold", "--as-name", "recipient", "one"],
        vec![
            "hold",
            "--as-name",
            "recipient",
            "one",
            "--clear",
            "--reason",
            "no",
        ],
        vec!["resolve", "--as-name", "recipient", "missing"],
        vec!["hold", "--as-name", "recipient", "missing", "--clear"],
        vec!["resolve", "--as-name", "recipient", "bad\nidentifier"],
    ] {
        assert!(!fixture.cli(&args).status.success());
    }
    fixture
        .database()
        .execute("UPDATE messages SET sender=?1", ["x".repeat(10_000)])
        .unwrap();
    assert!(!fixture
        .cli(&["resolve", "--as-name", "recipient", "one"])
        .status
        .success());
    let timestamp: Option<String> = fixture
        .database()
        .query_row("SELECT resolved_at FROM message_state", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert!(timestamp.is_none());
    let missing = support::Host::new();
    assert!(!missing
        .cli(&["mailbox", "resolve", "--as-name", "absent", "one"])
        .status
        .success());
    assert!(!missing.0.join(".agents").exists());
}

#[test]
fn concurrent_resolution_is_idempotent_and_does_not_read_large_retained_bodies() {
    let fixture = Fixture::new();
    fixture.read();
    // Simulate a large retained row with already-present lifecycle metadata.
    fixture
        .database()
        .execute("UPDATE messages SET body=?1", ["q".repeat(2_097_152)])
        .unwrap();
    std::thread::scope(|scope| {
        let workers: Vec<_> = (0..8).map(|_| scope.spawn(|| fixture.resolved())).collect();
        let results: Vec<_> = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect();
        assert!(results.iter().all(|result| result == &results[0]));
    });
    assert_eq!(
        fixture
            .database()
            .query_row("SELECT length(body) FROM messages", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        2_097_152
    );
}
