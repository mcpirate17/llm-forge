//! Real native processes and disposable loopback fixtures; no external agent messages.
#![cfg(target_os = "linux")]
#[allow(dead_code)]
mod support;
use forge::process_group::ProcessGroup;
use serde_json::Value;
use std::fs;
use std::io::{BufRead, Read};
use std::net::{TcpListener, TcpStream};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

struct Fixture {
    host: support::Host,
    port: u16,
}
impl Fixture {
    fn new() -> Self {
        let host = support::Host::new();
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let result = Self { host, port };
        result.init("sender", port);
        conductor_native::a2a_store::Store::initialize(&result.root(), "sender").unwrap();
        result
    }
    fn root(&self) -> std::path::PathBuf {
        self.host.0.join(".agents/a2a")
    }
    fn init(&self, name: &str, port: u16) {
        parsed(
            self.host
                .command()
                .env("PATH", "")
                .args([
                    "mailbox",
                    "init",
                    "--name",
                    name,
                    "--port",
                    &port.to_string(),
                ])
                .output()
                .unwrap(),
        );
    }
    fn command(&self, duration: &str) -> Command {
        let mut command = self.host.command();
        command
            .env("PATH", "")
            .env("CONDUCTOR_PYTHON", "/bin/false")
            .args([
                "mailbox",
                "supervise",
                "--as-name",
                "sender",
                "--duration",
                duration,
            ]);
        command
    }
    fn state(&self) -> Value {
        fs::read(self.root().join("sender/supervisor.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or(Value::Null)
    }
    fn wait_state(&self, condition: impl Fn(&Value) -> bool) -> Value {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let state = self.state();
            if condition(&state) {
                return state;
            }
            assert!(Instant::now() < deadline, "state did not arrive: {state}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    fn enqueue(&self, recipient: &str, body: &str) -> Value {
        parsed(
            self.host
                .command()
                .env("PATH", "")
                .args([
                    "mailbox",
                    "enqueue",
                    "--from-name",
                    "sender",
                    "--to",
                    recipient,
                    "--body",
                    body,
                ])
                .output()
                .unwrap(),
        )
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
    assert!(fd >= 0, "{}", std::io::Error::last_os_error());
    unsafe { OwnedFd::from_raw_fd(fd as i32) }
}
fn ready(fd: &OwnedFd, milliseconds: i32) -> bool {
    let mut p = libc::pollfd {
        fd: fd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    assert!(unsafe { libc::poll(&mut p, 1, milliseconds) } >= 0);
    p.revents & libc::POLLIN != 0
}
fn signal(fd: &OwnedFd, signal: i32) {
    assert_eq!(
        unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                fd.as_raw_fd(),
                signal,
                std::ptr::null::<libc::siginfo_t>(),
                0u32,
            )
        },
        0
    );
}
struct Running {
    group: ProcessGroup,
    pidfd: OwnedFd,
}
impl Running {
    fn start(command: &mut Command) -> Self {
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let group = ProcessGroup::spawn(command).unwrap();
        let pidfd = pidfd(group.id());
        Self { group, pidfd }
    }
    fn finish(mut self, timeout: Duration) -> Output {
        assert!(
            ready(&self.pidfd, timeout.as_millis() as i32),
            "child did not exit"
        );
        let status = self.group.stop().unwrap();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        self.group
            .take_stdout()
            .unwrap()
            .read_to_end(&mut stdout)
            .unwrap();
        self.group
            .take_stderr()
            .unwrap()
            .read_to_end(&mut stderr)
            .unwrap();
        Output {
            status,
            stdout,
            stderr,
        }
    }
}

#[test]
fn finite_idle_supervision_is_durable_private_and_releases_its_lease() {
    let fixture = Fixture::new();
    let before = Instant::now();
    let result = parsed(fixture.command("1").output().unwrap());
    assert!(before.elapsed() >= Duration::from_secs(1));
    assert!(before.elapsed() < Duration::from_secs(3));
    assert_eq!(result["status"], "completed");
    assert_eq!(
        result["cycles"], 1,
        "idle mailbox must not spin on its own writes"
    );
    assert_eq!(result["endpoint_starts"], 0);
    assert_eq!(result, fixture.state());
    for name in ["supervisor.json", ".supervisor.lock"] {
        assert_eq!(
            fs::metadata(fixture.root().join("sender").join(name))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    assert_eq!(
        parsed(fixture.command("1").output().unwrap())["status"],
        "completed"
    );
}

#[test]
fn expiry_stops_the_owned_receiver_and_keeps_its_store() {
    let fixture = Fixture::new();
    let running = Running::start(fixture.command("2").arg("--serve"));
    let state = fixture.wait_state(|s| s["endpoint_pid"].as_u64().is_some());
    let receiver = pidfd(state["endpoint_pid"].as_u64().unwrap() as u32);
    assert!(TcpStream::connect(("127.0.0.1", fixture.port)).is_ok());
    let result = parsed(running.finish(Duration::from_secs(4)));
    assert_eq!(result["status"], "completed");
    assert_eq!(result["endpoint_starts"], 1);
    assert!(result["endpoint_pid"].is_null());
    assert!(ready(&receiver, 1000));
    assert!(TcpStream::connect(("127.0.0.1", fixture.port)).is_err());
    assert!(fixture.root().join("sender/store.sqlite").is_file());
}

#[test]
fn deadline_cancels_a_database_blocked_flush_without_losing_the_queue() {
    let fixture = Fixture::new();
    let recipient = TcpListener::bind("127.0.0.1:0").unwrap();
    fixture.init("recipient", recipient.local_addr().unwrap().port());
    drop(recipient);
    let queued = fixture.enqueue("recipient", "bounded retry fixture");
    let database = rusqlite::Connection::open(fixture.root().join("sender/store.sqlite")).unwrap();
    database.execute_batch("BEGIN IMMEDIATE").unwrap();
    let before = Instant::now();
    let output = fixture.command("1").output().unwrap();
    assert!(before.elapsed() < Duration::from_secs(3));
    let state = parsed(output);
    assert_eq!(state["status"], "completed");
    assert_eq!(state["last_flush"]["status"], "timeout");
    database.execute_batch("ROLLBACK").unwrap();
    let mut store =
        conductor_native::a2a_store::Store::required(&fixture.root(), "sender", false).unwrap();
    let row = store
        .message(queued["message_id"].as_str().unwrap(), "outbound", 16384)
        .unwrap();
    assert_eq!(row["body"], "bounded retry fixture");
    assert!(matches!(
        row["delivery_status"].as_str(),
        Some("queued" | "pending")
    ));
}

#[test]
fn termination_while_receiver_readiness_is_blocked_reaps_the_child() {
    let fixture = Fixture::new();
    let database = rusqlite::Connection::open(fixture.root().join("sender/store.sqlite")).unwrap();
    database.execute_batch("BEGIN IMMEDIATE").unwrap();
    let running = Running::start(fixture.command("30").arg("--serve"));
    let deadline = Instant::now() + Duration::from_secs(2);
    while TcpStream::connect(("127.0.0.1", fixture.port)).is_err() {
        assert!(Instant::now() < deadline, "receiver never bound");
        std::thread::sleep(Duration::from_millis(5));
    }
    signal(&running.pidfd, libc::SIGTERM);
    let output = running.finish(Duration::from_secs(2));
    assert_eq!(
        output.status.code(),
        Some(130),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fixture.state()["status"], "interrupted");
    assert!(TcpStream::connect(("127.0.0.1", fixture.port)).is_err());
    database.execute_batch("ROLLBACK").unwrap();
}

#[test]
fn an_existing_receiver_survives_supervision_and_termination() {
    let fixture = Fixture::new();
    let mut endpoint = Running::start(fixture.host.command().args([
        "mailbox",
        "serve",
        "--name",
        "sender",
        "--lifetime-seconds",
        "30",
    ]));
    let mut line = String::new();
    std::io::BufReader::new(endpoint.group.take_stdout().unwrap())
        .read_line(&mut line)
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&line).unwrap()["name"],
        "sender"
    );
    let running = Running::start(fixture.command("30").arg("--serve"));
    let state = fixture.wait_state(|s| s["cycles"].as_u64().is_some_and(|v| v > 0));
    assert!(state["endpoint_pid"].is_null());
    signal(&running.pidfd, libc::SIGTERM);
    let output = running.finish(Duration::from_secs(2));
    assert_eq!(output.status.code(), Some(130));
    assert_eq!(fixture.state()["status"], "interrupted");
    assert!(!ready(&endpoint.pidfd, 0));
    assert!(TcpStream::connect(("127.0.0.1", fixture.port)).is_ok());
}

#[test]
fn duplicate_owner_and_invalid_arguments_cannot_overwrite_lifecycle_state() {
    let fixture = Fixture::new();
    for args in [
        vec!["--duration", "NaN"],
        vec!["--duration", "3601"],
        vec!["--interval", "0"],
        vec!["--interval", "inf"],
        vec!["--max-restarts", "11"],
        vec!["--max-messages", "0"],
        vec!["--as-name", "../escape"],
        vec!["--as-name", "missing"],
    ] {
        let result = fixture
            .host
            .command()
            .args(["mailbox", "supervise"])
            .args(if args[0] == "--as-name" {
                vec![]
            } else {
                vec!["--as-name", "sender"]
            })
            .args(args)
            .output()
            .unwrap();
        assert!(!result.status.success());
    }
    assert!(!fixture.root().join("sender/supervisor.json").exists());
    let running = Running::start(&mut fixture.command("30"));
    fixture.wait_state(|s| s["cycles"] == 1);
    let before = fs::read(fixture.root().join("sender/supervisor.json")).unwrap();
    let other = fixture.command("1").output().unwrap();
    assert!(!other.status.success());
    assert!(String::from_utf8_lossy(&other.stderr).contains("busy"));
    assert_eq!(
        fs::read(fixture.root().join("sender/supervisor.json")).unwrap(),
        before
    );
    signal(&running.pidfd, libc::SIGINT);
    assert_eq!(
        running.finish(Duration::from_secs(2)).status.code(),
        Some(130)
    );
}

#[test]
fn signals_cancel_a_live_flush_and_cleanup_only_the_owned_endpoint() {
    let fixture = Fixture::new();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    fixture.init("recipient", listener.local_addr().unwrap().port());
    let queued = fixture.enqueue("recipient", "disposable queued content");
    let running = Running::start(fixture.command("30").arg("--serve"));
    let state = fixture.wait_state(|s| s["endpoint_pid"].as_u64().is_some());
    let receiver = pidfd(state["endpoint_pid"].as_u64().unwrap() as u32);
    listener.set_nonblocking(true).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    let _stream = loop {
        if let Ok((stream, _)) = listener.accept() {
            break stream;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    };
    let before = Instant::now();
    signal(&running.pidfd, libc::SIGTERM);
    let output = running.finish(Duration::from_secs(2));
    assert!(before.elapsed() < Duration::from_secs(2));
    assert_eq!(
        output.status.code(),
        Some(130),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(ready(&receiver, 1000));
    assert_eq!(fixture.state()["status"], "interrupted");
    let mut store =
        conductor_native::a2a_store::Store::required(&fixture.root(), "sender", false).unwrap();
    let row = store
        .message(queued["message_id"].as_str().unwrap(), "outbound", 16384)
        .unwrap();
    assert!(matches!(
        row["delivery_status"].as_str(),
        Some("queued" | "pending")
    ));
    assert_eq!(row["body"], "disposable queued content");
}

#[test]
fn owned_receiver_failure_uses_the_exact_restart_budget() {
    let fixture = Fixture::new();
    let running = Running::start(
        fixture
            .command("30")
            .args(["--serve", "--max-restarts", "1"]),
    );
    let first = fixture.wait_state(|s| s["endpoint_starts"] == 1);
    signal(
        &pidfd(first["endpoint_pid"].as_u64().unwrap() as u32),
        libc::SIGKILL,
    );
    let second = fixture.wait_state(|s| s["endpoint_starts"] == 2);
    assert_ne!(first["endpoint_pid"], second["endpoint_pid"]);
    signal(
        &pidfd(second["endpoint_pid"].as_u64().unwrap() as u32),
        libc::SIGKILL,
    );
    let output = running.finish(Duration::from_secs(4));
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("restart budget"));
    let state = fixture.state();
    assert_eq!(state["status"], "failed");
    assert_eq!(state["endpoint_starts"], 2);
    assert!(state["endpoint_pid"].is_null());
}

#[test]
fn idle_wakeup_delivers_a_later_enqueue_and_preserves_its_message_id() {
    let fixture = Fixture::new();
    let running = Running::start(fixture.command("30").arg("--serve"));
    fixture.wait_state(|s| s["cycles"] == 1);
    let queued = fixture.enqueue("sender", "late local fixture message");
    fixture.wait_state(|s| s["last_flush"]["counts"]["delivered"] == 1);
    signal(&running.pidfd, libc::SIGTERM);
    assert_eq!(
        running.finish(Duration::from_secs(2)).status.code(),
        Some(130)
    );
    let mut store =
        conductor_native::a2a_store::Store::required(&fixture.root(), "sender", false).unwrap();
    let id = queued["message_id"].as_str().unwrap();
    assert_eq!(
        store.message(id, "outbound", 16384).unwrap()["delivery_status"],
        "delivered"
    );
    let received = store.message(id, "inbound", 16384).unwrap();
    assert_eq!(received["body"], "late local fixture message");
    assert!(received["read_at"].is_null());
}

#[test]
fn hostile_lock_and_state_paths_never_modify_their_targets() {
    for name in [".supervisor.lock", "supervisor.json"] {
        let fixture = Fixture::new();
        let sentinel = fixture.host.0.join("sentinel");
        fs::write(&sentinel, "unchanged").unwrap();
        std::os::unix::fs::symlink(&sentinel, fixture.root().join("sender").join(name)).unwrap();
        assert!(!fixture.command("1").output().unwrap().status.success());
        assert_eq!(fs::read_to_string(&sentinel).unwrap(), "unchanged");
        assert!(TcpStream::connect(("127.0.0.1", fixture.port)).is_err());
    }
}
