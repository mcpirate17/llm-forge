//! Real native CLI checks against synthetic A2A SQLite stores. No Python/server.

use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

const MESSAGES: &str = "
CREATE TABLE messages (
 message_id TEXT NOT NULL,
 direction TEXT NOT NULL CHECK(direction IN ('inbound','outbound')),
 sender TEXT NOT NULL, recipient TEXT NOT NULL, body TEXT NOT NULL,
 data_json TEXT, created_at TEXT NOT NULL, received_at TEXT,
 delivery_status TEXT NOT NULL, status_reason TEXT, read_at TEXT,
 PRIMARY KEY(direction,message_id));
CREATE INDEX messages_inbox ON messages(direction,read_at,created_at);";
const METADATA: &str = "
CREATE TABLE message_state (
 direction TEXT NOT NULL, message_id TEXT NOT NULL, thread_id TEXT NOT NULL,
 summary TEXT NOT NULL, protocol_status TEXT NOT NULL,
 requires_response INTEGER NOT NULL CHECK(requires_response IN(0,1)),
 retention_class TEXT NOT NULL, resolved_at TEXT, superseded_at TEXT,
 hold_reason TEXT, tombstoned_at TEXT, body_sha256 TEXT NOT NULL,
 body_bytes INTEGER NOT NULL, data_sha256 TEXT, data_bytes INTEGER NOT NULL,
 PRIMARY KEY(direction,message_id));
CREATE TABLE message_presentations (
 direction TEXT NOT NULL,message_id TEXT NOT NULL,presented_at TEXT NOT NULL,
 PRIMARY KEY(direction,message_id));
CREATE TABLE delivery_events (
 event_id INTEGER PRIMARY KEY,message_id TEXT NOT NULL,occurred_at TEXT NOT NULL,
 status TEXT NOT NULL,reason TEXT);";

struct Fixture {
    host: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let host = std::env::temp_dir().join(format!(
            "forge-mailbox-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&host).unwrap();
        Self { host }
    }

    fn root(&self) -> PathBuf {
        self.host.join(".agents/a2a")
    }
    fn path(&self) -> PathBuf {
        self.root().join("receiver/store.sqlite")
    }

    fn registry(&self) {
        std::fs::create_dir_all(self.root()).unwrap();
        std::fs::write(self.root().join("agents.json"),json!({"schema_version":1,"agents":{"receiver":{"port":7399,"token":"synthetic-mailbox-test-token"}}}).to_string()).unwrap();
    }

    fn store(&self, metadata: bool) -> Connection {
        self.registry();
        std::fs::create_dir_all(self.path().parent().unwrap()).unwrap();
        let connection = Connection::open(self.path()).unwrap();
        connection.execute_batch(MESSAGES).unwrap();
        if metadata {
            connection.execute_batch(METADATA).unwrap();
        }
        connection
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_forge"));
        command
            .args(["mailbox", "--host"])
            .arg(&self.host)
            .args(args);
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command(args).output().unwrap()
    }

    fn ok(&self, args: &[&str]) -> Value {
        let output = self.run(args);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }

    fn error(&self, args: &[&str], fragment: &str) {
        let output = self.run(args);
        assert!(!output.status.success());
        assert!(
            output.stdout.is_empty(),
            "failed command exposed partial output"
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains(fragment), "expected {fragment:?}: {stderr}");
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.host).unwrap();
    }
}

fn insert(connection: &Connection, id: &str, direction: &str, body: &str, data: Option<&str>) {
    connection.execute("INSERT INTO messages(message_id,direction,sender,recipient,body,data_json,created_at,received_at,delivery_status) VALUES (?1,?2,'sender','receiver',?3,?4,'2026-09-26T12:00:00.000+00:00',NULL,'delivered')",params![id,direction,body,data]).unwrap();
}

fn lifecycle(connection: &Connection, id: &str, summary: &str) {
    connection.execute("INSERT INTO message_state(direction,message_id,thread_id,summary,protocol_status,requires_response,retention_class,body_sha256,body_bytes,data_bytes) VALUES ('inbound',?1,'thread-1',?2,'open',1,'operational','fixture-digest',100,0)",params![id,summary]).unwrap();
}

#[test]
fn native_inbox_matches_existing_envelope_and_show_preserves_raw_json() {
    let fixture = Fixture::new();
    let connection = fixture.store(true);
    let body = "private-body-".repeat(500);
    let data = r#"{"kind": "coordination-v2", "summary": "compact"}"#;
    insert(&connection, "message-1", "inbound", &body, Some(data));
    lifecycle(&connection, "message-1", "Safe bounded summary");
    connection
        .execute(
            "UPDATE message_state SET data_bytes=?1",
            [data.len() as i64],
        )
        .unwrap();
    let before = std::fs::read(fixture.path()).unwrap();
    let value = fixture.ok(&["inbox", "--as-name", "receiver", "--compact", "--json"]);
    assert_eq!(
        value,
        json!({"schema_version":1,"authority":"bounded-a2a-inbox","agent":"receiver","unread_only":false,"total":1,"shown":1,"omitted":0,"raw_bytes_not_injected":body.len()+data.len(),"messages":[{"id":"message-1","from":"sender","at":"2026-09-26T12:00:00.000+00:00","thread":"thread-1","status":"open","requires_response":true,"summary":"Safe bounded summary","raw_bytes":body.len()+data.len()}]})
    );
    let shown = fixture.ok(&["show", "--as-name", "receiver", "--json", "message-1"]);
    // A2aStore.messages has eleven persisted columns. Compare every field,
    // including nullable values and the unparsed data_json string, to the
    // existing full-message contract rather than checking only a field count.
    assert_eq!(
        shown,
        json!({
            "message_id":"message-1", "direction":"inbound", "sender":"sender",
            "recipient":"receiver", "body":body, "data_json":data,
            "created_at":"2026-09-26T12:00:00.000+00:00", "received_at":null,
            "delivery_status":"delivered", "status_reason":null, "read_at":null
        })
    );
    assert_eq!(std::fs::read(fixture.path()).unwrap(), before);
    let presentations: i64 = connection
        .query_row("SELECT count(*) FROM message_presentations", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(presentations, 0);
}

#[test]
fn unicode_envelope_budgets_count_characters_and_truncate_safely() {
    let fixture = Fixture::new();
    let connection = fixture.store(true);
    for number in 0..8 {
        let id = format!("m-{number}");
        insert(&connection, &id, "inbound", "retained body", None);
        lifecycle(&connection, &id, &"🦀界 ".repeat(100));
    }
    let output = fixture.run(&[
        "inbox",
        "--as-name",
        "receiver",
        "--json",
        "--max-chars",
        "600",
        "--preview-chars",
        "320",
    ]);
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    let payload: Value = serde_json::from_str(&text).unwrap();
    assert!(text.trim_end().chars().count() <= 600);
    assert!(text.len() > text.chars().count());
    assert!(payload["shown"].as_u64().unwrap() < 8);
    assert_eq!(payload["total"], 8);
    assert_eq!(payload["messages"][0]["id"], "m-7");
    assert!(payload["messages"][0]["summary"]
        .as_str()
        .unwrap()
        .ends_with('…'));
}

#[test]
fn legacy_store_without_lifecycle_tables_keeps_bounded_fallback() {
    let fixture = Fixture::new();
    let connection = fixture.store(false);
    insert(
        &connection,
        "legacy",
        "inbound",
        "line one\n  line two\r\n end",
        None,
    );
    let before = std::fs::read(fixture.path()).unwrap();
    let payload = fixture.ok(&["inbox", "--as-name", "receiver", "--json"]);
    assert_eq!(payload["messages"][0]["thread"], "legacy:sender");
    assert_eq!(payload["messages"][0]["summary"], "line one line two end");
    assert_eq!(payload["messages"][0]["status"], "open");
    assert_eq!(payload["messages"][0]["requires_response"], true);
    assert_eq!(
        fixture.ok(&["history", "--as-name", "receiver"]),
        json!({"schema_version":1,"available":false,"events":[]})
    );
    assert_eq!(std::fs::read(fixture.path()).unwrap(), before);
}

#[test]
fn summaries_preserve_python_whitespace_and_empty_received_at_semantics() {
    let fixture = Fixture::new();
    let connection = fixture.store(true);
    insert(&connection, "spacing", "inbound", "fallback body", None);
    lifecycle(
        &connection,
        "spacing",
        "\u{001c} one\u{001d}two\u{001e}three\u{001f} ",
    );
    connection
        .execute("UPDATE messages SET received_at=''", [])
        .unwrap();
    let payload = fixture.ok(&["inbox", "--as-name", "receiver", "--json"]);
    assert_eq!(payload["messages"][0]["summary"], "one two three");
    assert_eq!(
        payload["messages"][0]["at"],
        "2026-09-26T12:00:00.000+00:00"
    );
}

#[test]
fn bounded_body_and_metadata_previews_preserve_embedded_nul_and_unicode() {
    let fixture = Fixture::new();
    let connection = fixture.store(true);
    insert(
        &connection,
        "body",
        "inbound",
        "界🦀 a\0b following text",
        None,
    );
    insert(&connection, "metadata", "inbound", "unused body", None);
    lifecycle(&connection, "metadata", "🦀界 a\0b following summary");
    let output = fixture.run(&["inbox", "--as-name", "receiver", "--json"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains(r"a\u0000b"));
    let payload: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        payload["messages"][0]["summary"],
        "🦀界 a\0b following summary"
    );
    assert_eq!(
        payload["messages"][1]["summary"],
        "界🦀 a\0b following text"
    );
    assert!(text.trim_end().chars().count() <= 1200);
}

#[test]
fn bounded_utf8_prefix_allows_only_a_truncated_final_codepoint() {
    let fixture = Fixture::new();
    let connection = fixture.store(true);
    // 32 characters require at most 128 bytes. This prefix ends partway into
    // the 32nd crab; it is outside the first 32 displayed characters regardless.
    let source = format!("a{}", "🦀".repeat(50));
    insert(&connection, "body", "inbound", &source, None);
    insert(&connection, "metadata", "inbound", "body", None);
    lifecycle(&connection, "metadata", &source);
    let payload = fixture.ok(&[
        "inbox",
        "--as-name",
        "receiver",
        "--json",
        "--preview-chars",
        "32",
    ]);
    for message in payload["messages"].as_array().unwrap() {
        assert_eq!(message["summary"], format!("a{}", "🦀".repeat(31)));
    }
    connection
        .execute("UPDATE message_state SET summary=CAST(x'61f0' AS TEXT)", [])
        .unwrap();
    fixture.error(
        &["inbox", "--as-name", "receiver", "--json"],
        "incomplete utf-8",
    );
    connection.execute("DELETE FROM message_state", []).unwrap();
    connection
        .execute("UPDATE messages SET body=CAST(x'61ff62' AS TEXT)", [])
        .unwrap();
    fixture.error(
        &["inbox", "--as-name", "receiver", "--json"],
        "invalid utf-8",
    );
}

#[test]
fn missing_stores_do_not_initialize_and_unknown_identifiers_fail() {
    let fixture = Fixture::new();
    fixture.registry();
    let expected = json!({"schema_version":1,"available":false,"events":[]});
    assert_eq!(fixture.ok(&["history", "--as-name", "receiver"]), expected);
    fixture.error(
        &["inbox", "--as-name", "receiver", "--json"],
        "not initialized",
    );
    fixture.error(
        &["read", "--as-name", "receiver", "unknown"],
        "not initialized",
    );
    fixture.error(
        &["show", "--as-name", "receiver", "unknown"],
        "not initialized",
    );
    assert!(!fixture.root().join("receiver").exists());
    fixture.error(&["history", "--as-name", "missing"], "unknown identity");
    fixture.error(&["inbox", "--as-name", "../escape"], "invalid agent name");
    let connection = fixture.store(false);
    fixture.error(
        &["show", "--as-name", "receiver", "missing"],
        "unknown inbound message",
    );
    fixture.error(
        &["read", "--as-name", "receiver", "missing"],
        "unknown inbound message",
    );
    drop(connection);
}

#[test]
fn read_is_inbound_only_durable_and_does_not_resolve_or_present() {
    let fixture = Fixture::new();
    let connection = fixture.store(true);
    insert(&connection, "same-id", "inbound", "incoming", None);
    insert(&connection, "same-id", "outbound", "outgoing", None);
    lifecycle(&connection, "same-id", "action needed");
    let read = fixture.ok(&["read", "--as-name", "receiver", "same-id"]);
    assert_eq!(read["state"], "read");
    assert_eq!(read["sender"], "sender");
    assert!(read["read_at"].as_str().unwrap().ends_with("+00:00"));
    fixture.error(
        &["read", "--as-name", "receiver", "same-id"],
        "already read",
    );
    assert_eq!(
        fixture.ok(&["inbox", "--as-name", "receiver", "--json", "--unread"])["total"],
        0
    );
    let outbound = fixture.ok(&[
        "show",
        "--as-name",
        "receiver",
        "--direction",
        "outbound",
        "--json",
        "same-id",
    ]);
    assert!(outbound["read_at"].is_null());
    assert_eq!(outbound["body"], "outgoing");
    let state: (String, Option<String>) = connection
        .query_row(
            "SELECT protocol_status,resolved_at FROM message_state",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(state, ("open".into(), None));
    let presented: i64 = connection
        .query_row("SELECT count(*) FROM message_presentations", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(presented, 0);
}

#[test]
fn concurrent_read_has_exactly_one_successful_acknowledgment() {
    let fixture = Fixture::new();
    let connection = fixture.store(false);
    insert(&connection, "race", "inbound", "incoming", None);
    let first = fixture
        .command(&["read", "--as-name", "receiver", "race"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let second = fixture
        .command(&["read", "--as-name", "receiver", "race"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let outputs = [
        first.wait_with_output().unwrap(),
        second.wait_with_output().unwrap(),
    ];
    assert_eq!(outputs.iter().filter(|o| o.status.success()).count(), 1);
    assert!(outputs
        .iter()
        .filter(|o| !o.status.success())
        .all(|o| String::from_utf8_lossy(&o.stderr).contains("already read")));
}

#[test]
fn history_is_bounded_filtered_newest_first_and_read_only() {
    let fixture = Fixture::new();
    let connection = fixture.store(true);
    connection.execute_batch("INSERT INTO delivery_events VALUES(1,'a','time-1','queued','offline'),(2,'b','time-2','failed','rejected'),(3,'a','time-3','delivered',NULL)").unwrap();
    let before = std::fs::read(fixture.path()).unwrap();
    let events = fixture.ok(&["history", "--as-name", "receiver", "--limit", "2"]);
    assert_eq!(events["events"].as_array().unwrap().len(), 2);
    assert_eq!(events["events"][0]["event_id"], 3);
    assert_eq!(events["events"][1]["event_id"], 2);
    let filtered = fixture.ok(&["history", "--as-name", "receiver", "--message-id", "a"]);
    assert_eq!(filtered["events"][1]["event_id"], 1);
    assert!(filtered["events"][0]["reason"].is_null());
    assert_eq!(std::fs::read(fixture.path()).unwrap(), before);
    fixture.error(
        &["history", "--as-name", "receiver", "--limit", "1001"],
        "between 1 and 1000",
    );
}

#[test]
fn oversized_messages_are_refused_before_output_and_escaping_is_bounded() {
    let fixture = Fixture::new();
    let connection = fixture.store(false);
    insert(&connection, "large", "inbound", &"x".repeat(2000), None);
    insert(
        &connection,
        "escaped",
        "inbound",
        &"\u{0001}".repeat(500),
        None,
    );
    fixture.error(
        &[
            "show",
            "--as-name",
            "receiver",
            "--json",
            "--max-bytes",
            "1000",
            "large",
        ],
        "content was not loaded or printed",
    );
    fixture.error(
        &[
            "show",
            "--as-name",
            "receiver",
            "--json",
            "--max-bytes",
            "1000",
            "escaped",
        ],
        "rendered message",
    );
    let shown = fixture.ok(&[
        "show",
        "--as-name",
        "receiver",
        "--json",
        "--max-bytes",
        "4000",
        "escaped",
    ]);
    assert_eq!(shown["body"].as_str().unwrap().chars().count(), 500);
}

#[test]
fn corrupt_schema_database_and_structured_payload_fail_loudly() {
    let fixture = Fixture::new();
    let connection = fixture.store(true);
    insert(
        &connection,
        "bad-data",
        "inbound",
        "body",
        Some("{not json"),
    );
    fixture.error(
        &["show", "--as-name", "receiver", "--json", "bad-data"],
        "invalid stored data_json",
    );
    connection
        .execute_batch("DROP TABLE message_state; CREATE TABLE message_state(wrong TEXT)")
        .unwrap();
    fixture.error(
        &["inbox", "--as-name", "receiver", "--json"],
        "invalid mailbox preview schema",
    );
    connection
        .execute_batch("DROP TABLE delivery_events; CREATE TABLE delivery_events(wrong TEXT)")
        .unwrap();
    fixture.error(&["history", "--as-name", "receiver"], "no such column");
    drop(connection);
    std::fs::write(fixture.path(), b"corrupt non-SQLite database").unwrap();
    fixture.error(&["history", "--as-name", "receiver"], "not a database");
}

#[test]
fn invalid_types_and_oversized_metadata_are_rejected_without_acknowledgment() {
    let fixture = Fixture::new();
    let connection = fixture.store(false);
    insert(&connection, "bad-body", "inbound", "body", None);
    connection
        .execute("UPDATE messages SET body=x'00ff'", [])
        .unwrap();
    fixture.error(
        &["inbox", "--as-name", "receiver", "--json"],
        "invalid body/data types",
    );
    connection
        .execute(
            "UPDATE messages SET body='body',sender=?1",
            ["s".repeat(5000)],
        )
        .unwrap();
    fixture.error(
        &["read", "--as-name", "receiver", "bad-body"],
        "Invalid column type",
    );
    let read_at: Option<String> = connection
        .query_row("SELECT read_at FROM messages", [], |r| r.get(0))
        .unwrap();
    assert!(read_at.is_none());
}

#[test]
fn empty_or_incomplete_database_is_not_reported_as_missing_history() {
    let fixture = Fixture::new();
    fixture.registry();
    std::fs::create_dir_all(fixture.path().parent().unwrap()).unwrap();
    let connection = Connection::open(fixture.path()).unwrap();
    fixture.error(
        &["history", "--as-name", "receiver"],
        "messages table is missing",
    );
    connection
        .execute_batch("CREATE TABLE messages(message_id TEXT)")
        .unwrap();
    fixture.error(
        &["history", "--as-name", "receiver"],
        "invalid messages schema",
    );
}

#[test]
fn tiny_envelope_does_not_hide_oversized_identifiers_or_history_fields() {
    let fixture = Fixture::new();
    let connection = fixture.store(true);
    insert(&connection, &"z".repeat(100_000), "inbound", "body", None);
    fixture.error(
        &[
            "inbox",
            "--as-name",
            "receiver",
            "--json",
            "--max-chars",
            "256",
            "--max-messages",
            "1",
        ],
        "Invalid column type",
    );
    connection.execute("INSERT INTO delivery_events(message_id,occurred_at,status,reason) VALUES ('id','now','failed',?1)",["r".repeat(100_000)]).unwrap();
    fixture.error(
        &["history", "--as-name", "receiver", "--limit", "1"],
        "Invalid column type",
    );
}

#[cfg(unix)]
#[test]
fn mailbox_symlink_cannot_escape_selected_state_directory() {
    let fixture = Fixture::new();
    fixture.registry();
    let outside = fixture.host.join("outside");
    std::fs::create_dir(&outside).unwrap();
    let connection = Connection::open(outside.join("store.sqlite")).unwrap();
    connection.execute_batch(MESSAGES).unwrap();
    std::os::unix::fs::symlink(&outside, fixture.root().join("receiver")).unwrap();
    fixture.error(
        &["inbox", "--as-name", "receiver", "--json"],
        "escapes state directory",
    );
}

#[test]
fn explicit_state_directory_and_human_output_work_without_python() {
    let fixture = Fixture::new();
    let connection = fixture.store(false);
    insert(&connection, "human", "inbound", "a friendly message", None);
    let output = Command::new(env!("CARGO_BIN_EXE_forge"))
        .args(["mailbox", "--state-dir"])
        .arg(fixture.root())
        .args(["inbox", "--as-name", "receiver"])
        .env("PATH", "")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.starts_with("A2A compact agent=receiver total=1 shown=1 omitted=0"));
    assert!(text.contains("a friendly message"));
    assert!(text.contains("raw bytes withheld from context: 18"));
}

#[test]
fn synthetic_mailbox_probe_reports_totals_but_returns_only_bounded_previews() {
    let fixture = Fixture::new();
    let connection = fixture.store(true);
    let body = "x".repeat(2048);
    connection.execute("WITH RECURSIVE numbers(n) AS (VALUES(1) UNION ALL SELECT n+1 FROM numbers WHERE n<4096) INSERT INTO messages(message_id,direction,sender,recipient,body,created_at,delivery_status) SELECT printf('m-%05d',n),'inbound','sender','receiver',?1,'2026-09-26T12:00:00.000+00:00','delivered' FROM numbers",[body]).unwrap();
    insert(
        &connection,
        "z-huge",
        "inbound",
        &"界".repeat(700_000),
        None,
    );
    lifecycle(&connection, "z-huge", "bounded huge-body summary");
    let started = Instant::now();
    let output = fixture.run(&[
        "inbox",
        "--as-name",
        "receiver",
        "--json",
        "--max-messages",
        "3",
    ]);
    let elapsed = started.elapsed();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout).unwrap();
    let payload: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(payload["total"], 4097);
    assert_eq!(payload["shown"], 3);
    assert_eq!(payload["raw_bytes_not_injected"], 4096 * 2048 + 700_000 * 3);
    assert_eq!(
        payload["messages"][0]["summary"],
        "bounded huge-body summary"
    );
    assert!(text.trim_end().chars().count() <= 1200);
    assert!(!text.contains("界界界"));
    eprintln!(
        "mailbox probe: 4097 messages, {} raw bytes, {} output bytes, {elapsed:?}",
        4096 * 2048 + 700_000 * 3,
        text.len()
    );
}
