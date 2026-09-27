//! Native-only A2A enqueue tests against the existing transport's SQLite shape.

use rusqlite::Connection;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{symlink, OpenOptionsExt};
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

const MESSAGES: &str = "
CREATE TABLE messages (
 message_id TEXT NOT NULL,direction TEXT NOT NULL CHECK(direction IN('inbound','outbound')),
 sender TEXT NOT NULL,recipient TEXT NOT NULL,body TEXT NOT NULL,data_json TEXT,
 created_at TEXT NOT NULL,received_at TEXT,delivery_status TEXT NOT NULL,
 status_reason TEXT,read_at TEXT,PRIMARY KEY(direction,message_id));
CREATE INDEX messages_inbox ON messages(direction,read_at,created_at);";

const MIGRATION: &str = "
CREATE TABLE delivery_events (
 event_id INTEGER PRIMARY KEY,message_id TEXT NOT NULL,occurred_at TEXT NOT NULL,
 status TEXT NOT NULL,reason TEXT);
CREATE TABLE message_state (
 direction TEXT NOT NULL,message_id TEXT NOT NULL,thread_id TEXT NOT NULL,
 summary TEXT NOT NULL,protocol_status TEXT NOT NULL,
 requires_response INTEGER NOT NULL CHECK(requires_response IN(0,1)),
 retention_class TEXT NOT NULL CHECK(retention_class IN('pinned','operational')),
 resolved_at TEXT,superseded_at TEXT,hold_reason TEXT,tombstoned_at TEXT,
 body_sha256 TEXT NOT NULL,body_bytes INTEGER NOT NULL,data_sha256 TEXT,
 data_bytes INTEGER NOT NULL,PRIMARY KEY(direction,message_id),
 FOREIGN KEY(direction,message_id) REFERENCES messages(direction,message_id) ON DELETE CASCADE);
CREATE TABLE retention_events (
 event_id TEXT PRIMARY KEY,direction TEXT NOT NULL,message_id TEXT NOT NULL,
 policy_version INTEGER NOT NULL,manifest_json TEXT NOT NULL,
 manifest_sha256 TEXT NOT NULL,compacted_at TEXT NOT NULL,
 UNIQUE(direction,message_id));
CREATE TABLE message_presentations (
 direction TEXT NOT NULL,message_id TEXT NOT NULL,presented_at TEXT NOT NULL,
 PRIMARY KEY(direction,message_id));";

struct Fixture {
    host: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let host = std::env::temp_dir().join(format!(
            "forge-queue-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&host).unwrap();
        Self { host }
    }

    fn root(&self) -> PathBuf {
        self.host.join(".agents/a2a")
    }
    fn path(&self) -> PathBuf {
        self.root().join("sender/store.sqlite")
    }
    fn lock_path(&self) -> PathBuf {
        self.root().join("sender/.delivery.lock")
    }

    fn registry(&self) {
        fs::create_dir_all(self.root()).unwrap();
        fs::write(
            self.root().join("agents.json"),
            json!({"schema_version":1,"agents":{
                "sender":{"port":7398,"token":"synthetic-sender-token"},
                "recipient":{"port":7399,"token":"synthetic-recipient-token"}
            }})
            .to_string(),
        )
        .unwrap();
    }

    fn store(&self, migrated: bool) -> Connection {
        self.registry();
        fs::create_dir_all(self.path().parent().unwrap()).unwrap();
        let connection = Connection::open(self.path()).unwrap();
        connection.execute_batch(MESSAGES).unwrap();
        if migrated {
            connection.execute_batch(MIGRATION).unwrap();
        }
        connection
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_forge"));
        command
            .args(["mailbox", "--host"])
            .arg(&self.host)
            .args(["enqueue", "--from-name", "sender", "--to", "recipient"])
            .env("PATH", "")
            .env("CONDUCTOR_PYTHON", "/bin/false");
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command().args(args).output().unwrap()
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
        assert!(output.stdout.is_empty(), "failed enqueue emitted a receipt");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains(fragment), "expected {fragment:?}: {stderr}");
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.host).unwrap();
    }
}

fn count(connection: &Connection, table: &str) -> i64 {
    connection
        .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .unwrap()
}

#[test]
fn plain_enqueue_is_queued_only_and_preserves_transport_receipt_and_fifo() {
    let fixture = Fixture::new();
    let connection = fixture.store(true);
    let first = fixture.ok(&["--body", "first private body"]);
    let second = fixture.ok(&["--body", "second private body"]);
    for (receipt, expected_body) in [
        (&first, "first private body"),
        (&second, "second private body"),
    ] {
        assert_eq!(receipt["schema_version"], 1);
        assert_eq!(receipt["authority"], "a2a-delivery-receipt");
        assert_eq!(receipt["sender"], "sender");
        assert_eq!(receipt["recipient"], "recipient");
        assert_eq!(receipt["delivery_status"], "queued");
        assert_eq!(receipt["status_reason"], "awaiting explicit flush");
        assert_eq!(receipt["protocol_status"], "open");
        assert_eq!(receipt["requires_response"], 1);
        assert_eq!(receipt["body_bytes"], expected_body.len());
        assert!(receipt["data_sha256"].is_null());
        assert_eq!(receipt["data_bytes"], 0);
        assert!(receipt["received_at"].is_null());
        assert_eq!(receipt["message_id"].as_str().unwrap().len(), 36);
    }
    assert_ne!(first["message_id"], second["message_id"]);
    let rows: Vec<String> = connection.prepare("SELECT message_id FROM messages WHERE direction='outbound' AND delivery_status='queued' ORDER BY created_at,rowid")
        .unwrap().query_map([], |row| row.get(0)).unwrap().map(Result::unwrap).collect();
    assert_eq!(
        rows,
        vec![
            first["message_id"].as_str().unwrap().to_owned(),
            second["message_id"].as_str().unwrap().to_owned()
        ]
    );
    assert_eq!(count(&connection, "messages"), 2);
    assert_eq!(count(&connection, "message_state"), 2);
    assert_eq!(count(&connection, "delivery_events"), 2);
    assert_eq!(count(&connection, "message_presentations"), 0);
    assert_eq!(count(&connection, "retention_events"), 0);
    let output = fixture.run(&["--body", "another secret"]);
    let receipt: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(receipt.get("body").is_none());
    assert!(receipt.get("data_json").is_none());
    assert_eq!(count(&connection, "delivery_events"), 3);
}

#[test]
fn unicode_coordination_v2_reuses_compaction_metadata_and_stored_byte_digests() {
    let fixture = Fixture::new();
    let connection = fixture.store(true);
    let data_path = fixture.host.join("data.json");
    fs::write(
        &data_path,
        json!({"kind":"coordination-v2","thread_id":"thread:alpha",
        "summary":"  🦀  triage\u{001c} now  ","status":"blocked",
        "requires_response":true,"supersedes":["prior-1"]})
        .to_string(),
    )
    .unwrap();
    let body = "界🦀 a\0b";
    let body_path = fixture.host.join("body.txt");
    fs::write(&body_path, body).unwrap();
    let receipt = fixture
        .command()
        .arg("--body-file")
        .arg(&body_path)
        .arg("--data-file")
        .arg(&data_path)
        .output()
        .unwrap();
    assert!(
        receipt.status.success(),
        "{}",
        String::from_utf8_lossy(&receipt.stderr)
    );
    let receipt: Value = serde_json::from_slice(&receipt.stdout).unwrap();
    assert_eq!(receipt["thread_id"], "thread:alpha");
    assert_eq!(receipt["summary"], "🦀 triage now");
    assert_eq!(receipt["protocol_status"], "blocked");
    assert_eq!(receipt["requires_response"], 1);
    let row: (String,String,String,i64,String,i64) = connection.query_row(
        "SELECT m.body,m.data_json,s.retention_class,s.body_bytes,s.protocol_status,s.requires_response FROM messages m JOIN message_state s ON s.direction=m.direction AND s.message_id=m.message_id WHERE m.message_id=?1 AND m.direction='outbound'",
        [receipt["message_id"].as_str().unwrap()],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?))).unwrap();
    assert_eq!(row.0, body);
    assert_eq!(row.2, "operational");
    assert_eq!(row.3, body.len() as i64);
    assert_eq!(row.4, "blocked");
    assert_eq!(row.5, 1);
    let body_digest = format!("{:x}", Sha256::digest(row.0.as_bytes()));
    let data_digest = format!("{:x}", Sha256::digest(row.1.as_bytes()));
    assert_eq!(receipt["body_sha256"], body_digest);
    assert_eq!(receipt["data_sha256"], data_digest);
    assert_eq!(receipt["data_bytes"], row.1.len());
    assert_eq!(
        serde_json::from_str::<Value>(&row.1).unwrap()["summary"],
        "  🦀  triage\u{001c} now  "
    );
}

#[test]
fn invalid_coordination_and_data_kinds_leave_the_store_untouched() {
    let fixture = Fixture::new();
    let connection = fixture.store(true);
    let data_path = fixture.host.join("invalid.json");
    fs::write(
        &data_path,
        r#"{"kind":"coordination-v2","thread_id":"bad space"}"#,
    )
    .unwrap();
    fixture
        .command()
        .args(["--body", "text", "--data-file"])
        .arg(&data_path)
        .output()
        .map(|out| assert!(!out.status.success()))
        .unwrap();
    fs::write(&data_path, r#"{"kind":"unknown"}"#).unwrap();
    fixture
        .command()
        .args(["--body", "text", "--data-file"])
        .arg(&data_path)
        .output()
        .map(|out| assert!(!out.status.success()))
        .unwrap();
    assert_eq!(count(&connection, "messages"), 0);
    assert_eq!(count(&connection, "message_state"), 0);
    assert_eq!(count(&connection, "delivery_events"), 0);
    assert!(!fixture.lock_path().exists());
}

#[test]
fn missing_registry_store_and_migration_fail_without_initialization() {
    let fixture = Fixture::new();
    fixture.error(&["--body", "text"], "reading registry");
    fixture.registry();
    fixture.error(&["--body", "text"], "not initialized");
    assert!(!fixture.root().join("sender").exists());
    let connection = fixture.store(false);
    fixture.error(&["--body", "text"], "not fully migrated");
    assert_eq!(count(&connection, "messages"), 0);
    assert!(!fixture.lock_path().exists());
    let mut registry: Value =
        serde_json::from_slice(&fs::read(fixture.root().join("agents.json")).unwrap()).unwrap();
    registry["agents"]
        .as_object_mut()
        .unwrap()
        .remove("recipient");
    fs::write(fixture.root().join("agents.json"), registry.to_string()).unwrap();
    fixture.error(&["--body", "text"], "unknown identity");
    assert_eq!(count(&connection, "messages"), 0);
}

#[test]
fn bounded_body_and_data_files_refuse_oversize_and_non_utf8() {
    let fixture = Fixture::new();
    let connection = fixture.store(true);
    let body = fixture.host.join("body.txt");
    fs::write(&body, vec![b'x'; 262_145]).unwrap();
    let output = fixture
        .command()
        .arg("--body-file")
        .arg(&body)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("body exceeds 262144 bytes"));
    fs::write(&body, [0xff]).unwrap();
    let output = fixture
        .command()
        .arg("--body-file")
        .arg(&body)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("body is not UTF-8"));
    let data = fixture.host.join("data.json");
    fs::write(&data, vec![b' '; 1_048_577]).unwrap();
    let output = fixture
        .command()
        .args(["--body", "safe", "--data-file"])
        .arg(&data)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("data payload exceeds 1048576 bytes"));
    assert_eq!(count(&connection, "messages"), 0);
    assert!(!fixture.lock_path().exists());
}

#[test]
fn failed_event_insert_rolls_back_the_message_and_state() {
    let fixture = Fixture::new();
    let connection = fixture.store(true);
    connection.execute_batch("CREATE TRIGGER reject_queue BEFORE INSERT ON delivery_events BEGIN SELECT RAISE(ABORT,'injected delivery event failure'); END;").unwrap();
    fixture.error(&["--body", "rollback"], "injected delivery event failure");
    assert_eq!(count(&connection, "messages"), 0);
    assert_eq!(count(&connection, "message_state"), 0);
    assert_eq!(count(&connection, "delivery_events"), 0);
}

#[test]
fn sender_lock_contention_fails_cleanly_then_succeeds() {
    let fixture = Fixture::new();
    let connection = fixture.store(true);
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(fixture.lock_path())
        .unwrap();
    // SAFETY: the test owns this descriptor until explicitly unlocking it.
    assert_eq!(
        unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
    fixture.error(&["--body", "waiting"], "active delivery");
    assert_eq!(count(&connection, "messages"), 0);
    assert_eq!(unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_UN) }, 0);
    drop(lock);
    assert_eq!(
        fixture.ok(&["--body", "waiting"])["delivery_status"],
        "queued"
    );
}

#[test]
fn symlinked_delivery_lock_cannot_escape_sender_directory() {
    let fixture = Fixture::new();
    let connection = fixture.store(true);
    let outside = fixture.host.join("outside-lock");
    fs::write(&outside, b"unchanged").unwrap();
    symlink(&outside, fixture.lock_path()).unwrap();
    let output = fixture.run(&["--body", "safe"]);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert_eq!(fs::read(&outside).unwrap(), b"unchanged");
    assert_eq!(count(&connection, "messages"), 0);
}
