//! Native retention contract tests against disposable SQLite fixtures only.

use rusqlite::{params, Connection};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

const NOW: &str = "2026-08-30T12:00:00.000+00:00";
const OLD: &str = "2026-08-30T09:00:00.000+00:00";
const CUTOFF: &str = "2026-08-30T11:00:00.000+00:00";

const SCHEMA: &str = "
CREATE TABLE messages (
 message_id TEXT NOT NULL,direction TEXT NOT NULL,sender TEXT NOT NULL,
 recipient TEXT NOT NULL,body TEXT NOT NULL,data_json TEXT,created_at TEXT NOT NULL,
 received_at TEXT,delivery_status TEXT NOT NULL,status_reason TEXT,read_at TEXT,
 PRIMARY KEY(direction,message_id));
CREATE TABLE message_state (
 direction TEXT NOT NULL,message_id TEXT NOT NULL,thread_id TEXT NOT NULL,
 summary TEXT NOT NULL,protocol_status TEXT NOT NULL,requires_response INTEGER NOT NULL,
 retention_class TEXT NOT NULL,resolved_at TEXT,superseded_at TEXT,hold_reason TEXT,
 tombstoned_at TEXT,body_sha256 TEXT NOT NULL,body_bytes INTEGER NOT NULL,
 data_sha256 TEXT,data_bytes INTEGER NOT NULL,PRIMARY KEY(direction,message_id),
 FOREIGN KEY(direction,message_id) REFERENCES messages(direction,message_id));
CREATE TABLE retention_events (
 event_id TEXT PRIMARY KEY,direction TEXT NOT NULL,message_id TEXT NOT NULL,
 policy_version INTEGER NOT NULL,manifest_json TEXT NOT NULL,
 manifest_sha256 TEXT NOT NULL,compacted_at TEXT NOT NULL,
 UNIQUE(direction,message_id),
 FOREIGN KEY(direction,message_id) REFERENCES messages(direction,message_id));";

struct Fixture {
    host: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let host = std::env::temp_dir().join(format!(
            "forge-a2a-retention-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&host).unwrap();
        Self { host }
    }

    fn state(&self) -> PathBuf {
        self.host.join(".agents/a2a")
    }
    fn store_path(&self, name: &str) -> PathBuf {
        self.state().join(name).join("store.sqlite")
    }
    fn store(&self, name: &str) -> Connection {
        let path = self.store_path(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        conn
    }
    fn command(&self, args: &[&str]) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_forge"));
        command
            .args(["mailbox", "--host"])
            .arg(&self.host)
            .arg("retention");
        if !args
            .iter()
            .any(|arg| *arg == "--now" || arg.starts_with("--now="))
        {
            command.args(["--now", NOW]);
        }
        if !args
            .iter()
            .any(|arg| *arg == "--grace-hours" || arg.starts_with("--grace-hours="))
        {
            command.args(["--grace-hours", "1"]);
        }
        command.args(args).output().unwrap()
    }
    fn ok(&self, args: &[&str]) -> Value {
        let output = self.command(args);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }
    fn error(&self, args: &[&str], expected: &str) {
        let output = self.command(args);
        assert!(!output.status.success());
        assert!(output.stdout.is_empty(), "failed command printed a receipt");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains(expected),
            "expected {expected:?} in {stderr}"
        );
    }
    fn evidence(&self, name: &str, content: &str) {
        let path = self.host.join("research/reports").join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.host).unwrap();
    }
}

fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn insert(conn: &Connection, id: &str, body: &str, data: Option<&str>) {
    conn.execute(
        "INSERT INTO messages(message_id,direction,sender,recipient,body,data_json,
        created_at,received_at,delivery_status,status_reason,read_at)
        VALUES(?1,'inbound','sender','worker',?2,?3,?4,?4,'delivered',NULL,?4)",
        params![id, body, data, OLD],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO message_state(direction,message_id,thread_id,summary,
        protocol_status,requires_response,retention_class,resolved_at,superseded_at,
        hold_reason,tombstoned_at,body_sha256,body_bytes,data_sha256,data_bytes)
        VALUES('inbound',?1,'thread-1',?2,'resolved',0,'operational',?3,NULL,
        NULL,NULL,?4,?5,?6,?7)",
        params![
            id,
            format!("summary {id}"),
            OLD,
            hash(body.as_bytes()),
            body.len() as i64,
            data.map(|value| hash(value.as_bytes())),
            data.map_or(0, str::len) as i64
        ],
    )
    .unwrap();
}

fn count(conn: &Connection, table: &str) -> i64 {
    conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
        row.get(0)
    })
    .unwrap()
}

#[test]
fn preview_is_read_only_and_apply_writes_exact_manifest_receipt() {
    let fixture = Fixture::new();
    let conn = fixture.store("worker");
    let data = r#"{"kind":"coordination-v2","summary":"Done 🦀","unrelated":"discard"}"#;
    insert(&conn, "eligible", "finished 🦀", Some(data));
    let before = std::fs::read(fixture.store_path("worker")).unwrap();
    let preview = fixture.ok(&["--store", "worker"]);
    assert_eq!(preview["authority"], "deterministic-a2a-retention");
    assert_eq!(preview["automatic"], false);
    assert_eq!(preview["mode"], "preview");
    assert_eq!(preview["results"][0]["eligible"], 1);
    assert_eq!(preview["results"][0]["compacted"], 0);
    assert_eq!(
        preview["results"][0]["original_content_bytes"],
        "finished 🦀".len() + data.len()
    );
    assert_eq!(std::fs::read(fixture.store_path("worker")).unwrap(), before);
    assert_eq!(count(&conn, "retention_events"), 0);

    let applied = fixture.ok(&["--store", "worker", "--as-name", "worker", "--apply"]);
    assert_eq!(applied["results"][0]["compacted"], 1);
    assert_eq!(
        applied["results"][0]["manifest_sha256"],
        preview["results"][0]["manifest_sha256"]
    );
    let (body, data_after): (String, Option<String>) = conn
        .query_row(
            "SELECT body,data_json FROM messages WHERE message_id='eligible'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(body, "[compacted: resolved A2A content retained by digest]");
    assert_eq!(data_after, None);
    let (manifest_json, digest, event_id): (String, String, String) = conn
        .query_row(
            "SELECT manifest_json,manifest_sha256,event_id FROM retention_events",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    let manifest: Value = serde_json::from_str(&manifest_json).unwrap();
    assert_eq!(manifest["structured"]["summary"], "Done 🦀");
    assert!(manifest["structured"].get("unrelated").is_none());
    assert_eq!(manifest["body_sha256"], hash("finished 🦀".as_bytes()));
    assert_eq!(manifest["manifest_sha256"], digest);
    let mut without_digest = manifest.clone();
    without_digest
        .as_object_mut()
        .unwrap()
        .remove("manifest_sha256");
    without_digest.sort_all_objects();
    assert_eq!(
        digest,
        hash(serde_json::to_string(&without_digest).unwrap().as_bytes())
    );
    assert_eq!(
        event_id,
        hash(format!("2\0inbound\0eligible\0{digest}").as_bytes())
    );
    assert_eq!(
        fixture.ok(&["--store", "worker"])["results"][0]["eligible"],
        0
    );
}

#[test]
fn apply_requires_one_exact_actor_and_cannot_escape_state_dir() {
    let fixture = Fixture::new();
    let conn = fixture.store("worker");
    let other = fixture.store("other");
    insert(&conn, "eligible", "content", None);
    insert(&other, "kept", "keep other store", None);
    fixture.error(&["--apply"], "exactly one explicit --store");
    fixture.error(&["--store", "worker", "--apply"], "--as-name matching");
    fixture.error(
        &["--store", "worker", "--as-name", "other", "--apply"],
        "--as-name matching",
    );
    fixture.error(
        &["--store", "worker", "--store", "worker"],
        "duplicate --store",
    );
    fixture.error(&["--store", "../worker"], "invalid agent name");
    assert_eq!(count(&conn, "retention_events"), 0);

    let outside = fixture.host.join("outside.sqlite");
    std::fs::copy(fixture.store_path("worker"), &outside).unwrap();
    let link = fixture.store_path("escape");
    std::fs::create_dir_all(link.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&outside, &link).unwrap();
    fixture.error(
        &["--store", "escape", "--as-name", "escape", "--apply"],
        "escapes the state directory",
    );
    fixture.ok(&["--store", "worker", "--as-name", "worker", "--apply"]);
    assert_eq!(count(&conn, "retention_events"), 1);
    assert_eq!(count(&other, "retention_events"), 0);
    let body: String = other
        .query_row("SELECT body FROM messages", [], |row| row.get(0))
        .unwrap();
    assert_eq!(body, "keep other store");
}

fn insert_exclusion_rows(conn: &Connection) {
    for id in [
        "eligible",
        "unread",
        "unresolved",
        "held",
        "pinned",
        "required",
        "protected",
        "new",
        "blocked",
        "superseded",
        "gate",
        "legacy-evidence",
        "outbound",
    ] {
        insert(conn, id, "content", None);
    }
    conn.execute(
        "UPDATE messages SET read_at=NULL WHERE message_id='unread'",
        [],
    )
    .unwrap();
    conn.execute(
        "UPDATE message_state SET protocol_status='open' WHERE message_id='unresolved'",
        [],
    )
    .unwrap();
    conn.execute(
        "UPDATE message_state SET hold_reason='keep' WHERE message_id='held'",
        [],
    )
    .unwrap();
    conn.execute(
        "UPDATE message_state SET retention_class='pinned' WHERE message_id='pinned'",
        [],
    )
    .unwrap();
    conn.execute(
        "UPDATE message_state SET requires_response=1 WHERE message_id='required'",
        [],
    )
    .unwrap();
    conn.execute(
        "UPDATE message_state SET resolved_at=?1 WHERE message_id='new'",
        ["2026-08-30T11:00:00.001+00:00"],
    )
    .unwrap();
    conn.execute(
        "UPDATE message_state SET protocol_status='blocked' WHERE message_id='blocked'",
        [],
    )
    .unwrap();
    conn.execute("UPDATE message_state SET resolved_at=NULL,superseded_at=?1,protocol_status='superseded' WHERE message_id='superseded'", [OLD])
        .unwrap();
    conn.execute("UPDATE message_state SET retention_class='pinned' WHERE message_id IN ('gate','legacy-evidence')", [])
        .unwrap();
    conn.execute(
        "UPDATE messages SET data_json=?1 WHERE message_id='gate'",
        [r#"{"kind":"gate-review","gate":"promotion"}"#],
    )
    .unwrap();
    conn.execute(
        "UPDATE messages SET data_json=?1 WHERE message_id='legacy-evidence'",
        [r#"{"kind":"message","receipt":"immutable"}"#],
    )
    .unwrap();
    conn.execute_batch(
        "BEGIN; PRAGMA defer_foreign_keys=ON;
         UPDATE messages SET direction='outbound' WHERE message_id='outbound';
         UPDATE message_state SET direction='outbound' WHERE message_id='outbound';
         COMMIT;",
    )
    .unwrap();
}

#[test]
fn selection_excludes_unread_unresolved_held_pinned_outbound_and_evidence() {
    let fixture = Fixture::new();
    let conn = fixture.store("worker");
    insert_exclusion_rows(&conn);
    fixture.evidence(
        "nested/promotion_gate_receipt.json",
        r#"{"nested":{"message_id":"protected"}}"#,
    );
    fixture.evidence("not-evidence.json", r#"{"message_id":"eligible"}"#);
    let preview = fixture.ok(&["--store", "worker"]);
    assert_eq!(preview["results"][0]["eligible"], 2);
    assert_eq!(preview["results"][0]["evidence_files"], 1);
    assert_eq!(preview["results"][0]["evidence_protected"], 1);
    fixture.ok(&["--store", "worker", "--as-name", "worker", "--apply"]);
    let mut stmt = conn
        .prepare("SELECT message_id FROM retention_events ORDER BY message_id")
        .unwrap();
    let ids = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(ids, ["eligible", "superseded"]);
}

#[test]
fn malformed_evidence_and_content_drift_fail_without_writes() {
    let fixture = Fixture::new();
    let conn = fixture.store("worker");
    insert(&conn, "eligible", "original", None);
    fixture.evidence("receipt.json", "not json");
    fixture.error(
        &["--store", "worker", "--as-name", "worker", "--apply"],
        "malformed JSON",
    );
    assert_eq!(count(&conn, "retention_events"), 0);
    fixture.evidence("receipt.json", "{}");
    conn.execute(
        "UPDATE messages SET body='changed' WHERE message_id='eligible'",
        [],
    )
    .unwrap();
    fixture.error(
        &["--store", "worker", "--as-name", "worker", "--apply"],
        "content drifted",
    );
    assert_eq!(count(&conn, "retention_events"), 0);
    let body: String = conn
        .query_row(
            "SELECT body FROM messages WHERE message_id='eligible'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(body, "changed");
}

#[test]
fn evidence_symlink_escape_and_size_limit_fail_closed() {
    let fixture = Fixture::new();
    let conn = fixture.store("worker");
    insert(&conn, "eligible", "original", None);
    let outside = fixture
        .host
        .parent()
        .unwrap()
        .join(format!("outside-{}.json", std::process::id()));
    std::fs::write(&outside, "{}").unwrap();
    let link = fixture.host.join("research/reports/gate.json");
    std::fs::create_dir_all(link.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&outside, &link).unwrap();
    fixture.error(
        &["--store", "worker", "--as-name", "worker", "--apply"],
        "evidence path escapes root",
    );
    std::fs::remove_file(&link).unwrap();
    std::fs::remove_file(outside).unwrap();
    std::fs::write(&link, vec![b' '; (2 << 20) + 1]).unwrap();
    // A size failure still wins when opening the file would be denied.
    std::fs::set_permissions(&link, std::fs::Permissions::from_mode(0o000)).unwrap();
    fixture.error(
        &["--store", "worker", "--as-name", "worker", "--apply"],
        "exceeds 2097152 bytes",
    );
    std::fs::set_permissions(&link, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(count(&conn, "retention_events"), 0);
}

#[test]
fn whole_batch_rolls_back_when_a_trigger_changes_later_content() {
    let fixture = Fixture::new();
    let conn = fixture.store("worker");
    insert(&conn, "first", "original one", None);
    insert(&conn, "second", "original two", None);
    conn.execute_batch(
        "CREATE TRIGGER drift_content AFTER UPDATE OF body ON messages
        WHEN NEW.message_id='first' BEGIN
        UPDATE messages SET body='concurrent edit' WHERE message_id='second'; END;",
    )
    .unwrap();
    fixture.error(
        &["--store", "worker", "--as-name", "worker", "--apply"],
        "changed during retention",
    );
    assert_eq!(count(&conn, "retention_events"), 0);
    let bodies = conn
        .prepare("SELECT body FROM messages ORDER BY message_id")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(bodies, ["original one", "original two"]);
}

#[test]
fn whole_batch_rolls_back_when_a_trigger_changes_later_lifecycle_fields() {
    let cases = [
        "summary='changed summary'",
        "protocol_status='blocked'",
        "requires_response=1",
        "data_sha256='0000'",
        "data_bytes=data_bytes+1",
    ];
    for (index, mutation) in cases.into_iter().enumerate() {
        let fixture = Fixture::new();
        let conn = fixture.store("worker");
        insert(&conn, "first", "original one", None);
        insert(&conn, "second", "original two", None);
        conn.execute_batch(&format!(
            "CREATE TRIGGER drift_state AFTER UPDATE OF body ON messages
            WHEN NEW.message_id='first' BEGIN
            UPDATE message_state SET {mutation} WHERE message_id='second'; END;"
        ))
        .unwrap();
        fixture.error(
            &["--store", "worker", "--as-name", "worker", "--apply"],
            "lifecycle changed during retention",
        );
        assert_eq!(count(&conn, "retention_events"), 0, "case {index}");
        let body: String = conn
            .query_row(
                "SELECT body FROM messages WHERE message_id='first'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(body, "original one", "case {index}");
    }
}

#[test]
fn tombstone_text_does_not_drive_eligibility_and_apply_is_idempotent() {
    let fixture = Fixture::new();
    let conn = fixture.store("worker");
    insert(
        &conn,
        "collision",
        "[compacted: resolved A2A content retained by digest]",
        None,
    );
    let first = fixture.ok(&["--store", "worker", "--as-name", "worker", "--apply"]);
    assert_eq!(first["results"][0]["compacted"], 1);
    let second = fixture.ok(&["--store", "worker", "--as-name", "worker", "--apply"]);
    assert_eq!(second["results"][0]["eligible"], 0);
    assert_eq!(second["results"][0]["compacted"], 0);
    assert_eq!(count(&conn, "retention_events"), 1);
    let tombstoned_at: Option<String> = conn
        .query_row(
            "SELECT tombstoned_at FROM message_state WHERE message_id='collision'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(tombstoned_at.as_deref(), Some(NOW));
}

#[test]
fn preexisting_content_metadata_drift_fails_before_any_write() {
    for mutation in [
        "body_sha256='0000'",
        "body_bytes=body_bytes+1",
        "data_sha256='0000'",
        "data_bytes=data_bytes+1",
    ] {
        let fixture = Fixture::new();
        let conn = fixture.store("worker");
        insert(
            &conn,
            "eligible",
            "original",
            Some(r#"{"kind":"coordination"}"#),
        );
        conn.execute_batch(&format!("UPDATE message_state SET {mutation}"))
            .unwrap();
        fixture.error(
            &["--store", "worker", "--as-name", "worker", "--apply"],
            "content drifted",
        );
        assert_eq!(count(&conn, "retention_events"), 0);
        let body: String = conn
            .query_row("SELECT body FROM messages", [], |row| row.get(0))
            .unwrap();
        assert_eq!(body, "original");
    }
}

#[test]
fn bounded_batch_order_limit_and_timestamp_validation_fail_closed() {
    let fixture = Fixture::new();
    let conn = fixture.store("worker");
    for id in ["charlie", "alpha", "bravo"] {
        insert(&conn, id, "content", None);
    }
    fixture.error(&["--store", "worker", "--limit", "0"], "retention limit");
    fixture.error(&["--store", "worker", "--limit", "1001"], "retention limit");
    fixture.error(
        &["--store", "worker", "--grace-hours", "0.9"],
        "retention grace",
    );
    fixture.error(
        &["--store", "worker", "--now", "2026-02-31T12:00:00+00:00"],
        "not a valid",
    );
    assert_eq!(count(&conn, "retention_events"), 0);
    let applied = fixture.ok(&[
        "--store",
        "worker",
        "--as-name",
        "worker",
        "--limit",
        "2",
        "--apply",
    ]);
    assert_eq!(applied["results"][0]["compacted"], 2);
    let ids = conn
        .prepare("SELECT message_id FROM retention_events ORDER BY message_id")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(ids, ["alpha", "bravo"]);
}

#[test]
fn future_read_timestamp_and_nonfile_evidence_abort_without_changes() {
    let fixture = Fixture::new();
    let conn = fixture.store("worker");
    insert(&conn, "future-read", "content", None);
    conn.execute(
        "UPDATE messages SET read_at='2026-08-30T13:00:00.000+00:00'",
        [],
    )
    .unwrap();
    fixture.error(
        &["--store", "worker", "--as-name", "worker", "--apply"],
        "future A2A timestamps",
    );
    assert_eq!(count(&conn, "retention_events"), 0);
    conn.execute("UPDATE messages SET read_at=?1", [OLD])
        .unwrap();
    std::fs::create_dir_all(fixture.host.join("research/reports/unsafe_receipt.json")).unwrap();
    fixture.error(
        &["--store", "worker", "--as-name", "worker", "--apply"],
        "not a regular file",
    );
    assert_eq!(count(&conn, "retention_events"), 0);
}

#[test]
fn cutoff_is_inclusive_but_latest_terminal_timestamp_controls() {
    let fixture = Fixture::new();
    let conn = fixture.store("worker");
    insert(&conn, "at-cutoff", "old", None);
    insert(&conn, "later-transition", "old", None);
    conn.execute(
        "UPDATE message_state SET resolved_at=?1 WHERE message_id='at-cutoff'",
        [CUTOFF],
    )
    .unwrap();
    conn.execute("UPDATE message_state SET superseded_at='2026-08-30T11:00:00.001+00:00' WHERE message_id='later-transition'", []).unwrap();
    let preview = fixture.ok(&["--store", "worker"]);
    assert_eq!(preview["results"][0]["eligible"], 1);
    assert_eq!(preview["results"][0]["compacted"], 0);
}
