//! Local fixture tests for native A2A send and retry. No Python runtime or peer is launched.

use rusqlite::Connection;
use serde_json::{json, Value};
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

// Offline/retry cases deliberately close and rebind a port. Keep that lifecycle
// exclusive so another fixture cannot acquire the temporarily unbound endpoint.
static ENDPOINT_LIFECYCLE: Mutex<()> = Mutex::new(());

const SCHEMA: &str = "
CREATE TABLE messages (
 message_id TEXT NOT NULL,direction TEXT NOT NULL CHECK(direction IN('inbound','outbound')),
 sender TEXT NOT NULL,recipient TEXT NOT NULL,body TEXT NOT NULL,data_json TEXT,
 created_at TEXT NOT NULL,received_at TEXT,delivery_status TEXT NOT NULL,
 status_reason TEXT,read_at TEXT,PRIMARY KEY(direction,message_id));
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
    port: u16,
}

impl Fixture {
    fn new(port: u16) -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let host = std::env::temp_dir().join(format!(
            "forge-transport-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(host.join(".agents/a2a/sender")).unwrap();
        let fixture = Self { host, port };
        fixture.registry();
        fixture.store().execute_batch(SCHEMA).unwrap();
        fixture
    }

    fn root(&self) -> PathBuf {
        self.host.join(".agents/a2a")
    }

    fn store(&self) -> Connection {
        Connection::open(self.root().join("sender/store.sqlite")).unwrap()
    }

    fn registry(&self) {
        fs::write(
            self.root().join("agents.json"),
            json!({"schema_version":1,"agents":{
                "sender":{"port":7398,"token":"synthetic-sender-token"},
                "recipient":{"port":self.port,"token":"synthetic-recipient-token"}
            }})
            .to_string(),
        )
        .unwrap();
    }

    fn command(&self, action: &str) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_forge"));
        command
            .args(["mailbox", "--host"])
            .arg(&self.host)
            .arg(action)
            .env("PATH", "")
            .env("CONDUCTOR_PYTHON", "/bin/false");
        command
    }

    fn send(&self, body: &str) -> Output {
        self.command("send")
            .args(["--from-name", "sender", "--to", "recipient", "--body", body])
            .output()
            .unwrap()
    }

    fn flush(&self, limit: usize) -> Output {
        self.command("flush")
            .args(["--as-name", "sender", "--max-messages", &limit.to_string()])
            .output()
            .unwrap()
    }

    fn make_retries_due(&self) {
        self.store()
            .execute("UPDATE outbound_retries SET next_attempt_ms=0", [])
            .unwrap();
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.host).unwrap();
    }
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn request(stream: &mut TcpStream) -> (String, Value) {
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 4096];
    let split = loop {
        let read = stream.read(&mut buffer).unwrap();
        assert!(read > 0, "request closed before HTTP headers");
        bytes.extend_from_slice(&buffer[..read]);
        assert!(bytes.len() <= 1_200_000, "request exceeded fixture bound");
        if let Some(split) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break split + 4;
        }
    };
    let head = String::from_utf8(bytes[..split].to_vec()).unwrap();
    let length: usize = head
        .lines()
        .find_map(|line| {
            line.split_once(':')
                .filter(|(key, _)| key.eq_ignore_ascii_case("content-length"))
                .map(|(_, value)| value.trim().parse().unwrap())
        })
        .unwrap_or(0);
    while bytes.len() < split + length {
        let read = stream.read(&mut buffer).unwrap();
        assert!(read > 0, "request closed before body");
        bytes.extend_from_slice(&buffer[..read]);
    }
    let body = if length == 0 {
        Value::Null
    } else {
        serde_json::from_slice(&bytes[split..split + length]).unwrap()
    };
    (head, body)
}

fn reply(stream: &mut TcpStream, status: u16, body: &Value) {
    let data = body.to_string();
    let head = format!("HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", data.len());
    stream.write_all(head.as_bytes()).unwrap();
    stream.write_all(data.as_bytes()).unwrap();
}

fn accept_bounded(listener: &TcpListener) -> TcpStream {
    listener.set_nonblocking(true).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match listener.accept() {
            Ok((stream, _)) => return stream,
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline =>
            {
                thread::sleep(Duration::from_millis(5));
            }
            Err(error) => panic!("fixture peer did not receive expected request: {error}"),
        }
    }
}

fn peer(
    port: u16,
    calls: usize,
    skills: Value,
    bad_ack: bool,
) -> (thread::JoinHandle<()>, Arc<Mutex<Vec<Value>>>) {
    let listener = TcpListener::bind(("127.0.0.1", port)).unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let copied = Arc::clone(&seen);
    let handle = thread::spawn(move || {
        for _ in 0..calls {
            let mut stream = accept_bounded(&listener);
            let (head, body) = request(&mut stream);
            if head.starts_with("GET /.well-known/agent-card.json ") {
                reply(
                    &mut stream,
                    200,
                    &json!({"name":"recipient","skills":skills}),
                );
            } else {
                assert!(head.starts_with("POST / "));
                let headers = head.to_ascii_lowercase();
                assert!(headers.contains("x-a2a-token: synthetic-recipient-token"));
                assert!(headers.contains("a2a-version: 1.0"));
                assert_eq!(body["method"], "SendMessage");
                let id = body["params"]["message"]["messageId"].as_str().unwrap();
                let ack = if bad_ack { "wrong-id" } else { id };
                reply(
                    &mut stream,
                    200,
                    &json!({"jsonrpc":"2.0","result":{"message":{"parts":[{"data":{"kind":"delivery-receipt","message_id":ack,"recipient":"recipient"}}]}}}),
                );
                copied.lock().unwrap().push(body);
            }
        }
    });
    (handle, seen)
}

#[test]
fn offline_send_is_durable_and_flush_retries_same_ids_in_fifo_order() {
    let _endpoint = ENDPOINT_LIFECYCLE.lock().unwrap();
    let port = free_port();
    let fixture = Fixture::new(port);
    let first = fixture.send("first private body");
    let second = fixture.send("second private body");
    assert_eq!(
        first.status.code(),
        Some(3),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    assert_eq!(
        second.status.code(),
        Some(3),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    let first: Value = serde_json::from_slice(&first.stdout).unwrap();
    let second: Value = serde_json::from_slice(&second.stdout).unwrap();
    assert_eq!(first["delivery_status"], "queued");
    assert_eq!(second["delivery_status"], "queued");
    assert!(first.get("body").is_none());
    fixture.make_retries_due();
    // One cached discovery plus two ordered sends, rather than two discoveries.
    let (server, seen) = peer(port, 3, json!([{"id":"coordination-v2"}]), false);
    let output = fixture.flush(2);
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    server.join().unwrap();
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result.as_array().unwrap().len(), 2);
    assert_eq!(result[0]["message_id"], first["message_id"]);
    assert_eq!(result[1]["message_id"], second["message_id"]);
    assert_eq!(result[0]["status"], "delivered");
    let seen = seen.lock().unwrap();
    assert_eq!(
        seen[0]["params"]["message"]["messageId"],
        first["message_id"]
    );
    assert_eq!(
        seen[1]["params"]["message"]["messageId"],
        second["message_id"]
    );
}

#[test]
fn invalid_ack_is_terminal_and_no_queue_policy_closes_offline_send() {
    let _endpoint = ENDPOINT_LIFECYCLE.lock().unwrap();
    let port = free_port();
    let fixture = Fixture::new(port);
    let (server, _) = peer(port, 2, json!([]), true);
    let rejected = fixture.send("reject me");
    assert_eq!(rejected.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("did not echo"));
    server.join().unwrap();
    let failed: i64 = fixture
        .store()
        .query_row(
            "SELECT count(*) FROM messages WHERE delivery_status='failed'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(failed, 1);
    let no_queue = fixture
        .command("send")
        .args([
            "--from-name",
            "sender",
            "--to",
            "recipient",
            "--body",
            "offline",
            "--no-queue",
        ])
        .output()
        .unwrap();
    assert_eq!(no_queue.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&no_queue.stderr).contains("queue disabled"));
    let retryable: i64 = fixture
        .store()
        .query_row(
            "SELECT count(*) FROM messages WHERE delivery_status IN ('pending','queued')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(retryable, 0);
}

#[test]
fn malformed_peer_json_is_terminal_and_records_failure() {
    let _endpoint = ENDPOINT_LIFECYCLE.lock().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let fixture = Fixture::new(port);
    let server = thread::spawn(move || {
        let mut card_stream = accept_bounded(&listener);
        let (head, _) = request(&mut card_stream);
        assert!(head.starts_with("GET /.well-known/agent-card.json "));
        reply(
            &mut card_stream,
            200,
            &json!({"name":"recipient","skills":[]}),
        );
        let mut send_stream = accept_bounded(&listener);
        let (head, _) = request(&mut send_stream);
        assert!(head.starts_with("POST / "));
        let body = b"not-json";
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        send_stream.write_all(response.as_bytes()).unwrap();
        send_stream.write_all(body).unwrap();
    });
    let output = fixture.send("malformed response");
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("invalid peer JSON"));
    server.join().unwrap();
    let (status, reason): (String, String) = fixture
        .store()
        .query_row(
            "SELECT delivery_status,status_reason FROM messages",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(status, "failed");
    assert!(reason.contains("invalid peer JSON"));
}

#[test]
fn crash_left_pending_retries_and_structured_preflight_refuses_unsupported_peer() {
    let _endpoint = ENDPOINT_LIFECYCLE.lock().unwrap();
    let port = free_port();
    let fixture = Fixture::new(port);
    fixture.store().execute(
        "INSERT INTO messages(message_id,direction,sender,recipient,body,created_at,delivery_status) VALUES ('crash-left','outbound','sender','recipient','pending body','2026-09-27T00:00:00+00:00','pending')",
        [],
    ).unwrap();
    let (server, seen) = peer(port, 2, json!([]), false);
    let result = fixture.flush(1);
    assert_eq!(
        result.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    server.join().unwrap();
    assert_eq!(
        seen.lock().unwrap()[0]["params"]["message"]["messageId"],
        "crash-left"
    );
    let data = fixture.host.join("structured.json");
    fs::write(&data, json!({"kind":"coordination-v2","thread_id":"thread-1","summary":"status","status":"open","requires_response":true,"supersedes":[]}).to_string()).unwrap();
    let (server, seen) = peer(port, 1, json!([{"id":"coordination"}]), false);
    let output = fixture
        .command("send")
        .args([
            "--from-name",
            "sender",
            "--to",
            "recipient",
            "--body",
            "body",
            "--data-file",
        ])
        .arg(data)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("does not advertise coordination-v2"));
    server.join().unwrap();
    assert!(seen.lock().unwrap().is_empty());
    let stored: i64 = fixture
        .store()
        .query_row("SELECT count(*) FROM messages", [], |row| row.get(0))
        .unwrap();
    assert_eq!(stored, 1);
}

#[test]
fn structured_offline_message_rechecks_skill_and_records_terminal_failure() {
    let _endpoint = ENDPOINT_LIFECYCLE.lock().unwrap();
    let port = free_port();
    let fixture = Fixture::new(port);
    let data = fixture.host.join("structured.json");
    fs::write(&data, json!({"kind":"coordination-v2","thread_id":"thread-1","summary":"status","status":"open","requires_response":true,"supersedes":[]}).to_string()).unwrap();
    let queued = fixture
        .command("send")
        .args([
            "--from-name",
            "sender",
            "--to",
            "recipient",
            "--body",
            "private",
            "--data-file",
        ])
        .arg(&data)
        .output()
        .unwrap();
    assert_eq!(
        queued.status.code(),
        Some(3),
        "{}",
        String::from_utf8_lossy(&queued.stderr)
    );
    let queued: Value = serde_json::from_slice(&queued.stdout).unwrap();
    fixture.make_retries_due();
    let (server, seen) = peer(port, 1, json!([{"id":"coordination"}]), false);
    let flushed = fixture.flush(1);
    assert_eq!(flushed.status.code(), Some(0));
    server.join().unwrap();
    assert!(seen.lock().unwrap().is_empty());
    let rows: Value = serde_json::from_slice(&flushed.stdout).unwrap();
    assert_eq!(rows[0]["status"], "failed");
    assert_eq!(rows[0]["message_id"], queued["message_id"]);
    let (status, reason): (String, String) = fixture
        .store()
        .query_row(
            "SELECT delivery_status,status_reason FROM messages WHERE message_id=?1",
            [queued["message_id"].as_str().unwrap()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(status, "failed");
    assert!(reason.contains("coordination-v2"));
}

#[test]
fn structured_send_preflights_once_and_stdin_body_reaches_authenticated_wire() {
    let _endpoint = ENDPOINT_LIFECYCLE.lock().unwrap();
    let port = free_port();
    let fixture = Fixture::new(port);
    let data = fixture.host.join("structured.json");
    fs::write(&data, json!({"kind":"coordination-v2","thread_id":"thread-2","summary":"bounded summary","status":"open","requires_response":true,"supersedes":[]}).to_string()).unwrap();
    let (server, seen) = peer(port, 2, json!([{"id":"coordination-v2"}]), false);
    let mut child = fixture
        .command("send")
        .args([
            "--from-name",
            "sender",
            "--to",
            "recipient",
            "--stdin",
            "--data-file",
        ])
        .arg(data)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all("private 🦀 body".as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let receipt: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(receipt["delivery_status"], "delivered");
    assert!(!String::from_utf8_lossy(&output.stdout).contains("private 🦀 body"));
    server.join().unwrap();
    let wire = seen.lock().unwrap();
    assert_eq!(wire.len(), 1);
    assert_eq!(
        wire[0]["params"]["message"]["parts"][0]["text"],
        "private 🦀 body"
    );
    assert_eq!(
        wire[0]["params"]["message"]["parts"][1]["data"]["thread_id"],
        "thread-2"
    );
    let status: String = fixture
        .store()
        .query_row("SELECT delivery_status FROM messages", [], |row| row.get(0))
        .unwrap();
    assert_eq!(status, "delivered");
}

#[test]
fn flush_limit_reports_remaining_and_missing_recipient_fails_old_queued_message() {
    let _endpoint = ENDPOINT_LIFECYCLE.lock().unwrap();
    let port = free_port();
    let fixture = Fixture::new(port);
    for body in ["one", "two"] {
        assert_eq!(fixture.send(body).status.code(), Some(3));
    }
    fixture.make_retries_due();
    let (server, _) = peer(port, 2, json!([]), false);
    let first = fixture.flush(1);
    assert_eq!(first.status.code(), Some(3));
    let rows: Value = serde_json::from_slice(&first.stdout).unwrap();
    assert_eq!(rows.as_array().unwrap().len(), 1);
    assert_eq!(rows[0]["status"], "delivered");
    server.join().unwrap();
    fs::write(
        fixture.root().join("agents.json"),
        json!({"schema_version":1,"agents":{
            "sender":{"port":7398,"token":"synthetic-sender-token"}
        }})
        .to_string(),
    )
    .unwrap();
    let second = fixture.flush(1);
    assert_eq!(second.status.code(), Some(0));
    let rows: Value = serde_json::from_slice(&second.stdout).unwrap();
    assert_eq!(rows[0]["status"], "failed");
    let reason: String = fixture
        .store()
        .query_row(
            "SELECT status_reason FROM messages WHERE delivery_status='failed'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(reason.contains("recipient no longer registered"));
}

#[test]
fn structured_offline_message_rechecks_capability_and_ack_on_retry() {
    let _endpoint = ENDPOINT_LIFECYCLE.lock().unwrap();
    let port = free_port();
    let fixture = Fixture::new(port);
    let data = fixture.host.join("structured.json");
    fs::write(&data, json!({"kind":"coordination-v2","thread_id":"thread-retry","summary":"retry summary","status":"open","requires_response":true,"supersedes":[]}).to_string()).unwrap();
    let queued = fixture
        .command("send")
        .args([
            "--from-name",
            "sender",
            "--to",
            "recipient",
            "--body",
            "private retry body",
            "--data-file",
        ])
        .arg(data)
        .output()
        .unwrap();
    assert_eq!(
        queued.status.code(),
        Some(3),
        "{}",
        String::from_utf8_lossy(&queued.stderr)
    );
    let receipt: Value = serde_json::from_slice(&queued.stdout).unwrap();
    let id = receipt["message_id"].as_str().unwrap();
    assert!(receipt.get("body").is_none());
    assert!(receipt.get("data_json").is_none());
    let stored_data: String = fixture
        .store()
        .query_row(
            "SELECT data_json FROM messages WHERE message_id=?1",
            [id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&stored_data).unwrap()["thread_id"],
        "thread-retry"
    );

    fixture.make_retries_due();
    let (server, seen) = peer(port, 2, json!([{"id":"coordination-v2"}]), false);
    let flushed = fixture.flush(1);
    assert_eq!(
        flushed.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&flushed.stderr)
    );
    server.join().unwrap();
    let rows: Value = serde_json::from_slice(&flushed.stdout).unwrap();
    assert_eq!(rows[0]["message_id"], id);
    assert_eq!(rows[0]["status"], "delivered");
    let posted = seen.lock().unwrap();
    assert_eq!(posted.len(), 1);
    assert_eq!(posted[0]["params"]["message"]["messageId"], id);
    assert_eq!(
        posted[0]["params"]["message"]["parts"][1]["data"]["thread_id"],
        "thread-retry"
    );
    let history = fixture
        .command("history")
        .args(["--as-name", "sender", "--message-id", id, "--limit", "1"])
        .output()
        .unwrap();
    assert_eq!(history.status.code(), Some(0));
    let events: Value = serde_json::from_slice(&history.stdout).unwrap();
    assert_eq!(events["available"], true);
    assert_eq!(events["events"][0]["status"], "delivered");
    assert!(events["events"][0].get("body").is_none());
}

#[test]
fn sender_lock_rejects_concurrent_flush_without_consuming_queue() {
    let _endpoint = ENDPOINT_LIFECYCLE.lock().unwrap();
    let fixture = Fixture::new(free_port());
    let queued = fixture.send("serialized send");
    assert_eq!(queued.status.code(), Some(3));
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(fixture.root().join("sender/.delivery.lock"))
        .unwrap();
    // SAFETY: the descriptor remains open until the explicit unlock below.
    assert_eq!(
        unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
    let refused = fixture.flush(1);
    assert_eq!(refused.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&refused.stderr).contains("active delivery"));
    let pending: i64 = fixture
        .store()
        .query_row(
            "SELECT count(*) FROM messages WHERE delivery_status='queued'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(pending, 1);
    // SAFETY: unlock the same descriptor before the fixture is removed.
    assert_eq!(unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_UN) }, 0);
}

#[test]
fn history_of_uninitialized_identity_does_not_create_a_store() {
    let _endpoint = ENDPOINT_LIFECYCLE.lock().unwrap();
    let fixture = Fixture::new(free_port());
    let store_path = fixture.root().join("sender/store.sqlite");
    fs::remove_file(&store_path).unwrap();
    let output = fixture
        .command("history")
        .args(["--as-name", "sender"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    let payload: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        payload,
        json!({"schema_version":1,"available":false,"events":[]})
    );
    assert!(!store_path.exists());
}

#[test]
fn new_send_flushes_older_backlog_before_its_own_delivery() {
    let _endpoint = ENDPOINT_LIFECYCLE.lock().unwrap();
    let port = free_port();
    let fixture = Fixture::new(port);
    let old = fixture.send("older body");
    assert_eq!(old.status.code(), Some(3));
    let old: Value = serde_json::from_slice(&old.stdout).unwrap();
    fixture.make_retries_due();
    let (server, seen) = peer(port, 3, json!([]), false);
    let fresh = fixture.send("fresh body");
    assert_eq!(
        fresh.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&fresh.stderr)
    );
    server.join().unwrap();
    let fresh: Value = serde_json::from_slice(&fresh.stdout).unwrap();
    assert_eq!(fresh["delivery_status"], "delivered");
    let posted = seen.lock().unwrap();
    assert_eq!(posted.len(), 2);
    assert_eq!(
        posted[0]["params"]["message"]["messageId"],
        old["message_id"]
    );
    assert_eq!(
        posted[1]["params"]["message"]["messageId"],
        fresh["message_id"]
    );
    let failed_or_pending: i64 = fixture
        .store()
        .query_row(
            "SELECT count(*) FROM messages WHERE delivery_status!='delivered'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(failed_or_pending, 0);
}

#[test]
fn legacy_send_accepts_card_without_v2_skill() {
    let _endpoint = ENDPOINT_LIFECYCLE.lock().unwrap();
    let port = free_port();
    let fixture = Fixture::new(port);
    let data = fixture.host.join("legacy.json");
    fs::write(&data, json!({"kind":"coordination"}).to_string()).unwrap();
    let (server, seen) = peer(port, 2, json!([{"id":"coordination"}]), false);
    let result = fixture
        .command("send")
        .args([
            "--from-name",
            "sender",
            "--to",
            "recipient",
            "--body",
            "legacy",
            "--data-file",
        ])
        .arg(data)
        .output()
        .unwrap();
    assert_eq!(
        result.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    server.join().unwrap();
    let posted = seen.lock().unwrap();
    assert_eq!(posted.len(), 1);
    assert_eq!(
        posted[0]["params"]["message"]["parts"][1]["data"]["kind"],
        "coordination"
    );
}

#[test]
fn transient_busy_response_retries_the_same_id_only_when_due() {
    let _endpoint = ENDPOINT_LIFECYCLE.lock().unwrap();
    let port = free_port();
    let fixture = Fixture::new(port);
    let listener = TcpListener::bind(("127.0.0.1", port)).unwrap();
    let server = thread::spawn(move || {
        let mut ids = Vec::new();
        for attempt in 0..2 {
            let mut discovery = accept_bounded(&listener);
            assert!(request(&mut discovery).0.starts_with("GET "));
            reply(
                &mut discovery,
                200,
                &json!({"name":"recipient","skills":[]}),
            );
            let mut stream = accept_bounded(&listener);
            let (_, body) = request(&mut stream);
            let id = body["params"]["message"]["messageId"].as_str().unwrap();
            ids.push(id.to_owned());
            if attempt == 0 {
                // HTML/empty error bodies must not turn an HTTP retry into terminal JSON rejection.
                stream.write_all(b"HTTP/1.1 503 Busy\r\nRetry-After: 2\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
            } else {
                reply(
                    &mut stream,
                    200,
                    &json!({"result":{"message":{"parts":[{"data":{"kind":"delivery-receipt","message_id":id}}]}}}),
                );
            }
        }
        assert_eq!(ids[0], ids[1]);
    });
    let before = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    let first = fixture.send("busy but recoverable");
    assert_eq!(
        first.status.code(),
        Some(3),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let receipt: Value = serde_json::from_slice(&first.stdout).unwrap();
    let retry: (u64, i64) = fixture
        .store()
        .query_row(
            "SELECT attempts,next_attempt_ms FROM outbound_retries",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(retry.0, 1);
    assert!(retry.1 >= before + 2000, "Retry-After was not honored");
    let early = fixture.flush(1);
    assert_eq!(early.status.code(), Some(3));
    assert_eq!(
        serde_json::from_slice::<Value>(&early.stdout).unwrap(),
        json!([])
    );
    fixture.make_retries_due();
    let recovered = fixture.flush(1);
    assert_eq!(recovered.status.code(), Some(0));
    assert_eq!(
        serde_json::from_slice::<Value>(&recovered.stdout).unwrap()[0]["message_id"],
        receipt["message_id"]
    );
    server.join().unwrap();
}

#[test]
fn logical_idempotency_reuses_receipt_and_rejects_payload_collision() {
    let _endpoint = ENDPOINT_LIFECYCLE.lock().unwrap();
    let port = free_port();
    let fixture = Fixture::new(port);
    let (server, seen) = peer(port, 2, json!([]), false);
    let send = |body: &str| {
        fixture
            .command("send")
            .args([
                "--from-name",
                "sender",
                "--to",
                "recipient",
                "--body",
                body,
                "--idempotency-key",
                "logical-operation",
            ])
            .output()
            .unwrap()
    };
    let first = send("one operation");
    assert!(first.status.success());
    server.join().unwrap();
    let second = send("one operation");
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&first.stdout).unwrap()["message_id"],
        serde_json::from_slice::<Value>(&second.stdout).unwrap()["message_id"]
    );
    let conflicting = send("changed operation");
    assert!(!conflicting.status.success());
    assert!(String::from_utf8_lossy(&conflicting.stderr).contains("conflicting idempotency key"));
    assert_eq!(seen.lock().unwrap().len(), 1);
    assert_eq!(
        fixture
            .store()
            .query_row("SELECT count(*) FROM messages", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn logical_key_accepts_equivalent_json_and_rejects_changed_data() {
    let _endpoint = ENDPOINT_LIFECYCLE.lock().unwrap();
    let fixture = Fixture::new(free_port());
    let data = fixture.host.join("coordination.json");
    let enqueue = || {
        fixture
            .command("enqueue")
            .args([
                "--from-name",
                "sender",
                "--to",
                "recipient",
                "--body",
                "same operation",
                "--idempotency-key",
                "structured-key",
                "--data-file",
            ])
            .arg(&data)
            .output()
            .unwrap()
    };
    fs::write(&data,r#"{"kind":"coordination-v2","thread_id":"thread-1","summary":"progress","status":"informational","requires_response":false}"#).unwrap();
    let first = enqueue();
    assert!(first.status.success());
    fs::write(&data,r#"{ "requires_response": false, "status": "informational", "summary": "progress", "thread_id": "thread-1", "kind": "coordination-v2" }"#).unwrap();
    let equivalent = enqueue();
    assert!(
        equivalent.status.success(),
        "{}",
        String::from_utf8_lossy(&equivalent.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&first.stdout).unwrap()["message_id"],
        serde_json::from_slice::<Value>(&equivalent.stdout).unwrap()["message_id"]
    );
    fs::write(&data,r#"{"kind":"coordination-v2","thread_id":"thread-1","summary":"changed progress","status":"informational","requires_response":false}"#).unwrap();
    let changed = enqueue();
    assert!(!changed.status.success());
    assert!(String::from_utf8_lossy(&changed.stderr).contains("conflicting idempotency key"));
}

#[test]
fn recipient_batches_are_parallel_ordered_and_discover_each_peer_once() {
    let _endpoint = ENDPOINT_LIFECYCLE.lock().unwrap();
    let a = TcpListener::bind("127.0.0.1:0").unwrap();
    let b = TcpListener::bind("127.0.0.1:0").unwrap();
    let fixture = Fixture::new(a.local_addr().unwrap().port());
    let mut registry: Value =
        serde_json::from_slice(&fs::read(fixture.root().join("agents.json")).unwrap()).unwrap();
    registry["agents"]["other"] =
        json!({"port":b.local_addr().unwrap().port(),"token":"synthetic-recipient-token"});
    fs::write(fixture.root().join("agents.json"), registry.to_string()).unwrap();
    for (recipient, body) in [
        ("recipient", "a1"),
        ("recipient", "a2"),
        ("other", "b1"),
        ("other", "b2"),
    ] {
        let output = fixture
            .command("enqueue")
            .args(["--from-name", "sender", "--to", recipient, "--body", body])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let concurrent = Arc::new(AtomicUsize::new(0));
    let peers = [(a,"recipient",vec!["a1","a2"]),(b,"other",vec!["b1","b2"])].into_iter().map(|(listener,name,expected)| {
        let concurrent = Arc::clone(&concurrent);
        thread::spawn(move || {
            let mut stream = accept_bounded(&listener);
            assert!(request(&mut stream).0.starts_with("GET "));
            reply(&mut stream,200,&json!({"name":name,"skills":[]}));
            for (index,expected) in expected.iter().enumerate() {
                let mut stream = accept_bounded(&listener);
                let (head,body) = request(&mut stream);
                assert!(head.starts_with("POST "),"peer card was fetched twice");
                assert_eq!(body["params"]["message"]["parts"][0]["text"],*expected);
                if index == 0 {
                    concurrent.fetch_add(1,Ordering::SeqCst);
                    let deadline = Instant::now()+Duration::from_secs(2);
                    while concurrent.load(Ordering::SeqCst)<2 && Instant::now()<deadline { thread::sleep(Duration::from_millis(5)); }
                    assert_eq!(concurrent.load(Ordering::SeqCst),2,"recipient delivery was serialized");
                }
                reply(&mut stream,200,&json!({"result":{"message":{"parts":[{"data":{"kind":"delivery-receipt","message_id":body["params"]["message"]["messageId"]}}]}}}));
            }
        })
    }).collect::<Vec<_>>();
    let flushed = fixture.flush(4);
    assert!(
        flushed.status.success(),
        "{}",
        String::from_utf8_lossy(&flushed.stderr)
    );
    for peer in peers {
        peer.join().unwrap();
    }
    assert_eq!(
        serde_json::from_slice::<Value>(&flushed.stdout)
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        4
    );
}
