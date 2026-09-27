//! Native CLI contracts for bounded graph-backed code context. No Python or indexer.

use rusqlite::{params, Connection};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

struct Fixture {
    host: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let host = std::env::temp_dir().join(format!(
            "forge-graph-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&host).unwrap();
        Self { host }
    }

    fn write(&self, relative: &str, text: &str) -> PathBuf {
        let path = self.host.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, text).unwrap();
        path
    }

    fn graph(&self) -> Connection {
        let path = self.host.join(".code-review-graph/graph.db");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            "CREATE TABLE nodes (
               qualified_name TEXT, name TEXT, kind TEXT, file_path TEXT,
               line_start INTEGER, line_end INTEGER, signature TEXT, file_hash TEXT);
             CREATE TABLE edges (
               source_qualified TEXT, target_qualified TEXT, kind TEXT, line INTEGER);
             CREATE INDEX nodes_file_path ON nodes(file_path);
             CREATE INDEX edges_source ON edges(source_qualified);
             CREATE INDEX edges_target ON edges(target_qualified);",
        )
        .unwrap();
        conn
    }

    fn command(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_forge"))
            .args(["graph", "--host"])
            .arg(&self.host)
            .args(args)
            .output()
            .unwrap()
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
        assert!(output.stdout.is_empty());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains(expected), "expected {expected:?}: {stderr}");
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.host).unwrap();
    }
}

fn node(conn: &Connection, name: &str, file: &Path, line: i64, hash: Option<&str>) {
    let qualified = format!("{}::{name}", file.display());
    conn.execute(
        "INSERT INTO nodes VALUES (?1,?2,'Function',?3,?4,?5,?6,?7)",
        params![
            qualified,
            name,
            file.to_string_lossy(),
            line,
            line + 1,
            format!("def {name}():"),
            hash
        ],
    )
    .unwrap();
}

fn edge(
    conn: &Connection,
    source: &Path,
    source_name: &str,
    target: &Path,
    target_name: &str,
    kind: &str,
    line: i64,
) {
    conn.execute(
        "INSERT INTO edges VALUES (?1,?2,?3,?4)",
        params![
            format!("{}::{source_name}", source.display()),
            format!("{}::{target_name}", target.display()),
            kind,
            line
        ],
    )
    .unwrap();
}

#[test]
fn context_projects_exact_symbol_and_directed_edges() {
    let fixture = Fixture::new();
    let target = fixture.write(
        "pkg/mod.py",
        "def ping():\n    return 'pong'\n\ndef other():\n    pass\n",
    );
    let caller = fixture.write("pkg/caller.py", "def invoke():\n    ping()\n");
    let callee = fixture.write("pkg/helper.py", "def help():\n    return 1\n");
    let conn = fixture.graph();
    node(&conn, "ping", &target, 1, None);
    node(&conn, "other", &target, 4, None);
    node(&conn, "invoke", &caller, 1, None);
    node(&conn, "help", &callee, 1, None);
    edge(&conn, &caller, "invoke", &target, "ping", "CALLS", 2);
    edge(&conn, &target, "ping", &callee, "help", "CALLS", 2);
    edge(&conn, &target, "ping", &callee, "help", "CONTAINS", 0);

    let result = fixture.ok(&["context", "pkg/mod.py", "--symbol", "ping"]);
    assert_eq!(result["graph_status"], "ok");
    assert_eq!(result["file_path"], "pkg/mod.py");
    assert_eq!(result["symbols"].as_array().unwrap().len(), 1);
    assert_eq!(result["symbols"][0]["name"], "ping");
    assert_eq!(result["source"]["line_start"], 1);
    assert!(result["source"]["text"]
        .as_str()
        .unwrap()
        .contains("return 'pong'"));
    assert_eq!(result["callers"].as_array().unwrap().len(), 1);
    assert_eq!(result["callers"][0]["line"], 2);
    assert_eq!(result["callers"][0]["file_path"], "pkg/caller.py");
    assert_eq!(result["callees"].as_array().unwrap().len(), 1);
    assert_eq!(result["callees"][0]["file_path"], "pkg/helper.py");
    assert_eq!(result["callees"][0]["line"], 1);

    let exact = fixture.ok(&["context", "pkg/mod.py", "--symbol", "other"]);
    assert_eq!(exact["callers"].as_array().unwrap().len(), 0);
    assert_eq!(exact["callees"].as_array().unwrap().len(), 0);
    fixture.error(&["context", "pkg/mod.py", "--symbol", "ping%"], "--symbol");
}

#[test]
fn missing_or_stale_index_is_visible_and_never_created() {
    let fixture = Fixture::new();
    let target = fixture.write("pkg/mod.py", "def ping():\n    return 1\n");
    let missing = fixture.ok(&["context", "pkg/mod.py"]);
    assert_eq!(missing["graph_status"], "unavailable (graph.db missing)");
    assert!(!fixture.host.join(".code-review-graph/graph.db").exists());

    let conn = fixture.graph();
    node(&conn, "ping", &target, 1, Some(&"0".repeat(64)));
    let stale = fixture.ok(&["context", "pkg/mod.py", "--symbol", "ping"]);
    assert_eq!(stale["graph_status"], "stale (indexed source hash differs)");
    assert_eq!(stale["callers"].as_array().unwrap().len(), 0);
    assert_eq!(stale["callees"].as_array().unwrap().len(), 0);
}

#[test]
fn optional_edge_line_uses_caller_definition_line() {
    let fixture = Fixture::new();
    let target = fixture.write("pkg/target.py", "def target(): pass\n");
    let caller = fixture.write("pkg/caller.py", "# one\n# two\ndef invoke(): pass\n");
    let graph_dir = fixture.host.join(".code-review-graph");
    std::fs::create_dir_all(&graph_dir).unwrap();
    let conn = Connection::open(graph_dir.join("graph.db")).unwrap();
    conn.execute_batch(
        "CREATE TABLE nodes (qualified_name TEXT, name TEXT, kind TEXT, file_path TEXT, line_start INTEGER, line_end INTEGER);
         CREATE TABLE edges (source_qualified TEXT, target_qualified TEXT, kind TEXT);",
    )
    .unwrap();
    conn.execute(
        "INSERT INTO nodes VALUES (?1,'target','Function',?2,1,1)",
        params![
            format!("{}::target", target.display()),
            target.to_string_lossy()
        ],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO nodes VALUES (?1,'invoke','Function',?2,3,3)",
        params![
            format!("{}::invoke", caller.display()),
            caller.to_string_lossy()
        ],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO edges VALUES (?1,?2,'CALLS')",
        params![
            format!("{}::invoke", caller.display()),
            format!("{}::target", target.display())
        ],
    )
    .unwrap();
    let result = fixture.ok(&["context", "pkg/target.py"]);
    assert_eq!(result["callers"][0]["line"], 3);
}

#[test]
fn embedded_nul_in_index_metadata_fails_without_partial_json() {
    let fixture = Fixture::new();
    let target = fixture.write("pkg/mod.py", "def ping(): pass\n");
    let conn = fixture.graph();
    conn.execute(
        "INSERT INTO nodes VALUES (?1,'ping','Function',?2,1,1,NULL,NULL)",
        params![
            format!("{}::pi\0ng", target.display()),
            target.to_string_lossy()
        ],
    )
    .unwrap();
    fixture.error(&["context", "pkg/mod.py"], "metadata contains NUL");

    let oversized = Fixture::new();
    let source = oversized.write("pkg/mod.py", "def ping(): pass\n");
    let db = oversized.graph();
    db.execute(
        "INSERT INTO nodes VALUES (?1,'ping','Function',?2,1,1,NULL,NULL)",
        params!["q".repeat(600), source.to_string_lossy()],
    )
    .unwrap();
    oversized.error(&["context", "pkg/mod.py"], "field byte limit");
}

#[test]
fn missing_schema_is_explicit_and_outside_source_is_rejected() {
    let fixture = Fixture::new();
    fixture.write("pkg/mod.py", "def ping(): pass\n");
    let graph_dir = fixture.host.join(".code-review-graph");
    std::fs::create_dir_all(&graph_dir).unwrap();
    let conn = Connection::open(graph_dir.join("graph.db")).unwrap();
    conn.execute_batch("CREATE TABLE nodes (name TEXT);")
        .unwrap();
    let missing = fixture.ok(&["context", "pkg/mod.py"]);
    assert!(missing["graph_status"]
        .as_str()
        .unwrap()
        .starts_with("unavailable (graph schema missing"));

    let outside = fixture.host.with_extension("outside.py");
    std::fs::write(&outside, "def escape(): pass\n").unwrap();
    fixture.error(
        &["context", outside.to_str().unwrap()],
        "code path escapes host",
    );
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&outside, fixture.host.join("pkg/link.py")).unwrap();
        fixture.error(&["context", "pkg/link.py"], "code path escapes host");
    }
    std::fs::remove_file(outside).unwrap();
}

#[test]
fn refs_deduplicate_bound_output_and_skip_uncontained_paths() {
    let fixture = Fixture::new();
    let target = fixture.write("pkg/a.py", "def ping():\n    return 'π'\n");
    fixture.write("pkg/b.py", "def other(): pass\n");
    let conn = fixture.graph();
    node(&conn, "ping", &target, 1, None);
    let body = "inspect pkg/a.py::ping then pkg/a.py::ping and pkg/b.py::other";
    let result = fixture.ok(&["refs", "--text", body, "--max-refs", "1"]);
    assert_eq!(result["authority"], "bounded-a2a-code-context");
    assert_eq!(result["contexts"].as_array().unwrap().len(), 1);
    assert_eq!(result["contexts"][0]["path"], "pkg/a.py");
    assert_eq!(result["omitted_refs"], 1);
    assert!(result["contexts"][0]["source"]
        .as_str()
        .unwrap()
        .contains('π'));

    let bounded = fixture.command(&["refs", "--text", body, "--max-bytes", "256"]);
    assert!(
        bounded.status.success(),
        "{}",
        String::from_utf8_lossy(&bounded.stderr)
    );
    assert!(bounded.stdout.len() <= 257); // newline is outside the JSON budget
    let out: Value = serde_json::from_slice(&bounded.stdout).unwrap();
    assert!(out["omitted_refs"].as_u64().unwrap() <= 2);
    fixture.error(&["refs", "--text", &"x".repeat(4_097)], "--scan-bytes");

    let body_file = fixture.write(
        "message.txt",
        &format!("pkg/a.py::ping {} pkg/b.py::other", "x".repeat(300)),
    );
    let prefix = fixture.ok(&[
        "refs",
        "--body-file",
        body_file.to_str().unwrap(),
        "--scan-bytes",
        "256",
    ]);
    assert_eq!(prefix["input_truncated"], true);
    assert_eq!(prefix["contexts"].as_array().unwrap().len(), 1);
}

#[test]
fn unicode_nul_and_source_limits_fail_without_partial_output() {
    let fixture = Fixture::new();
    fixture.write("pkg/unicode.rs", "fn café() { println!(\"ok\"); }\n");
    let result = fixture.ok(&["context", "pkg/unicode.rs"]);
    assert!(result["source"]["text"].as_str().unwrap().contains("café"));
    fixture.write("pkg/nul.py", "\0def ping(): pass\n");
    fixture.error(&["context", "pkg/nul.py"], "NUL");
    fixture.write("pkg/large.py", &"x".repeat((1 << 18) + 1));
    fixture.error(&["context", "pkg/large.py"], "exceeds");
}

#[test]
fn indexed_many_row_fixture_keeps_query_and_output_bounded() {
    let fixture = Fixture::new();
    let target = fixture.write("pkg/mod.rs", "fn target() {\n    helper();\n}\n");
    let conn = fixture.graph();
    conn.execute_batch("BEGIN").unwrap();
    node(&conn, "target", &target, 1, None);
    for index in 0..4_000 {
        conn.execute(
            "INSERT INTO nodes VALUES (?1,?2,'Function','noise/unrelated.rs',1,1,NULL,NULL)",
            params![format!("noise::{index}"), format!("unused_{index}")],
        )
        .unwrap();
    }
    let caller_path = fixture.host.join("noise/caller.rs");
    for index in 0..80 {
        let name = format!("caller_{index:03}");
        node(&conn, &name, &caller_path, index + 1, None);
        edge(
            &conn,
            &caller_path,
            &name,
            &target,
            "target",
            "CALLS",
            index + 1,
        );
    }
    conn.execute_batch("COMMIT").unwrap();

    let start = Instant::now();
    let output = fixture.command(&[
        "context",
        "pkg/mod.rs",
        "--symbol",
        "target",
        "--max-edges",
        "5",
        "--max-bytes",
        "4096",
    ]);
    eprintln!("bounded graph fixture: {} ms", start.elapsed().as_millis());
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.len() <= 4097); // JSON budget plus newline
    let context: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(context["graph_status"], "ok", "{context}");
    assert!(context["callers"].as_array().unwrap().len() <= 5);
    assert_eq!(context["truncated"], true);
    assert_eq!(
        context["callers"][0]["qualified_name"],
        "noise/caller.rs::caller_000"
    );

    let tighter = fixture.command(&[
        "context",
        "pkg/mod.rs",
        "--symbol",
        "target",
        "--max-edges",
        "5",
        "--max-bytes",
        "512",
    ]);
    assert!(
        tighter.status.success(),
        "{}",
        String::from_utf8_lossy(&tighter.stderr)
    );
    assert!(tighter.stdout.len() <= 513);
}
