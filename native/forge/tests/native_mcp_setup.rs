#[allow(dead_code)]
mod support;
use serde_json::{json, Value};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

struct Fixture {
    host: support::Host,
    socket: PathBuf,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl Fixture {
    fn new(change: impl FnOnce(&mut Value, &mut Value)) -> Self {
        let host = support::Host::new();
        fs::set_permissions(&host.0, fs::Permissions::from_mode(0o700)).unwrap();
        let directory = host.0.join("service");
        fs::create_dir(&directory).unwrap();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
        let socket = directory.join("socket");
        let listener = UnixListener::bind(&socket).unwrap();
        fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let mut status = json!({"ok":true,"result":{"pid":std::process::id(),"protocol":1,"stopping":false,"source_root":host.0}});
        let mut catalog = json!({"ok":true,"result":{"tools":[{"name":"code_inspect","inputSchema":{"type":"object","additionalProperties":false,"properties":{},"required":[]}}]}});
        change(&mut status, &mut catalog);
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let worker = std::thread::spawn(move || {
            while !stopped.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream
                            .set_read_timeout(Some(Duration::from_secs(1)))
                            .unwrap();
                        let mut request = String::new();
                        if BufReader::new(&mut stream).read_line(&mut request).is_err() {
                            continue;
                        }
                        let request: Value = serde_json::from_str(&request).unwrap();
                        let reply = if request["method"] == "status" {
                            &status
                        } else {
                            &catalog
                        };
                        if reply.is_null() {
                            // A silent peer must hit the client's deadline, not block forever.
                            while !stopped.load(Ordering::Acquire) {
                                std::thread::sleep(Duration::from_millis(5));
                            }
                        } else {
                            writeln!(stream, "{reply}").unwrap();
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(2))
                    }
                    Err(e) => panic!("listener failed: {e}"),
                }
            }
        });
        Self {
            host,
            socket,
            stop,
            worker: Some(worker),
        }
    }
    fn run(&self, args: &[&str]) -> Output {
        self.host
            .command()
            .env("PATH", "")
            .env("CONDUCTOR_PYTHON", "/missing/interpreter")
            .args(["mcp", "--host"])
            .arg(&self.host.0)
            .args(args)
            .output()
            .unwrap()
    }
    fn bind(&self, args: &[&str]) -> Output {
        let mut options = vec!["bind", "--socket", self.socket.to_str().unwrap()];
        options.extend(args);
        self.run(&options)
    }
    fn config(&self) -> PathBuf {
        self.host.0.join(".mcp.json")
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.worker.take().unwrap().join().unwrap();
    }
}
fn parsed(output: Output) -> Value {
    assert!(output.status.success(), "{output:?}");
    serde_json::from_slice(&output.stdout).unwrap()
}
fn fail(output: Output, fragment: &str) {
    assert!(!output.status.success(), "{output:?}");
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains(fragment), "{stderr}");
}
fn write(path: &Path, text: &str) {
    fs::write(path, text).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o640)).unwrap();
}

#[test]
fn plan_bind_check_unbind_and_exact_rollback_preserve_custom_settings() {
    let fixture = Fixture::new(|_, _| {});
    let original =
        "{\n \"theme\":\"keep\",\"mcpServers\":{\"custom\":{\"command\":\"unchanged\"}}}\n";
    write(&fixture.config(), original);
    let plan = parsed(fixture.bind(&[]));
    assert_eq!(plan["applied"], false);
    assert_eq!(plan["services_started"], false);
    assert_eq!(plan["service"]["tools"], json!(["code_inspect"]));
    assert_eq!(fs::read_to_string(fixture.config()).unwrap(), original);
    assert!(!fixture.host.0.join(".forge-mcp.lock").exists());
    let apply = parsed(fixture.bind(&["--apply"]));
    assert_eq!(apply["after"], plan["after"]);
    let entry = &apply["desired_server"];
    assert_eq!(entry["args"][4], "--source-root");
    assert_eq!(entry["args"][5], json!(fixture.host.0));
    let config: Value = serde_json::from_slice(&fs::read(fixture.config()).unwrap()).unwrap();
    assert_eq!(config["theme"], "keep");
    assert_eq!(config["mcpServers"]["custom"]["command"], "unchanged");
    assert_eq!(config["mcpServers"]["forge"], *entry);
    assert_eq!(
        fs::metadata(fixture.config()).unwrap().permissions().mode() & 0o777,
        0o640
    );
    assert_eq!(parsed(fixture.bind(&["--apply"]))["changed"], false);
    assert_eq!(
        parsed(fixture.run(&["check", "--socket", fixture.socket.to_str().unwrap()]))["configured"],
        true
    );
    parsed(fixture.run(&["rollback", "--apply"]));
    assert_eq!(fs::read_to_string(fixture.config()).unwrap(), original);
    parsed(fixture.bind(&["--apply"]));
    let installed = fs::read(fixture.config()).unwrap();
    parsed(fixture.run(&["unbind", "--apply"]));
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(fixture.config()).unwrap()).unwrap()
            ["mcpServers"]
            .as_object()
            .unwrap()
            .len(),
        1
    );
    parsed(fixture.run(&["rollback", "--apply"]));
    assert_eq!(fs::read(fixture.config()).unwrap(), installed);
}

#[test]
fn legacy_replacement_requires_explicit_flag_and_does_not_capture_custom_entries() {
    let fixture = Fixture::new(|_, _| {});
    let entry = json!({"command":"/old/.venv/bin/python3.12","args":["-m","conductor.crg_server","--repo",fixture.host.0],"cwd":fixture.host.0,"type":"stdio","env":{"CRG_ROLE":"review"}});
    let original = json!({"mcpServers":{"code-review-graph":entry}}).to_string();
    write(&fixture.config(), &original);
    fail(fixture.bind(&["--apply"]), "--replace-legacy");
    assert_eq!(fs::read_to_string(fixture.config()).unwrap(), original);
    parsed(fixture.bind(&["--replace-legacy", "--apply"]));
    let config: Value = serde_json::from_slice(&fs::read(fixture.config()).unwrap()).unwrap();
    assert!(config["mcpServers"].get("code-review-graph").is_none());
    parsed(fixture.run(&["rollback", "--apply"]));
    assert_eq!(fs::read_to_string(fixture.config()).unwrap(), original);
    let mut custom = entry;
    custom["env"]["CUSTOM"] = json!("preserve");
    write(
        &fixture.config(),
        &json!({"mcpServers":{"code-review-graph":custom}}).to_string(),
    );
    parsed(fixture.bind(&["--replace-legacy", "--apply"]));
    let config: Value = serde_json::from_slice(&fs::read(fixture.config()).unwrap()).unwrap();
    assert_eq!(config["mcpServers"]["code-review-graph"], custom);
}

#[test]
fn malformed_oversized_or_conflicting_configs_are_not_replaced() {
    let fixture = Fixture::new(|_, _| {});
    for (text, reason) in [
        ("[]".to_owned(), "must be an object"),
        (
            r#"{"mcpServers":{},"mcpServers":{}}"#.to_owned(),
            "duplicate",
        ),
        (r#"{"mcpServers":[]}"#.to_owned(), "must be an object"),
        (
            r#"{"mcpServers":{"forge":{"command":"unrelated"}}}"#.to_owned(),
            "customized",
        ),
        (" ".repeat(1024 * 1024 + 1), "exceeds"),
    ] {
        write(&fixture.config(), &text);
        fail(fixture.bind(&["--apply", "--replace-legacy"]), reason);
        assert_eq!(fs::read_to_string(fixture.config()).unwrap(), text);
        assert!(!fixture
            .host
            .0
            .join(".mcp.json.forge-native.bak.json")
            .exists());
    }
}

#[test]
fn wrong_host_stopping_or_invalid_catalog_prevents_configuration() {
    for case in 0..4 {
        let fixture = Fixture::new(|status, catalog| match case {
            0 => status["result"]["source_root"] = json!("/wrong-host"),
            1 => status["result"]["stopping"] = json!(true),
            2 => status["result"]["protocol"] = json!(2),
            _ => catalog["result"]["tools"][0]["inputSchema"] = json!({"type":"object"}),
        });
        let output = fixture.bind(&["--apply"]);
        assert!(!output.status.success(), "{output:?}");
        assert!(!fixture.config().exists());
        assert!(!fixture.host.0.join(".forge-mcp.lock").exists());
    }
}

#[test]
fn socket_permissions_symlink_and_silent_peer_fail_bounded_without_writes() {
    let fixture = Fixture::new(|_, _| {});
    fs::set_permissions(&fixture.socket, fs::Permissions::from_mode(0o666)).unwrap();
    fail(fixture.bind(&["--apply"]), "must be private");
    fs::set_permissions(&fixture.socket, fs::Permissions::from_mode(0o600)).unwrap();
    let symlink = fixture.host.0.join("alias");
    std::os::unix::fs::symlink(&fixture.socket, &symlink).unwrap();
    fail(
        fixture.run(&["bind", "--socket", symlink.to_str().unwrap(), "--apply"]),
        "nonsymlink",
    );
    let silent = Fixture::new(|status, _| *status = Value::Null);
    let started = Instant::now();
    fail(silent.bind(&["--apply"]), "deadline exceeded");
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(!silent.config().exists());
}

#[test]
fn rollback_detects_drift_and_initial_binding_can_restore_absence() {
    let fixture = Fixture::new(|_, _| {});
    parsed(fixture.bind(&["--apply"]));
    let installed = fs::read(fixture.config()).unwrap();
    write(&fixture.config(), r#"{"user":"edited"}"#);
    fail(fixture.run(&["rollback", "--apply"]), "changed after");
    fs::write(fixture.config(), &installed).unwrap();
    fs::set_permissions(fixture.config(), fs::Permissions::from_mode(0o600)).unwrap();
    parsed(fixture.run(&["rollback", "--apply"]));
    assert!(!fixture.config().exists());
    assert_eq!(
        parsed(fixture.run(&["rollback", "--apply"]))["changed"],
        false
    );
}

#[test]
fn bound_adapter_refuses_a_different_host_before_exposing_tools() {
    let fixture = Fixture::new(|status, _| status["result"]["source_root"] = json!("/different"));
    let output = fixture
        .host
        .command()
        .env("PATH", "")
        .args(["harness", "stdio", "--socket"])
        .arg(&fixture.socket)
        .arg("--source-root")
        .arg(&fixture.host.0)
        .output()
        .unwrap();
    fail(output, "source root differs");
}
