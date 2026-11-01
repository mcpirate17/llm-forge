#![cfg(target_os = "linux")]
#[allow(dead_code)]
mod support;
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::fs;
use std::os::unix::{ffi::OsStrExt, fs::symlink};
use std::process::{Command, Output};
use std::time::Duration;
fn write(host: &support::Host, name: &str, source: &str) {
    let path = host.0.join(name);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, source).unwrap();
}
fn command(host: &support::Host) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_forge"));
    command
        .env_clear()
        .env("PATH", "")
        .env("HOME", &host.0)
        .env("CONDUCTOR_PYTHON", "/forbidden/python")
        .env("FORGE_PYTHON", "/forbidden/python")
        .current_dir(&host.0)
        .args(["code", "context", "--root"])
        .arg(&host.0);
    command
}
fn run(mut command: Command) -> Output {
    forge::process_probe::run(&mut command, Duration::from_secs(15), 1024 * 1024).unwrap()
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
fn failure(output: &Output, message: &str) {
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(output.stdout.is_empty());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains(message),
        "{output:?}"
    );
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[test]
fn native_json_preserves_docs_decorators_class_scope_and_snapshot_binding() {
    let host = support::Host::new();
    let source = r#""""Module documentation."""
@decorate
class Service(Base):
    """Service documentation."""
    flag: bool = True
    @trace
    async def run(self, x: int) -> bool:
        """Run documentation 界."""
        return implementation(x)
    def other(self): return 2

def separate(): return 3
"#;
    write(&host, "service.py", source);
    let mut selected = command(&host);
    selected.args([
        "service.py",
        "--symbol",
        "Service.run",
        "--no-graph",
        "--json",
    ]);
    let output = success(&run(selected));
    assert_eq!(output["schema_version"], 1);
    assert_eq!(output["file_path"], "service.py");
    assert_eq!(output["language"], "python");
    assert_eq!(output["format"], "source-preserving-v1");
    assert_eq!(output["source_sha256"], hash(source.as_bytes()));
    assert_eq!(output["graph_status"], "disabled");
    assert_eq!(output["callers"], json!([]));
    assert_eq!(output["callees"], json!([]));
    let skeleton = output["skeleton"].as_str().unwrap();
    for text in [
        "@decorate",
        "class Service(Base):",
        "@trace",
        "async def run(self, x: int) -> bool:",
        "Run documentation 界.",
        "flag: bool = True",
    ] {
        assert!(skeleton.contains(text), "{skeleton}");
    }
    assert!(!skeleton.contains("implementation"));
    assert!(!skeleton.contains("def other"));
    assert!(!skeleton.contains("def separate"));
    let mut class = command(&host);
    class.args(["service.py", "--symbol", "Service", "--no-graph", "--json"]);
    assert!(success(&run(class))["skeleton"]
        .as_str()
        .unwrap()
        .contains("def other"));
    assert_eq!(
        fs::read(host.0.join("service.py")).unwrap(),
        source.as_bytes()
    );
}

#[test]
fn markdown_default_and_rust_json_are_native_and_leave_sources_unchanged() {
    let host = support::Host::new();
    let source = "/// Public documentation\npub fn work(x: i32) -> i32 { x + 1 }\n";
    write(&host, "work.rs", source);
    let mut cmd = command(&host);
    cmd.args(["work.rs", "--no-graph"]);
    let output = run(cmd);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let markdown = String::from_utf8(output.stdout).unwrap();
    for text in [
        "### AST Context: `work.rs`",
        "```rust",
        "pub fn work(x: i32) -> i32",
        "Public documentation",
        "Source SHA-256:",
        "rust-signatures-v1",
        "graph status: disabled",
    ] {
        assert!(markdown.contains(text), "{markdown}");
    }
    assert!(markdown.contains(&hash(source.as_bytes())));
    assert!(!markdown.contains("x + 1"));
    let mut cmd = command(&host);
    cmd.args(["work.rs", "--no-graph", "--json"]);
    let output = success(&run(cmd));
    assert_eq!(output["language"], "rust");
    assert_eq!(output["format"], "rust-signatures-v1");
    assert_eq!(output["source_sha256"], hash(source.as_bytes()));
    assert_eq!(fs::read(host.0.join("work.rs")).unwrap(), source.as_bytes());
}

fn indexed_graph(host: &support::Host) {
    let target = "def target():\n    return 1\n";
    let caller = "def caller():\n    return target()\n";
    write(host, "pkg/target.py", target);
    write(host, "pkg/caller.py", caller);
    fs::create_dir_all(host.0.join(".forge")).unwrap();
    let connection = Connection::open(host.0.join(".forge/graph.db")).unwrap();
    connection.execute_batch("CREATE TABLE nodes(qualified_name TEXT,name TEXT,kind TEXT,file_path TEXT,file_hash TEXT,line_start INTEGER,line_end INTEGER);
        CREATE TABLE edges(source_qualified TEXT,target_qualified TEXT,kind TEXT);").unwrap();
    for (name, path, source) in [
        ("target", "pkg/target.py", target),
        ("caller", "pkg/caller.py", caller),
    ] {
        connection
            .execute(
                "INSERT INTO nodes VALUES(?1,?1,'Function',?2,?3,1,2)",
                params![name, path, hash(source.as_bytes())],
            )
            .unwrap();
    }
    connection
        .execute("INSERT INTO edges VALUES('caller','target','calls')", [])
        .unwrap();
}
#[test]
fn graph_relationships_and_explicit_unavailable_status_are_preserved() {
    let host = support::Host::new();
    indexed_graph(&host);
    let mut cmd = command(&host);
    cmd.args(["pkg/target.py", "--json"]);
    let output = success(&run(cmd));
    assert_eq!(output["graph_status"], "ok");
    assert_eq!(
        output["callers"],
        json!([{"qualified_name":"caller","kind":"calls","file_path":"pkg/caller.py"}])
    );
    assert_eq!(output["callees"], json!([]));
    write(
        &host,
        "pkg/caller.py",
        "def caller():\n    return other()\n",
    );
    let mut cmd = command(&host);
    cmd.args(["pkg/target.py", "--json"]);
    let stale = success(&run(cmd));
    assert!(stale["graph_status"].as_str().unwrap().starts_with("stale"));
    assert_eq!(stale["callers"], json!([]));
    fs::remove_file(host.0.join(".forge/graph.db")).unwrap();
    let mut cmd = command(&host);
    cmd.args(["pkg/target.py", "--json"]);
    let output = success(&run(cmd));
    assert!(output["graph_status"]
        .as_str()
        .unwrap()
        .starts_with("unavailable"));
    let mut cmd = command(&host);
    cmd.arg("pkg/target.py");
    let output = run(cmd);
    assert_eq!(output.status.code(), Some(0));
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("Notice: code-review-graph unavailable")
    );
}

#[test]
fn source_type_language_and_capability_errors_fail_promptly_without_output() {
    let host = support::Host::new();
    write(&host, "real.py", "def real(): pass\n");
    write(&host, "note.md", "notes");
    symlink(host.0.join("real.py"), host.0.join("alias.py")).unwrap();
    symlink(&host.0, host.0.join("parent-alias")).unwrap();
    let fifo = host.0.join("pipe.py");
    let name = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    for (path, message) in [
        ("note.md", "unsupported"),
        ("missing.py", "not found"),
        ("alias.py", "symlinks"),
        ("parent-alias/real.py", "symlinks"),
        ("pipe.py", "regular file"),
        ("../escape.py", "unsafe"),
        (".git/config.py", "unsafe"),
    ] {
        let mut cmd = command(&host);
        cmd.args([path, "--no-graph", "--json"]);
        failure(&run(cmd), message);
    }
    let mut cmd = command(&host);
    cmd.args(["real.py", "--symbol", "absent", "--no-graph", "--json"]);
    failure(&run(cmd), "not found");
    let mut cmd = command(&host);
    cmd.args(["real.py", "--expected-hash", &"0".repeat(64), "--no-graph"]);
    failure(&run(cmd), "source hash changed");
    assert_eq!(
        fs::read(host.0.join("real.py")).unwrap(),
        b"def real(): pass\n"
    );
}

#[test]
fn source_and_escaped_output_budgets_refuse_instead_of_cutting_context() {
    let host = support::Host::new();
    write(&host, "too-large.py", &" ".repeat(1024 * 1024 + 1));
    let mut cmd = command(&host);
    cmd.args(["too-large.py", "--no-graph", "--json"]);
    failure(&run(cmd), "byte limit");
    // A valid literal under the source byte cap becomes >4 MiB when JSON escaped.
    let source = format!("\"\"\"{}\"\"\"\n", "\u{0001}".repeat(750_000));
    write(&host, "escaped.py", &source);
    let mut cmd = command(&host);
    cmd.args(["escaped.py", "--no-graph", "--json"]);
    failure(&run(cmd), "byte limit");
    assert_eq!(
        fs::metadata(host.0.join("escaped.py")).unwrap().len(),
        source.len() as u64
    );
}

fn hold_mtime(path: &std::path::Path, modified: std::time::SystemTime) {
    fs::OpenOptions::new()
        .write(true)
        .open(path)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(modified))
        .unwrap();
}
#[test]
fn same_size_same_mtime_edits_use_current_signatures_and_reject_old_hash() {
    let host = support::Host::new();
    let modified = std::time::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    for (file, before, after, signature) in [
        (
            "fresh.py",
            "def alpha(x: int) -> int:\n    return x\n",
            "def bravo(x: str) -> str:\n    return x\n",
            "def bravo(x: str) -> str:",
        ),
        (
            "fresh.rs",
            "pub fn alpha(x: i32) -> i32 { x }\n",
            "pub fn bravo(x: u32) -> u32 { x }\n",
            "pub fn bravo(x: u32) -> u32",
        ),
    ] {
        assert_eq!(before.len(), after.len());
        write(&host, file, before);
        let path = host.0.join(file);
        hold_mtime(&path, modified);
        let initial_metadata = fs::metadata(&path).unwrap();
        let mut cmd = command(&host);
        cmd.args([file, "--no-graph", "--json"]);
        let initial = success(&run(cmd));
        let old_hash = initial["source_sha256"].as_str().unwrap();
        assert_eq!(old_hash, hash(before.as_bytes()));
        assert!(initial["skeleton"].as_str().unwrap().contains("alpha"));
        write(&host, file, after);
        hold_mtime(&path, modified);
        let current_metadata = fs::metadata(&path).unwrap();
        assert_eq!(current_metadata.len(), initial_metadata.len());
        assert_eq!(
            current_metadata.modified().unwrap(),
            initial_metadata.modified().unwrap()
        );
        let mut cmd = command(&host);
        cmd.args([file, "--no-graph", "--json"]);
        let current = success(&run(cmd));
        assert_eq!(current["source_sha256"], hash(after.as_bytes()));
        assert_ne!(current["source_sha256"], initial["source_sha256"]);
        assert!(
            current["skeleton"].as_str().unwrap().contains(signature),
            "{current}"
        );
        assert!(!current["skeleton"].as_str().unwrap().contains("alpha"));
        let mut cmd = command(&host);
        cmd.args([file, "--no-graph", "--expected-hash", old_hash, "--json"]);
        failure(&run(cmd), "source hash changed");
        assert_eq!(fs::read(&path).unwrap(), after.as_bytes());
        assert_eq!(fs::metadata(path).unwrap().modified().unwrap(), modified);
    }
}

#[test]
fn unavailable_graph_still_labels_native_syntactic_callers_under_empty_path() {
    let host = support::Host::new();
    write(&host, "target.py", "def target():\n    return 1\n");
    write(
        &host,
        "caller.py",
        "from target import target\n\nvalue = target()\n",
    );
    write(&host, ".gitignore", "generated/\n");
    write(&host, "generated/ignored.py", "value = target()\n");
    let mut cmd = command(&host);
    cmd.args(["target.py", "--symbol", "target", "--json"]);
    let context = success(&run(cmd));
    assert!(context["graph_status"]
        .as_str()
        .unwrap()
        .starts_with("unavailable"));
    let callers = context["callers"].as_array().unwrap();
    assert_eq!(callers.len(), 1, "{context}");
    assert_eq!(callers[0]["kind"], "calls (syntactic)");
    assert_eq!(callers[0]["file_path"], "caller.py");
    assert_eq!(callers[0]["qualified_name"], "caller.py:3");
    assert_eq!(context["callees"], json!([]));
    assert_eq!(
        fs::read(host.0.join("target.py")).unwrap(),
        b"def target():\n    return 1\n"
    );
}
