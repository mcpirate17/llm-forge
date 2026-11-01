//! Disposable native mailbox presentation fixtures. No peer or model is launched.
#![cfg(target_os = "linux")]
#[allow(dead_code)]
mod support;
use conductor_native::a2a_store::Store;
use forge::process_group::ProcessGroup;
use rusqlite::Connection;
use serde_json::Value;
use std::io::{BufRead, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

struct Fixture(support::Host);
impl Fixture {
    fn new() -> Self {
        let fixture = Self(support::Host::new());
        Store::initialize(&fixture.root(), "recipient").unwrap();
        fixture
    }
    fn root(&self) -> std::path::PathBuf {
        self.0 .0.join(".agents/a2a")
    }
    fn db(&self) -> Connection {
        Connection::open(self.root().join("recipient/store.sqlite")).unwrap()
    }
    fn put(&self, id: &str) {
        Store::required(&self.root(), "recipient", true)
            .unwrap()
            .record_inbound(
                id,
                "sender",
                "recipient",
                &"private multiline 🦀\n".repeat(3000),
                None,
                "2026-10-31T00:00:00.000+00:00",
            )
            .unwrap();
    }
    fn command(&self) -> Command {
        let mut command = self.0.command();
        command
            .env("PATH", "")
            .env("CONDUCTOR_PYTHON", "/bin/false")
            .args(["mailbox", "watch", "--as-name", "recipient", "--json"]);
        command
    }
    fn once(&self) -> Output {
        self.command().arg("--once").output().unwrap()
    }
    fn count(&self, table: &str) -> i64 {
        assert!(matches!(table, "message_presentations" | "messages"));
        self.db()
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    }
    fn wait_lease(&self) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while !self.root().join("recipient/.watch.lock").exists() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}
fn parsed(output: Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}
fn pidfd(pid: u32) -> OwnedFd {
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid as libc::pid_t, 0u32) };
    assert!(fd >= 0);
    unsafe { OwnedFd::from_raw_fd(fd as i32) }
}
fn poll(fd: &impl AsRawFd, timeout: i32) -> bool {
    let mut p = libc::pollfd {
        fd: fd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    assert!(unsafe { libc::poll(&mut p, 1, timeout) } >= 0);
    p.revents & (libc::POLLIN | libc::POLLHUP) != 0
}
fn terminate(fd: &OwnedFd) {
    assert_eq!(
        unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                fd.as_raw_fd(),
                libc::SIGTERM,
                std::ptr::null::<libc::siginfo_t>(),
                0u32,
            )
        },
        0
    );
}

#[test]
fn once_bounds_batches_marks_only_emitted_ids_and_keeps_read_acknowledgment_separate() {
    let fixture = Fixture::new();
    for id in ["a", "b", "c", "d", "e"] {
        fixture.put(id);
    }
    let first_output = fixture
        .command()
        .args([
            "--once",
            "--max-messages",
            "2",
            "--preview-chars",
            "32",
            "--max-chars",
            "700",
        ])
        .output()
        .unwrap();
    assert!(
        String::from_utf8(first_output.stdout.clone())
            .unwrap()
            .chars()
            .count()
            <= 701
    );
    let first = parsed(first_output);
    assert_eq!(first["total"], 5);
    assert_eq!(first["shown"], 2);
    assert_eq!(fixture.count("message_presentations"), 2);
    let second = parsed(fixture.once());
    assert_eq!(second["total"], 3);
    assert!(second["messages"]
        .as_array()
        .unwrap()
        .iter()
        .all(|row| first["messages"]
            .as_array()
            .unwrap()
            .iter()
            .all(|old| row["id"] != old["id"])));
    assert_eq!(fixture.count("message_presentations"), 5);
    assert!(fixture.once().stdout.is_empty());
    assert_eq!(
        fixture
            .db()
            .query_row(
                "SELECT count(*) FROM messages WHERE read_at IS NULL",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        5
    );
    let replay = parsed(
        fixture
            .command()
            .args(["--once", "--replay-unread"])
            .output()
            .unwrap(),
    );
    assert_eq!(replay["total"], 5);
    assert_eq!(fixture.count("messages"), 5);
}

#[test]
fn superseded_information_is_coalesced_while_actionable_requests_and_replay_survive() {
    let fixture = Fixture::new();
    for id in ["info", "request", "new"] {
        fixture.put(id);
    }
    fixture
        .db()
        .execute_batch(
            "UPDATE message_state SET superseded_at='2026-10-31T01:00:00Z'
        WHERE message_id IN ('info','request');
        UPDATE message_state SET requires_response=0 WHERE message_id='info';",
        )
        .unwrap();
    let selected = parsed(fixture.once());
    assert_eq!(selected["total"], 2);
    assert!(selected["messages"]
        .as_array()
        .unwrap()
        .iter()
        .all(|row| row["id"] != "info"));
    let replay = parsed(
        fixture
            .command()
            .args(["--once", "--replay-unread"])
            .output()
            .unwrap(),
    );
    assert_eq!(replay["total"], 3);
    assert_eq!(fixture.count("messages"), 3);
}

#[test]
fn notifications_wake_an_idle_stream_and_duplicate_consumers_are_rejected() {
    let fixture = Fixture::new();
    let mut group = ProcessGroup::spawn(
        fixture
            .command()
            .args(["--duration", "30"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )
    .unwrap();
    let child = pidfd(group.id());
    fixture.wait_lease();
    let duplicate = fixture.once();
    assert!(!duplicate.status.success());
    assert!(String::from_utf8_lossy(&duplicate.stderr).contains("busy"));
    let stdout = group.take_stdout().unwrap();
    assert!(!poll(&stdout, 100));
    fixture.put("new-arrival");
    assert!(poll(&stdout, 2000));
    let mut line = String::new();
    std::io::BufReader::new(stdout)
        .read_line(&mut line)
        .unwrap();
    let result: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(result["messages"][0]["id"], "new-arrival");
    terminate(&child);
    assert!(poll(&child, 2000));
    assert_eq!(group.stop().unwrap().code(), Some(130));
    assert_eq!(fixture.count("message_presentations"), 1);
    assert_eq!(
        fixture
            .db()
            .query_row(
                "SELECT count(*) FROM messages WHERE read_at IS NULL",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
}

#[test]
fn unread_replay_waits_instead_of_waking_on_its_own_acknowledgment() {
    let fixture = Fixture::new();
    fixture.put("one");
    let result = fixture
        .command()
        .args(["--duration", "1", "--replay-unread"])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(String::from_utf8(result.stdout).unwrap().lines().count(), 1);
    assert_eq!(fixture.count("message_presentations"), 1);
}

#[test]
fn output_backpressure_expires_without_marking_the_unwritten_batch() {
    let fixture = Fixture::new();
    fixture.put("one");
    let (_reader, mut writer) = std::os::unix::net::UnixStream::pair().unwrap();
    writer.set_nonblocking(true).unwrap();
    loop {
        match writer.write(&[0u8; 16384]) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(error) => panic!("fill fixture output: {error}"),
        }
    }
    let mut group = ProcessGroup::spawn(
        fixture
            .command()
            .args(["--once", "--duration", "1"])
            .stdout(Stdio::from(OwnedFd::from(writer)))
            .stderr(Stdio::piped()),
    )
    .unwrap();
    let child = pidfd(group.id());
    assert!(poll(&child, 3000));
    assert!(!group.stop().unwrap().success());
    let mut error = String::new();
    group
        .take_stderr()
        .unwrap()
        .read_to_string(&mut error)
        .unwrap();
    assert!(error.contains("backpressure deadline"), "{error}");
    assert_eq!(fixture.count("message_presentations"), 0);
    assert_eq!(parsed(fixture.once())["messages"][0]["id"], "one");
}

#[test]
fn output_to_a_regular_file_succeeds_and_original_descriptor_flags_are_restored() {
    use std::fs::File;
    let fixture = Fixture::new();
    fixture.put("one");
    let path = fixture.0 .0.join("presentation.json");
    let file = File::create(&path).unwrap();
    let before = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
    let output = fixture
        .command()
        .arg("--once")
        .stdout(Stdio::from(file.try_clone().unwrap()))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) },
        before
    );
    let value: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(value["shown"], 1);
    assert_eq!(fixture.count("message_presentations"), 1);
}

#[test]
fn invalid_bounds_and_insufficient_output_budget_consume_nothing() {
    let fixture = Fixture::new();
    fixture.put("one");
    for args in [
        ["--interval", "NaN"],
        ["--duration", "0"],
        ["--max-messages", "9"],
        ["--preview-chars", "0"],
        ["--max-chars", "255"],
    ] {
        assert!(!fixture
            .command()
            .args(args)
            .output()
            .unwrap()
            .status
            .success());
    }
    assert!(!fixture.root().join("recipient/.watch.lock").exists());
    fixture
        .db()
        .execute("UPDATE messages SET sender=?1", ["s".repeat(500)])
        .unwrap();
    let result = fixture
        .command()
        .args(["--once", "--max-chars", "256"])
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("no message fits"));
    assert!(result.stdout.is_empty());
    assert_eq!(fixture.count("message_presentations"), 0);
}

#[test]
fn missing_stores_and_hostile_leases_do_not_create_or_change_mailbox_content() {
    let host = support::Host::new();
    assert!(!host
        .cli(&["mailbox", "watch", "--as-name", "missing", "--once"])
        .status
        .success());
    assert!(!host.0.join(".agents").exists());
    let fixture = Fixture::new();
    fixture.put("one");
    let sentinel = fixture.0 .0.join("sentinel");
    std::fs::write(&sentinel, "retain").unwrap();
    std::os::unix::fs::symlink(&sentinel, fixture.root().join("recipient/.watch.lock")).unwrap();
    assert!(!fixture.once().status.success());
    assert_eq!(std::fs::read_to_string(sentinel).unwrap(), "retain");
    assert_eq!(fixture.count("message_presentations"), 0);
}
