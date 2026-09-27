//! Interpreter-free graph context policy and read-only SQLite contracts.

use conductor_native::graph_context::dispatch;
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Repo(PathBuf);

impl Repo {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "forge-graph-context-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).unwrap();
        Self(root)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn write(&self, relative: &str, contents: &str) -> PathBuf {
        let path = self.0.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, contents).unwrap();
        path
    }

    fn database(&self) -> Connection {
        let path = self.0.join(".code-review-graph/graph.db");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            "CREATE TABLE nodes (qualified_name TEXT, name TEXT, file_path TEXT); \
             CREATE TABLE edges (source_qualified TEXT, target_qualified TEXT, kind TEXT);",
        )
        .unwrap();
        conn
    }
}

impl Drop for Repo {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn query(repo: &Repo, file: &str, symbol: Option<&str>) -> Value {
    dispatch(
        "relationships",
        &json!({"repo":repo.path(),"file_path":file,"target_symbol":symbol}),
    )
    .unwrap()
}

#[test]
fn absent_graph_uses_bounded_syntactic_callers_without_own_or_hidden_file() {
    let repo = Repo::new();
    repo.write("pkg/target.py", "def target_op(): pass\ntarget_op()\n");
    repo.write(
        "pkg/caller.py",
        "from pkg.target import target_op\ntarget_op()\n",
    );
    repo.write(".hidden/ignored.py", "target_op()\n");
    let result = query(&repo, "pkg/target.py", Some("target_op"));
    assert_eq!(result["status"], "unavailable (graph.db missing)");
    assert_eq!(result["callees"], json!([]));
    let callers = result["callers"].as_array().unwrap();
    assert_eq!(callers.len(), 1);
    assert_eq!(callers[0]["qualified_name"], "pkg/caller.py:2");
    assert_eq!(callers[0]["kind"], "calls (syntactic)");
    let direct = dispatch(
        "syntactic_callers",
        &json!({"repo":repo.path(),"symbol_name":"target_op",
            "target_file_rel":"pkg/target.py"}),
    )
    .unwrap();
    assert_eq!(direct, result["callers"]);
}

#[test]
fn graph_edges_are_distinct_ordered_filtered_and_limited_to_fifty() {
    let repo = Repo::new();
    let source = repo.write("pkg/engine.py", "def run(): pass\n");
    let conn = repo.database();
    conn.execute(
        "INSERT INTO nodes VALUES (?1, ?2, ?3)",
        params!["pkg.engine.run", "run", source.to_str().unwrap()],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO nodes VALUES (?1, ?2, ?3)",
        params!["pkg.sub.helper", "helper", "pkg/sub.py"],
    )
    .unwrap();
    for index in (0..55).rev() {
        let name = format!("caller.{index:03}");
        conn.execute(
            "INSERT INTO nodes VALUES (?1, ?2, ?3)",
            params![name, "invoke", format!("pkg/caller{index:03}.py")],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO edges VALUES (?1, ?2, 'calls')",
            params![name, "pkg.engine.run"],
        )
        .unwrap();
    }
    conn.execute(
        "INSERT INTO edges VALUES ('caller.000', 'pkg.engine.run', 'calls')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO edges VALUES ('caller.000', 'pkg.engine.run', 'contains')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO edges VALUES ('pkg.engine.run', 'pkg.sub.helper', 'calls')",
        [],
    )
    .unwrap();
    drop(conn);
    let result = query(&repo, "pkg/engine.py", Some("run"));
    assert_eq!(result["status"], "ok");
    let callers = result["callers"].as_array().unwrap();
    assert_eq!(callers.len(), 50);
    assert_eq!(callers[0]["qualified_name"], "caller.000");
    assert_eq!(callers[49]["qualified_name"], "caller.049");
    assert_eq!(result["callees"][0]["qualified_name"], "pkg.sub.helper");
    assert_eq!(
        query(&repo, "pkg/engine.py", Some("missing"))["callers"],
        json!([])
    );
}

#[test]
fn malformed_database_fails_closed_and_a_rebuilt_database_can_be_read() {
    let repo = Repo::new();
    repo.write("sample.py", "def ping(): pass\n");
    let db = repo.write(".code-review-graph/graph.db", "not a sqlite database");
    let result = query(&repo, "sample.py", None);
    assert!(result["status"]
        .as_str()
        .unwrap()
        .starts_with("unavailable (sqlite error:"));
    assert_eq!(result["callers"], json!([]));
    assert_eq!(result["callees"], json!([]));
    fs::remove_file(db).unwrap();
    let conn = repo.database();
    drop(conn);
    assert_eq!(query(&repo, "sample.py", None)["status"], "ok");
}

#[test]
fn markdown_separates_test_calls_from_normal_calls_and_caps_each_role() {
    let callers: Vec<Value> = (0..17)
        .map(|index| {
            json!({"qualified_name":format!("caller.{index:02}"),
            "kind":"CALLS","file_path":"pkg/mutation_testing.py"})
        })
        .chain([
            json!({"qualified_name":"suite.test_one","kind":"TESTED_BY",
                "file_path":"pkg/invariant.py"}),
            json!({"qualified_name":"suite.test_two","kind":"CALLS",
                "file_path":"pkg/tests/check.py"}),
        ])
        .collect();
    let result = dispatch(
        "markdown",
        &json!({"file_path":"pkg/mutation_testing.py","skeleton":"  def run(): ...  ",
            "graph_status":"unavailable (graph.db missing)","callers":callers,
            "callees":[{"qualified_name":"dep.call","kind":"CALLS",
                "file_path":"pkg/dependency.py"}]}),
    )
    .unwrap();
    let markdown = result.as_str().unwrap();
    assert!(markdown
        .starts_with("### AST Context: `pkg/mutation_testing.py`\n```python\ndef run(): ...\n```"));
    assert!(markdown.contains("*Notice: code-review-graph unavailable (graph.db missing)*"));
    assert!(markdown.contains("caller.14"));
    assert!(!markdown.contains("caller.15"));
    assert!(markdown.contains("dep.call"));
    assert!(markdown.contains("suite.test_one"));
    assert!(markdown.contains("suite.test_two"));
    assert!(!markdown.contains("`caller.00`\n**Tested By"));
    assert!(markdown.ends_with('\n'));
}

#[test]
fn test_path_classifier_uses_filename_or_tests_directory() {
    for (path, expected) in [
        ("conductor/test_graph_context.py", true),
        ("research/tests/helpers.py", true),
        ("pkg/util_test.py", true),
        ("conductor/mutation_testing.py", false),
        ("research/tools/attestation.py", false),
        ("contest/protest.py", false),
    ] {
        assert_eq!(
            dispatch("is_test_path", &json!({"path":path})).unwrap(),
            json!(expected)
        );
    }
}
