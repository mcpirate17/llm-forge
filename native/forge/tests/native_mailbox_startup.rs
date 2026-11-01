//! All endpoints and messages here belong to disposable local fixtures.
#[allow(dead_code)]
mod support;
use serde_json::Value;
use std::fs;
use std::net::{TcpListener, TcpStream};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::process::Output;
use std::time::Duration;

struct Receiver(OwnedFd);
impl Receiver {
    fn owned(pid: u64, host: &support::Host) -> Self {
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid as libc::pid_t, 0) };
        assert!(fd >= 0);
        let guard = Self(unsafe { OwnedFd::from_raw_fd(fd as i32) });
        let command = fs::read(format!("/proc/{pid}/cmdline")).unwrap();
        assert!(String::from_utf8_lossy(&command).contains(host.0.to_str().unwrap()));
        guard
    }
}
impl Drop for Receiver {
    fn drop(&mut self) {
        let code = unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                self.0.as_raw_fd(),
                libc::SIGKILL,
                std::ptr::null::<libc::siginfo_t>(),
                0,
            )
        };
        assert!(code == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH));
        let mut poll = libc::pollfd {
            fd: self.0.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        assert!(unsafe { libc::poll(&mut poll, 1, 3000) } > 0);
    }
}

fn init(host: &support::Host, name: &str, port: u16) {
    let output = host
        .command()
        .env("PATH", "")
        .args(["mailbox", "init", "--name", name, "--port"])
        .arg(port.to_string())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
fn start(host: &support::Host, name: &str, lifetime: u64) -> Output {
    host.command()
        .env("PATH", "")
        .args(["mailbox", "start", "--identity", name, "--lifetime-seconds"])
        .arg(lifetime.to_string())
        .output()
        .unwrap()
}
fn parsed(output: Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}
fn port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

#[test]
fn native_start_readiness_reuse_and_compact_preview_preserve_unread_content() {
    let host = support::Host::new();
    let port = port();
    init(&host, "fixture", port);
    let first = parsed(start(&host, "fixture", 60));
    let _receiver = Receiver::owned(first["receiver"]["pid"].as_u64().unwrap(), &host);
    assert_eq!(first["receiver"]["status"], "started");
    assert_eq!(first["flush_status"], 0);
    let registry = fs::read(host.0.join(".agents/a2a/agents.json")).unwrap();
    let body = "private raw message é ".repeat(2000);
    let mut store =
        conductor_native::a2a_store::Store::required(&host.0.join(".agents/a2a"), "fixture", true)
            .unwrap();
    store
        .record_inbound(
            "fixture-message",
            "fixture-sender",
            "fixture",
            &body,
            None,
            "2026-10-31T00:00:00Z",
        )
        .unwrap();
    let second = parsed(start(&host, "fixture", 60));
    assert_eq!(second["receiver"]["status"], "already-running");
    assert_eq!(
        fs::read(host.0.join(".agents/a2a/agents.json")).unwrap(),
        registry
    );
    let preview = &second["preview"];
    assert_eq!(preview["total"], 1);
    assert_eq!(preview["unread_only"], true);
    assert!(preview.to_string().chars().count() <= 1200);
    assert!(!preview.to_string().contains(&body));
    assert_eq!(preview["raw_bytes_not_injected"], body.len());
    assert!(store
        .message("fixture-message", "inbound", 100_000)
        .unwrap()["read_at"]
        .is_null());
}

#[test]
fn occupied_listener_is_preserved_and_failed_child_is_reported() {
    let host = support::Host::new();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    init(&host, "fixture", port);
    let out = start(&host, "fixture", 60);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("before readiness"));
    assert!(TcpStream::connect(("127.0.0.1", port)).is_ok());
    assert!(
        fs::read_to_string(host.0.join(".agents/a2a/fixture/serve.log"))
            .unwrap()
            .contains("cannot bind")
    );
}

#[test]
fn unregistered_and_unsafe_startup_state_fail_without_starting_receiver() {
    let host = support::Host::new();
    let port = port();
    init(&host, "fixture", port);
    assert!(!start(&host, "missing", 60).status.success());
    let dir = host.0.join(".agents/a2a/fixture");
    fs::create_dir_all(&dir).unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
    let sentinel = host.0.join("sentinel");
    fs::write(&sentinel, "retain").unwrap();
    std::os::unix::fs::symlink(&sentinel, dir.join(".startup.lock")).unwrap();
    assert!(!start(&host, "fixture", 60).status.success());
    assert_eq!(fs::read_to_string(sentinel).unwrap(), "retain");
    assert!(TcpStream::connect(("127.0.0.1", port)).is_err());
}

#[test]
fn automatic_receiver_lifetime_expires_without_a_polling_worker() {
    let host = support::Host::new();
    init(&host, "fixture", port());
    let result = parsed(start(&host, "fixture", 1));
    let receiver = Receiver::owned(result["receiver"]["pid"].as_u64().unwrap(), &host);
    let mut poll = libc::pollfd {
        fd: receiver.0.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    let start = std::time::Instant::now();
    assert!(unsafe { libc::poll(&mut poll, 1, 3000) } > 0);
    assert!(start.elapsed() < Duration::from_secs(3));
}

fn hook(host: &support::Host, provider: &str, identity: Option<&str>, payload: &str) -> Output {
    use std::io::Write;
    use std::process::Stdio;
    let mut command = host.command();
    command.env("PATH", "").env_remove("A2A_AGENT_NAME").args([
        "mailbox",
        "startup-hook",
        "--provider",
        provider,
        "--lifetime-seconds",
        "5",
    ]);
    if let Some(identity) = identity {
        command.args(["--identity", identity]);
    }
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn provider_envelopes_resolve_host_default_and_grok_emits_once_per_session() {
    let host = support::Host::new();
    init(&host, "fixture", port());
    fs::write(host.0.join(".agents/a2a/default_identity"), "fixture\n").unwrap();
    let first = parsed(start(&host, "fixture", 60));
    let _receiver = Receiver::owned(first["receiver"]["pid"].as_u64().unwrap(), &host);
    for provider in ["claude", "codex", "qwen", "grok"] {
        let output = parsed(hook(
            &host,
            provider,
            None,
            r#"{"session_id":"fixture-session"}"#,
        ));
        let expected = if provider == "grok" {
            "UserPromptSubmit"
        } else {
            "SessionStart"
        };
        assert_eq!(output["hookSpecificOutput"]["hookEventName"], expected);
        let context = output["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap();
        assert!(context.chars().count() <= 1200);
        let preview: Value = serde_json::from_str(context).unwrap();
        assert_eq!(preview["agent"], "fixture");
        assert_eq!(preview["unread_only"], true);
        let repeat = hook(&host, provider, None, r#"{"session_id":"fixture-session"}"#);
        assert!(repeat.status.success());
        assert_eq!(repeat.stdout.is_empty(), provider == "grok");
    }
    assert!(!parsed(hook(
        &host,
        "grok",
        Some("fixture"),
        r#"{"session_id":"second"}"#
    ))
    .is_null());
}

#[test]
fn hook_input_and_identity_errors_fail_before_receiver_or_state_changes() {
    let host = support::Host::new();
    let port = port();
    init(&host, "fixture", port);
    for payload in [
        "",
        "[]",
        "{",
        r#"{"session_id":"a","session_id":"b"}"#,
        "{}",
    ] {
        let result = hook(&host, "grok", Some("fixture"), payload);
        assert!(!result.status.success(), "{payload}");
        assert!(result.stdout.is_empty());
    }
    for identity in ["../escape", "not-registered", ""] {
        let result = hook(&host, "codex", Some(identity), "{}");
        assert!(!result.status.success());
        assert!(result.stdout.is_empty());
    }
    assert!(!host.0.join(".agents/a2a/fixture").exists());
    assert!(TcpStream::connect(("127.0.0.1", port)).is_err());
}

#[test]
fn failed_hook_startup_does_not_consume_grok_session() {
    let host = support::Host::new();
    let occupied = TcpListener::bind("127.0.0.1:0").unwrap();
    init(&host, "fixture", occupied.local_addr().unwrap().port());
    let input = r#"{"session_id":"retryable"}"#;
    assert!(!hook(&host, "grok", Some("fixture"), input).status.success());
    assert!(!host
        .0
        .join(".agents/a2a/fixture/.session-start/native.json")
        .exists());
    drop(occupied);
    let first = parsed(start(&host, "fixture", 60));
    let _receiver = Receiver::owned(first["receiver"]["pid"].as_u64().unwrap(), &host);
    assert!(!parsed(hook(&host, "grok", Some("fixture"), input)).is_null());
    assert!(hook(&host, "grok", Some("fixture"), input)
        .stdout
        .is_empty());
}

#[test]
fn concurrent_grok_hooks_emit_at_most_once_and_busy_calls_can_retry() {
    let host = support::Host::new();
    init(&host, "fixture", port());
    let first = parsed(start(&host, "fixture", 60));
    let _receiver = Receiver::owned(first["receiver"]["pid"].as_u64().unwrap(), &host);
    let barrier = std::sync::Barrier::new(12);
    let input = r#"{"session_id":"concurrent"}"#;
    let results = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..12)
            .map(|_| {
                scope.spawn(|| {
                    barrier.wait();
                    hook(&host, "grok", Some("fixture"), input)
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert_eq!(
        results
            .iter()
            .filter(|result| !result.stdout.is_empty())
            .count(),
        1
    );
    for result in results {
        if !result.status.success() {
            assert!(String::from_utf8_lossy(&result.stderr).contains("busy"));
        }
    }
    let repeated = hook(&host, "grok", Some("fixture"), input);
    assert!(repeated.status.success());
    assert!(repeated.stdout.is_empty());
}

#[test]
fn startup_registry_refuses_symlinks_fifos_and_duplicate_keys() {
    let host = support::Host::new();
    let port = port();
    init(&host, "fixture", port);
    let registry = host.0.join(".agents/a2a/agents.json");
    let sentinel = host.0.join("registry-original");
    fs::rename(&registry, &sentinel).unwrap();
    std::os::unix::fs::symlink(&sentinel, &registry).unwrap();
    assert!(!start(&host, "fixture", 5).status.success());
    fs::remove_file(&registry).unwrap();
    let name = std::ffi::CString::new(registry.as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    let begin = std::time::Instant::now();
    assert!(!start(&host, "fixture", 5).status.success());
    assert!(begin.elapsed() < Duration::from_secs(2));
    fs::remove_file(&registry).unwrap();
    fs::write(&registry, r#"{"schema_version":1,"agents":{},"agents":{}}"#).unwrap();
    assert!(!start(&host, "fixture", 5).status.success());
    assert!(TcpStream::connect(("127.0.0.1", port)).is_err());
}
