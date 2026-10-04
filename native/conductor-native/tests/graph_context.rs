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
#[cfg(feature = "source-analysis")]
fn rust_skeleton_preserves_impl_traits_attributes_and_multiline_signatures() {
    let source = "use std::fmt;\npub trait Worker {\n    fn run(&self, input: &str) -> usize;\n}\npub struct Engine { value: usize }\nimpl Worker for Engine {\n    #[inline]\n    fn run(\n        &self, input: &str,\n    ) -> usize { self.value + input.len() }\n}\n";
    let full = dispatch("rust_skeleton", &json!({"source": source})).unwrap();
    let skeleton = full["skeleton"].as_str().unwrap();
    assert!(skeleton.contains("pub trait Worker"));
    assert!(skeleton.contains("impl Worker for Engine"));
    assert!(skeleton.contains("#[inline]"));
    assert!(skeleton.contains("input: &str"));
    assert!(!skeleton.contains("self.value + input.len()"));
    let selected = dispatch(
        "rust_skeleton",
        &json!({"source": source, "target_symbol": "Engine::run"}),
    )
    .unwrap();
    assert!(selected["skeleton"]
        .as_str()
        .unwrap()
        .contains("impl Worker for Engine"));
    assert!(!selected["skeleton"]
        .as_str()
        .unwrap()
        .contains("pub trait Worker"));
    assert!(dispatch(
        "rust_skeleton",
        &json!({"source": source, "target_symbol": "missing"})
    )
    .is_err());
    assert!(dispatch("rust_skeleton", &json!({"source": "fn broken("})).is_err());
}

#[test]
fn external_adapter_validates_dirty_and_deleted_peer_hashes() {
    use sha2::{Digest, Sha256};
    let repo = Repo::new();
    let target = repo.write("pkg/target.py", "def target(): return 1\n");
    let caller = repo.write(
        "pkg/caller.py",
        "from pkg.target import target\ndef caller(): return target()\n",
    );
    let connection = repo.database();
    connection
        .execute_batch("ALTER TABLE nodes ADD COLUMN file_hash TEXT")
        .unwrap();
    for (name, path) in [("target", target), ("caller", caller)] {
        let hash = format!("{:x}", Sha256::digest(fs::read(&path).unwrap()));
        connection
            .execute(
                "INSERT INTO nodes VALUES (?1,?1,?2,?3)",
                params![name, path.to_string_lossy(), hash],
            )
            .unwrap();
    }
    connection
        .execute("INSERT INTO edges VALUES ('caller','target','CALLS')", [])
        .unwrap();
    drop(connection);
    assert_eq!(
        query(&repo, "pkg/target.py", Some("target"))["status"],
        "ok"
    );
    repo.write("pkg/caller.py", "def caller(): return 2\n");
    let dirty = query(&repo, "pkg/target.py", Some("target"));
    assert_eq!(dirty["status"], "stale (relationship endpoint differs)");
    assert!(dirty["callers"].as_array().unwrap().is_empty());
    fs::remove_file(repo.path().join("pkg/caller.py")).unwrap();
    assert_eq!(
        query(&repo, "pkg/target.py", Some("target"))["status"],
        "stale (relationship endpoint differs)"
    );
}

#[test]
fn external_selection_broadens_unverified_coverage_and_exposes_build_revision() {
    let repo = Repo::new();
    repo.write("pkg/source.py", "def source(): return 1\n");
    repo.write("tests/test_source.py", "def test_source(): pass\n");
    repo.write("tests/test_other.py", "def test_other(): pass\n");
    let connection = repo.database();
    connection.execute_batch("CREATE TABLE metadata (key TEXT,value TEXT); INSERT INTO metadata VALUES ('git_head_sha','expected-head')").unwrap();
    drop(connection);
    let plan = dispatch(
        "test_selection",
        &json!({"repo": repo.path(), "paths": ["pkg/source.py"], "expected_head": "expected-head"}),
    )
    .unwrap();
    assert_eq!(plan["metadata"]["git_head_sha"], "expected-head");
    assert_eq!(plan["complete"], false);
    assert_eq!(plan["scope"], "full-test-inventory-fallback");
    assert_eq!(
        plan["paths"],
        json!(["tests/test_other.py", "tests/test_source.py"])
    );
    let stale = dispatch("test_selection", &json!({"repo": repo.path(), "paths": ["pkg/source.py"], "expected_head": "different-head"})).unwrap();
    assert!(stale["reasons"]
        .as_array()
        .unwrap()
        .contains(&json!("graph build revision differs")));
}

#[test]
fn nullable_external_symbol_columns_keep_relationships_available() {
    let repo = Repo::new();
    let connection = repo.database();
    connection.execute_batch("ALTER TABLE nodes ADD COLUMN kind TEXT; ALTER TABLE nodes ADD COLUMN line_start INTEGER; ALTER TABLE nodes ADD COLUMN line_end INTEGER; INSERT INTO nodes VALUES ('target','run','pkg/target.py',NULL,NULL,NULL); INSERT INTO nodes VALUES ('caller','caller','pkg/caller.py',NULL,NULL,NULL); INSERT INTO edges VALUES ('caller','target','CALLS')").unwrap();
    drop(connection);
    let result = query(&repo, "pkg/target.py", Some("run"));
    assert_eq!(result["status"], "ok");
    assert_eq!(result["callers"][0]["qualified_name"], "caller");
}

#[test]
fn full_test_inventory_excludes_fixtures_and_unknown_edges_are_preserved() {
    let repo = Repo::new();
    repo.write("tests/fixtures/config.json", "{}");
    repo.write("tests/Cargo.toml", "[package]\nname='fixture'\n");
    repo.write("tests/README.md", "fixture documentation");
    repo.write("tests/test_real.py", "def test_real(): pass\n");
    repo.write("src/inline.rs", "#[test]\nfn inline() {}\n");
    let connection = repo.database();
    connection.execute_batch("INSERT INTO nodes VALUES ('target','target','pkg/target.py'); INSERT INTO nodes VALUES ('caller','test_real','tests/test_real.py'); INSERT INTO edges VALUES ('caller','target',NULL)").unwrap();
    drop(connection);
    let relations = query(&repo, "pkg/target.py", Some("target"));
    assert_eq!(relations["callers"][0]["kind"], "UNKNOWN");
    let plan = dispatch(
        "test_selection",
        &json!({"repo": repo.path(), "paths": ["pkg/target.py"]}),
    )
    .unwrap();
    assert_eq!(plan["complete"], false);
    assert_eq!(
        plan["paths"],
        json!(["src/inline.rs", "tests/test_real.py"])
    );
    assert!(plan["reasons"]
        .as_array()
        .unwrap()
        .contains(&json!("graph edge kinds unverified")));
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

#[test]
fn candidate_inventory_uses_snapshot_tests_and_keeps_conservative_fallback() {
    let repo = Repo::new();
    let candidate = Repo::new();
    repo.write("tests/test_live_scratch.py", "def test_scratch(): pass\n");
    candidate.write(
        "tests/test_new_candidate.py",
        "def test_candidate(): pass\n",
    );
    repo.database();
    let plan = dispatch(
        "test_selection",
        &json!({
            "repo": repo.path(), "paths": ["pkg/target.py"],
            "inventory_root": candidate.path(),
        }),
    )
    .unwrap();
    assert_eq!(plan["complete"], false);
    assert_eq!(plan["scope"], "full-test-inventory-fallback");
    assert_eq!(plan["paths"], json!(["tests/test_new_candidate.py"]));
    assert!(plan["reasons"]
        .as_array()
        .unwrap()
        .contains(&json!("candidate snapshot requires full test inventory")));
}

#[test]
fn missing_candidate_inventory_fails_closed() {
    let repo = Repo::new();
    repo.database();
    let error = dispatch(
        "test_selection",
        &json!({
            "repo": repo.path(), "paths": ["pkg/target.py"],
            "inventory_root": repo.path().join("missing"),
        }),
    )
    .unwrap_err();
    assert!(error.contains("candidate inventory unavailable"), "{error}");
}
