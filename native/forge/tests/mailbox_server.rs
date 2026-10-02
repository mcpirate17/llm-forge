//! Native loopback A2A server, registration and inbound SQLite contract.

use rusqlite::Connection;
use serde_json::{json, Value};
use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

struct Fixture {
    state: PathBuf,
    child: Option<Child>,
    port: u16,
}

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let state = std::env::temp_dir().join(format!(
            "forge-server-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&state).unwrap();
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        Self {
            state,
            child: None,
            port,
        }
    }

    fn command(&self, action: &str) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_forge"));
        command
            .args(["mailbox", "--state-dir"])
            .arg(&self.state)
            .arg(action)
            .env("PATH", "")
            .env("CONDUCTOR_PYTHON", "/bin/false");
        command
    }

    fn init(&self, name: &str) -> Output {
        self.command("init")
            .args(["--name", name, "--port", &self.port.to_string()])
            .output()
            .unwrap()
    }

    fn start(&mut self, name: &str) {
        let previous_generation = fs::read(self.state.join("agents.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .and_then(|registry| {
                registry["agents"][name]["generation"]
                    .as_str()
                    .map(str::to_owned)
            });
        self.child = Some(
            self.command("serve")
                .args(["--as-name", name, "--port", &self.port.to_string()])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if let Some(status) = self.child.as_mut().unwrap().try_wait().unwrap() {
                panic!("server exited early: {status}");
            }
            if self.registration_is_fresh(name, previous_generation.as_deref())
                && self.serves_card(name)
            {
                return;
            }
            thread::sleep(Duration::from_millis(20));
        }
        panic!("server did not register and serve its card in 5 seconds");
    }

    fn registration_is_fresh(&self, name: &str, previous_generation: Option<&str>) -> bool {
        let Ok(bytes) = fs::read(self.state.join("agents.json")) else {
            return false;
        };
        let Ok(registry) = serde_json::from_slice::<Value>(&bytes) else {
            return false;
        };
        let record = &registry["agents"][name];
        record["port"].as_u64() == Some(u64::from(self.port))
            && record["generation"]
                .as_str()
                .is_some_and(|generation| Some(generation) != previous_generation)
    }

    fn serves_card(&self, name: &str) -> bool {
        let address = SocketAddr::from(([127, 0, 0, 1], self.port));
        let Ok(mut stream) = TcpStream::connect_timeout(&address, Duration::from_millis(100))
        else {
            return false;
        };
        if stream
            .set_read_timeout(Some(Duration::from_millis(100)))
            .is_err()
            || stream
                .write_all(b"GET /.well-known/agent-card.json HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
                .is_err()
        {
            return false;
        }
        let mut response = Vec::new();
        if stream.read_to_end(&mut response).is_err() {
            return false;
        }
        let Some(split) = response.windows(4).position(|part| part == b"\r\n\r\n") else {
            return false;
        };
        response.starts_with(b"HTTP/1.1 200 ")
            && serde_json::from_slice::<Value>(&response[split + 4..])
                .is_ok_and(|card| card["name"] == name)
    }

    fn registry(&self) -> Value {
        serde_json::from_slice(&fs::read(self.state.join("agents.json")).unwrap()).unwrap()
    }

    fn token(&self, name: &str) -> String {
        self.registry()["agents"][name]["token"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    fn db(&self, name: &str) -> Connection {
        Connection::open(self.state.join(name).join("store.sqlite")).unwrap()
    }

    fn rpc(&self, token: Option<&str>, version: Option<&str>, body: &Value) -> (u16, Value) {
        let mut headers = String::new();
        if let Some(token) = token {
            headers.push_str(&format!("X-A2A-Token: {token}\r\n"));
        }
        if let Some(version) = version {
            headers.push_str(&format!("A2A-Version: {version}\r\n"));
        }
        let source = body.to_string();
        self.http("POST", "/", &headers, Some(&source))
    }

    fn http(&self, method: &str, path: &str, headers: &str, body: Option<&str>) -> (u16, Value) {
        let mut stream = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let body = body.unwrap_or("");
        write!(stream,"{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{headers}\r\n{body}",body.len()).unwrap();
        stream.flush().unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).unwrap();
        let split = response
            .windows(4)
            .position(|part| part == b"\r\n\r\n")
            .unwrap();
        let head = std::str::from_utf8(&response[..split]).unwrap();
        let status = head.split_whitespace().nth(1).unwrap().parse().unwrap();
        let value = serde_json::from_slice(&response[split + 4..]).unwrap();
        (status, value)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(child) = &mut self.child {
            let _ = child.kill();
            let _ = child.wait();
        }
        fs::remove_dir_all(&self.state).unwrap();
    }
}

fn message(id: &str, sender: &str, body: &str, data: Option<Value>) -> Value {
    let mut parts = vec![json!({"text":body})];
    if let Some(data) = data {
        parts.push(json!({"data":data}));
    }
    json!({"jsonrpc":"2.0","id":"request-1","method":"SendMessage",
        "params":{"message":{"messageId":id,"role":"ROLE_USER","parts":parts,
            "metadata":{"sender":sender}}}})
}

fn coordination(thread: &str, status: &str, supersedes: Vec<&str>) -> Value {
    json!({"kind":"coordination-v2","thread_id":thread,"summary":"Bounded status",
        "status":status,"requires_response":true,"supersedes":supersedes})
}

#[test]
fn card_auth_version_method_and_deduplicated_inbound_receipt() {
    let mut fixture = Fixture::new();
    fixture.start("recipient");
    let (status, card) = fixture.http("GET", "/.well-known/agent-card.json", "", None);
    assert_eq!(status, 200);
    assert_eq!(card["name"], "recipient");
    assert_eq!(card["supportedInterfaces"][0]["protocolBinding"], "JSONRPC");
    assert_eq!(card["supportedInterfaces"][0]["protocolVersion"], "1.0");
    assert!(card["skills"]
        .as_array()
        .unwrap()
        .iter()
        .any(|skill| skill["id"] == "coordination-v2"));
    let token = fixture.token("recipient");
    let request = message("m-1", "sender", "hello", None);
    assert_eq!(fixture.rpc(None, Some("1.0"), &request).0, 401);
    assert_eq!(fixture.rpc(Some("wrong"), Some("1.0"), &request).0, 401);
    assert!(fixture
        .rpc(Some(&token), None, &request)
        .1
        .get("error")
        .is_some());
    let mut wrong_method = request.clone();
    wrong_method["method"] = json!("message/send");
    assert_eq!(
        fixture.rpc(Some(&token), Some("1.0"), &wrong_method).1["error"]["code"],
        -32601
    );
    for _ in 0..2 {
        let (status, reply) = fixture.rpc(Some(&token), Some("1.0"), &request);
        assert_eq!(status, 200);
        assert_eq!(
            reply["result"]["message"]["parts"][0]["data"],
            json!({"kind":"delivery-receipt","message_id":"m-1","recipient":"recipient"})
        );
    }
    let db = fixture.db("recipient");
    let count: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM messages WHERE direction='inbound' AND message_id='m-1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
    let conflict = message("m-1", "sender", "different body", None);
    assert!(
        fixture.rpc(Some(&token), Some("1.0"), &conflict).1["error"]["message"]
            .as_str()
            .unwrap()
            .contains("conflicting duplicate")
    );
    let conflict = message("m-1", "other", "hello", None);
    assert!(fixture
        .rpc(Some(&token), Some("1.0"), &conflict)
        .1
        .get("error")
        .is_some());
}

#[test]
fn invalid_data_is_rejected_and_supersession_is_atomic() {
    let mut fixture = Fixture::new();
    fixture.start("recipient");
    let token = fixture.token("recipient");
    let invalid = message(
        "bad",
        "sender",
        "gate",
        Some(json!({"kind":"gate-review-request","gate":"three"})),
    );
    assert!(fixture
        .rpc(Some(&token), Some("1.0"), &invalid)
        .1
        .get("error")
        .is_some());
    let first = message(
        "first",
        "sender",
        "initial",
        Some(coordination("thread", "open", vec![])),
    );
    assert_eq!(fixture.rpc(Some(&token), Some("1.0"), &first).0, 200);
    let wrong = message(
        "wrong",
        "other",
        "different",
        Some(coordination("thread", "resolved", vec!["first"])),
    );
    assert!(fixture
        .rpc(Some(&token), Some("1.0"), &wrong)
        .1
        .get("error")
        .is_some());
    let cross = message(
        "cross",
        "sender",
        "different thread",
        Some(coordination("other-thread", "resolved", vec!["first"])),
    );
    assert!(fixture
        .rpc(Some(&token), Some("1.0"), &cross)
        .1
        .get("error")
        .is_some());
    let second = message(
        "second",
        "sender",
        "resolved",
        Some(coordination("thread", "resolved", vec!["first"])),
    );
    assert_eq!(fixture.rpc(Some(&token), Some("1.0"), &second).0, 200);
    let db = fixture.db("recipient");
    let count: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM messages WHERE direction='inbound'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 2);
    let status:String=db.query_row("SELECT protocol_status FROM message_state WHERE direction='inbound' AND message_id='first'",[],|r|r.get(0)).unwrap();
    assert_eq!(status, "superseded");
}

#[test]
fn bind_failure_preserves_registration_generation_and_permissions() {
    let fixture = Fixture::new();
    let init = fixture.init("busy");
    assert!(
        init.status.success(),
        "{}",
        String::from_utf8_lossy(&init.stderr)
    );
    let before = fs::read(fixture.state.join("agents.json")).unwrap();
    assert_eq!(
        fs::metadata(fixture.state.join("agents.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o077,
        0
    );
    let occupied = TcpListener::bind(("127.0.0.1", fixture.port)).unwrap();
    let failed = fixture
        .command("serve")
        .args(["--as-name", "busy"])
        .output()
        .unwrap();
    assert!(!failed.status.success());
    assert!(String::from_utf8_lossy(&failed.stderr).contains("cannot bind"));
    assert_eq!(fs::read(fixture.state.join("agents.json")).unwrap(), before);
    drop(occupied);
}

#[test]
fn registry_reap_requires_failure_streak_and_keeps_mailbox() {
    let fixture = Fixture::new();
    assert!(fixture.init("down").status.success());
    let one = fixture
        .command("reap")
        .args(["--consecutive-failures", "2"])
        .output()
        .unwrap();
    assert!(
        one.status.success(),
        "{}",
        String::from_utf8_lossy(&one.stderr)
    );
    assert!(fixture.registry()["agents"].get("down").is_some());
    let two = fixture
        .command("reap")
        .args(["--consecutive-failures", "2"])
        .output()
        .unwrap();
    assert!(
        two.status.success(),
        "{}",
        String::from_utf8_lossy(&two.stderr)
    );
    assert!(fixture.registry()["agents"].get("down").is_none());
    let result: Value = serde_json::from_slice(&two.stdout).unwrap();
    assert_eq!(result["reaped"], json!(["down"]));
}

#[test]
fn serving_rotates_generation_and_stale_liveness_cannot_reap_it() {
    let mut fixture = Fixture::new();
    assert!(fixture.init("restarted").status.success());
    let original = fixture.registry();
    let old_generation = original["agents"]["restarted"]["generation"]
        .as_str()
        .unwrap();
    let mut hash = sha2::Sha256::new();
    use sha2::Digest;
    hash.update(format!("restarted\0{}\0{}", fixture.port, old_generation));
    let fingerprint = format!("{:x}", hash.finalize());
    fs::write(
        fixture.state.join("liveness.json"),
        json!({"schema_version":1,"agents":{
        "restarted":{"consecutive_failures":50,"last_probe_at":"2026-08-30T00:00:00+00:00",
            "registration_fingerprint":fingerprint}}})
        .to_string(),
    )
    .unwrap();
    fixture.start("restarted");
    let current = fixture.registry();
    assert_ne!(current["agents"]["restarted"]["generation"], old_generation);
    assert_eq!(
        current["agents"]["restarted"]["token"],
        original["agents"]["restarted"]["token"]
    );
    let reap = fixture
        .command("reap")
        .args(["--consecutive-failures", "1"])
        .output()
        .unwrap();
    assert!(
        reap.status.success(),
        "{}",
        String::from_utf8_lossy(&reap.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&reap.stdout).unwrap()["reaped"],
        json!([])
    );
    assert!(fixture.registry()["agents"].get("restarted").is_some());
}

#[test]
fn native_sender_self_send_keeps_two_directional_facts() {
    let mut fixture = Fixture::new();
    fixture.start("self");
    let output = fixture
        .command("send")
        .args([
            "--from-name",
            "self",
            "--to",
            "self",
            "--body",
            "local handoff",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let receipt: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(receipt["delivery_status"], "delivered");
    let id = receipt["message_id"].as_str().unwrap();
    let db = fixture.db("self");
    let count: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM messages WHERE message_id=?1",
            [id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 2);
}
