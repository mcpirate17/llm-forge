#![cfg(target_os = "linux")]
#[allow(dead_code)]
mod support;
use serde_json::{json, Value};
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

fn file(root: &Path, name: &str, text: &str) -> PathBuf {
    let path = root.join(name);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, text).unwrap();
    path
}
fn command(host: &support::Host, home: &Path) -> Command {
    let mut command = Command::new(support::forge_binary::path());
    command
        .env_clear()
        .env("PATH", "")
        .env("HOME", home)
        .env("CONDUCTOR_PYTHON", "/forbidden/python")
        .env("FORGE_PYTHON", "/forbidden/python")
        .current_dir(&host.0)
        .args(["clerk", "--host"])
        .arg(&host.0);
    command
}
fn run(mut command: Command) -> Output {
    forge::process_probe::run(&mut command, Duration::from_secs(10), 1024 * 1024).unwrap()
}
fn success(output: &Output) -> Value {
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    serde_json::from_slice(&output.stdout).unwrap()
}
fn failure(output: &Output, text: &str) {
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(output.stdout.is_empty());
    let error: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error.as_object().unwrap().len(), 1);
    assert!(error["error"].as_str().unwrap().contains(text), "{error}");
}

#[test]
fn inventory_is_native_and_home_defaults_are_replaced_by_explicit_roots() {
    let host = support::Host::new();
    let home = support::Host::new();
    let note = file(&host.0, "note.md", "abc");
    let personal = file(&home.0, ".claude/tasks/task.md", "task facts");
    let mut cmd = command(&host, &home.0);
    cmd.args(["inventory", "note.md"]).arg(&personal);
    let inventory = success(&run(cmd));
    assert_eq!(inventory["schema_version"], 1);
    assert_eq!(inventory["authority"], "deterministic-clerk-inventory");
    assert_eq!(inventory["sources"][0]["path"], json!(note));
    assert_eq!(
        inventory["sources"][0]["sha256"],
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    let mut cmd = command(&host, &home.0);
    cmd.env("CONDUCTOR_CLERK_ROOTS", "")
        .arg("inventory")
        .arg(&personal);
    failure(&run(cmd), "outside");
    let external = support::Host::new();
    let extra = file(&external.0, "other.md", "other facts");
    let relative_root = Path::new("..").join(external.0.file_name().unwrap());
    let mut cmd = command(&host, &home.0);
    cmd.env("CONDUCTOR_CLERK_ROOTS", relative_root)
        .arg("inventory")
        .arg(&extra);
    assert_eq!(success(&run(cmd))["sources"].as_array().unwrap().len(), 1);
    let vault = file(&home.0, "Documents/CodexVault/vault.md", "vault facts");
    let mut cmd = command(&host, &home.0);
    cmd.arg("inventory").arg(vault);
    success(&run(cmd));
}

#[test]
fn tilde_host_relative_and_invalid_paths_have_bounded_json_errors() {
    let host = support::Host::new();
    let home = support::Host::new();
    file(&host.0, "note.md", "facts");
    file(&home.0, ".codex/memories/note.md", "memory facts");
    let mut cmd = command(&host, &home.0);
    cmd.args(["inventory", "~/.codex/memories/note.md"]);
    assert_eq!(success(&run(cmd))["sources"].as_array().unwrap().len(), 1);
    for (args, text) in [
        (vec!["inventory", "~other/note"], "named-user"),
        (vec!["inventory", "missing.md"], "source"),
        (vec!["inventory", ".current_work.md"], "forbidden"),
    ] {
        file(&host.0, ".current_work.md", "private test fixture");
        let mut cmd = command(&host, &home.0);
        cmd.args(args);
        failure(&run(cmd), text);
    }
    let mut cmd = command(&host, &home.0);
    cmd.env_remove("HOME").args(["inventory", "note.md"]);
    success(&run(cmd));
    let mut cmd = command(&host, &home.0);
    cmd.env_remove("HOME")
        .env("CONDUCTOR_CLERK_ROOTS", "")
        .args(["inventory", "~/note.md"]);
    failure(&run(cmd), "HOME");
    let mut cmd = command(&host, &home.0);
    cmd.arg("inventory").args(vec!["note.md"; 1025]);
    failure(&run(cmd), "1024");
}

fn saved_document(host: &support::Host, home: &Path) -> PathBuf {
    file(&host.0, "note.md", "abc");
    let mut cmd = command(host, home);
    cmd.args(["inventory", "note.md"]);
    let inventory = success(&run(cmd));
    let mut source = inventory["sources"][0].clone();
    source["prompt_text_chars"] = json!(3);
    let document = json!({"schema_version":1,"authority":"local-clerk-only","task_class":"compaction",
        "created_at":"2026-11-01T12:34:56+00:00","model":"qwen3.5:9b","eval_count":0,
        "sources":[source],"exact_duplicate_groups":[],
        "draft":{"summary":"facts","source_decisions":[],"open_tasks":[],"todo_items":[],
            "duplicate_candidates":[]},"application":"human review required; originals are preserved"});
    file(&host.0, "saved.json", &document.to_string())
}
#[test]
fn validate_is_native_offline_and_rejects_changed_provenance() {
    let host = support::Host::new();
    let home = support::Host::new();
    let path = saved_document(&host, &home.0);
    let mut cmd = command(&host, &home.0);
    cmd.env("OLLAMA_HOST", "not an origin")
        .args(["validate", "saved.json"]);
    assert_eq!(
        success(&run(cmd)),
        json!({"valid":true,"sources":1,"path":path})
    );
    let document: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    for (field, value, reason) in [
        ("authority", json!("approved"), "authority"),
        ("schema_version", json!(true), "schema"),
    ] {
        let mut invalid = document.clone();
        invalid[field] = value;
        fs::write(&path, invalid.to_string()).unwrap();
        let mut cmd = command(&host, &home.0);
        cmd.args(["validate", "saved.json"]);
        failure(&run(cmd), reason);
    }
    saved_document(&host, &home.0);
    file(&host.0, "note.md", "changed facts");
    let mut cmd = command(&host, &home.0);
    cmd.args(["validate", "saved.json"]);
    failure(&run(cmd), "source changed");
}

fn request(socket: &mut TcpStream) -> Value {
    socket
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 1024];
    loop {
        let count = socket.read(&mut buffer).unwrap();
        assert!(count > 0);
        bytes.extend_from_slice(&buffer[..count]);
        assert!(bytes.len() < 80_384);
        if let Some(end) = bytes.windows(4).position(|v| v == b"\r\n\r\n") {
            let headers = std::str::from_utf8(&bytes[..end]).unwrap();
            assert!(headers.starts_with("POST /api/generate HTTP/1.1"));
            let length = headers
                .lines()
                .find_map(|line| {
                    line.split_once(':')
                        .filter(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                        .map(|(_, value)| value.trim().parse::<usize>().unwrap())
                })
                .unwrap();
            if bytes.len() >= end + 4 + length {
                return serde_json::from_slice(&bytes[end + 4..end + 4 + length]).unwrap();
            }
        }
    }
}
struct Server {
    endpoint: String,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<Vec<Value>>>,
}
impl Server {
    fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let worker = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(8);
            let mut observed = Vec::new();
            while !flag.load(Ordering::SeqCst) && Instant::now() < deadline {
                match listener.accept() {
                    Ok((mut socket, _)) => {
                        observed.push(request(&mut socket));
                        let model = json!({"summary":"facts","source_decisions":[],"open_tasks":[],
                            "todo_items":[],"duplicate_candidates":[]});
                        let body=json!({"model":"qwen3.5:9b","response":model.to_string(),"eval_count":12}).to_string();
                        socket
                            .set_write_timeout(Some(Duration::from_secs(3)))
                            .unwrap();
                        write!(socket,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",body.len(),body).unwrap();
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2))
                    }
                    Err(error) => panic!("clerk CLI fixture accept: {error}"),
                }
            }
            observed
        });
        Self {
            endpoint,
            stop,
            worker: Some(worker),
        }
    }
    fn finish(mut self) -> Vec<Value> {
        self.stop.store(true, Ordering::SeqCst);
        self.worker.take().unwrap().join().unwrap()
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(worker) = self.worker.take() {
            let result = worker.join();
            if !thread::panicking() {
                result.unwrap();
            }
        }
    }
}
#[test]
fn explicit_draft_has_no_authority_and_output_is_exclusive() {
    let host = support::Host::new();
    let home = support::Host::new();
    let source = file(&host.0, "note.md", "# Note\noriginal facts");
    let original = fs::read(&source).unwrap();
    let server = Server::new();
    let mut cmd = command(&host, &home.0);
    cmd.env("OLLAMA_HOST", &server.endpoint)
        .args(["draft", "note.md"]);
    failure(&run(cmd), "clerical task");
    let mut cmd = command(&host, &home.0);
    cmd.env("OLLAMA_HOST", &server.endpoint)
        .env("LOCAL_AI_TASK", "compaction")
        .args(["draft", "note.md", "--output", "draft.json"]);
    let written = success(&run(cmd));
    let output = host.0.join("research/tmp/local_clerk/draft.json");
    assert_eq!(written, json!({"written":output,"sources":1}));
    let bytes = fs::read(&output).unwrap();
    let document: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(document["authority"], "local-clerk-only");
    assert_eq!(
        document["application"],
        "human review required; originals are preserved"
    );
    let mut cmd = command(&host, &home.0);
    cmd.env("OLLAMA_HOST", &server.endpoint)
        .env("LOCAL_AI_TASK", "compaction")
        .args(["draft", "note.md", "--output", "draft.json"]);
    failure(&run(cmd), "exists");
    assert_eq!(fs::read(&source).unwrap(), original);
    assert_eq!(fs::read(&output).unwrap(), bytes);
    let mut cmd = command(&host, &home.0);
    cmd.arg("validate").arg(&output);
    success(&run(cmd));
    let mut cmd = command(&host, &home.0);
    cmd.env("OLLAMA_HOST", &server.endpoint)
        .env("LOCAL_AI_TASK", "compaction")
        .args(["draft", "note.md"]);
    assert_eq!(success(&run(cmd))["authority"], "local-clerk-only");
    let observed = server.finish();
    assert_eq!(observed.len(), 2, "invalid requests must not run inference");
    for request in observed {
        assert_eq!(request["model"], "qwen3.5:9b");
        assert_eq!(
            request["options"],
            json!({"num_ctx":2048,"num_predict":384,"temperature":0.1})
        );
        assert!(request["system"]
            .as_str()
            .unwrap()
            .contains("zero authority"));
        assert!(request["prompt"].as_str().unwrap().chars().count() <= 4000);
    }
}

#[test]
fn invalid_destinations_never_contact_the_model() {
    use std::os::unix::fs::symlink;
    let host = support::Host::new();
    let home = support::Host::new();
    file(&host.0, "note.md", "facts");
    let base = host.0.join("research/tmp/local_clerk");
    file(&base, "existing.json", "unchanged");
    symlink(&home.0, base.join("redirect")).unwrap();
    let server = Server::new();
    for (path, reason) in [
        ("../escape.json", "traversal"),
        ("/tmp/outside.json", "output root"),
        ("existing.json", "exists"),
        ("redirect/draft.json", "parent"),
        ("~other/draft.json", "named-user"),
    ] {
        let mut cmd = command(&host, &home.0);
        cmd.env("OLLAMA_HOST", &server.endpoint)
            .env("LOCAL_AI_TASK", "compaction")
            .args(["draft", "note.md", "--output", path]);
        failure(&run(cmd), reason);
    }
    assert!(server.finish().is_empty());
    assert_eq!(fs::read(base.join("existing.json")).unwrap(), b"unchanged");
    assert!(!home.0.join("draft.json").exists());
}

#[test]
fn oversized_output_components_do_not_create_directories_or_contact_model() {
    let host = support::Host::new();
    let home = support::Host::new();
    file(&host.0, "note.md", "facts");
    let server = Server::new();
    for path in [
        format!("{}.json", "x".repeat(251)),
        format!("{}/draft.json", "x".repeat(256)),
    ] {
        let mut cmd = command(&host, &home.0);
        cmd.env("OLLAMA_HOST", &server.endpoint)
            .env("LOCAL_AI_TASK", "compaction")
            .args(["draft", "note.md", "--output", &path]);
        failure(&run(cmd), "component exceeds 255 bytes");
    }
    assert!(server.finish().is_empty());
    assert!(!host.0.join("research/tmp/local_clerk").exists());
}
