//! End-to-end native graph indexing fixtures. No Python interpreter or external indexer.

use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "forge-native-graph-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn write(&self, path: &str, content: &str) {
        let target = self.0.join(path);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(target, content).unwrap();
    }

    fn run(&self, args: &[&str]) -> Value {
        let output = Command::new(env!("CARGO_BIN_EXE_forge"))
            .args(["graph", "--host"])
            .arg(&self.0)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }

    fn fails(&self, args: &[&str], expected: &str) {
        let output = Command::new(env!("CARGO_BIN_EXE_forge"))
            .args(["graph", "--host"])
            .arg(&self.0)
            .args(args)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(String::from_utf8_lossy(&output.stderr).contains(expected));
    }

    fn db(&self) -> PathBuf {
        self.0.join(".forge/graph.db")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn names(rows: &Value) -> Vec<String> {
    rows.as_array()
        .unwrap()
        .iter()
        .map(|row| row["qualified_name"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn python_imports_and_local_calls_have_directed_edges() {
    let fixture = Fixture::new();
    fixture.write("pkg/target.py", "def café():\n    return 1\n");
    fixture.write(
        "pkg/caller.py",
        "from pkg.target import café\n\ndef invoke():\n    return café()\n",
    );
    let report = fixture.run(&["index"]);
    assert_eq!(report["authority"], "forge-native-structural-graph");
    assert_eq!(report["files"], 2);
    assert_eq!(report["resolved_calls"], 1);
    assert_eq!(report["embeddings"], "absent");
    assert!(fixture.db().is_file());
    assert!(!fixture.0.join(".code-review-graph/graph.db").exists());

    let target = fixture.run(&["context", "pkg/target.py", "--symbol", "café"]);
    assert_eq!(target["graph_status"], "ok");
    assert_eq!(names(&target["callers"]), vec!["pkg/caller.py::invoke"]);
    assert_eq!(target["callers"][0]["line"], 4);
    let caller = fixture.run(&["context", "pkg/caller.py", "--symbol", "invoke"]);
    assert_eq!(names(&caller["callees"]), vec!["pkg/target.py::café"]);
}

#[test]
fn rust_crate_path_resolves_and_ambiguous_bare_name_stays_unresolved() {
    let fixture = Fixture::new();
    fixture.write("src/util.rs", "pub fn target() -> i32 { 1 }\n");
    fixture.write("other/util.rs", "pub fn target() -> i32 { 2 }\n");
    fixture.write(
        "src/main.rs",
        "mod util;\nfn invoke() -> i32 { crate::util::target() }\nfn unknown() -> i32 { target() }\n",
    );
    let report = fixture.run(&["index"]);
    assert_eq!(report["resolved_calls"], 0); // two util.rs paths make crate mapping ambiguous
    assert_eq!(report["unresolved_calls"], 2);

    fs::remove_file(fixture.0.join("other/util.rs")).unwrap();
    let report = fixture.run(&["index"]);
    assert_eq!(report["resolved_calls"], 1);
    assert_eq!(report["unresolved_calls"], 1);
    let target = fixture.run(&["context", "src/util.rs", "--symbol", "target"]);
    assert_eq!(names(&target["callers"]), vec!["src/main.rs::invoke"]);
    let unknown = fixture.run(&["context", "src/main.rs", "--symbol", "unknown"]);
    assert!(unknown["callees"].as_array().unwrap().is_empty());
}

#[test]
fn edits_are_stale_until_refresh_and_deletion_prunes_relationships() {
    let fixture = Fixture::new();
    fixture.write(
        "pkg/a.py",
        "from pkg.b import target\ndef invoke():\n    target()\n",
    );
    fixture.write("pkg/b.py", "def target():\n    return 1\n");
    fixture.run(&["index"]);
    fixture.write("pkg/b.py", "def target():\n    return 2\n");
    let stale = fixture.run(&["context", "pkg/b.py", "--symbol", "target"]);
    assert_eq!(stale["graph_status"], "stale (indexed source hash differs)");
    assert!(stale["callers"].as_array().unwrap().is_empty());

    fixture.run(&["index"]);
    let fresh = fixture.run(&["context", "pkg/b.py", "--symbol", "target"]);
    assert_eq!(fresh["graph_status"], "ok");
    fs::remove_file(fixture.0.join("pkg/b.py")).unwrap();
    let report = fixture.run(&["index"]);
    assert_eq!(report["files"], 1);
    assert_eq!(report["resolved_calls"], 0);
    assert_eq!(report["unresolved_calls"], 1);
    let caller = fixture.run(&["context", "pkg/a.py", "--symbol", "invoke"]);
    assert!(caller["callees"].as_array().unwrap().is_empty());
}

#[test]
fn parse_failure_preserves_previous_complete_snapshot() {
    let fixture = Fixture::new();
    fixture.write("pkg/valid.py", "def working():\n    return 1\n");
    fixture.run(&["index"]);
    let before = fs::read(fixture.db()).unwrap();
    fixture.write("pkg/broken.py", "def broken(:\n    pass\n");
    fixture.fails(&["index"], "parse source: pkg/broken.py");
    assert_eq!(fs::read(fixture.db()).unwrap(), before);
    let context = fixture.run(&["context", "pkg/valid.py", "--symbol", "working"]);
    assert_eq!(context["graph_status"], "ok");
    assert!(!Path::new(&fixture.0.join(".code-review-graph/graph.db")).exists());
}

#[test]
fn dirty_peer_invalidates_cache_and_current_symbol_locations_are_reparsed() {
    let fixture = Fixture::new();
    fixture.write(
        "pkg/a.py",
        "from pkg.b import target\ndef invoke():\n    return target()\n",
    );
    fixture.write("pkg/b.py", "def target():\n    return 1\n");
    fixture.run(&["index"]);
    let first = fixture.run(&["context", "pkg/b.py", "--symbol", "target"]);
    assert_eq!(first["cache_status"], "miss");
    assert_eq!(
        fixture.run(&["context", "pkg/b.py", "--symbol", "target"])["cache_status"],
        "hit"
    );
    fixture.write("pkg/a.py", "def invoke():\n    return 2\n");
    let dirty = fixture.run(&["context", "pkg/b.py", "--symbol", "target"]);
    assert_eq!(dirty["cache_status"], "miss");
    assert_eq!(
        dirty["graph_status"],
        "stale (relationship endpoint differs)"
    );
    assert!(dirty["callers"].as_array().unwrap().is_empty());
    fixture.write(
        "pkg/b.py",
        "def unrelated():\n    return 0\n\ndef target():\n    return 3\n",
    );
    let moved = fixture.run(&["context", "pkg/b.py", "--symbol", "target"]);
    assert_eq!(moved["source"]["line_start"], 4);
    assert!(moved["source"]["text"]
        .as_str()
        .unwrap()
        .contains("return 3"));
    assert!(!moved["source"]["text"]
        .as_str()
        .unwrap()
        .contains("unrelated"));
    fixture.fails(
        &[
            "context",
            "pkg/b.py",
            "--expected-source-hash",
            first["source_hash"].as_str().unwrap(),
        ],
        "source hash changed",
    );
    fs::remove_file(fixture.0.join("pkg/a.py")).unwrap();
    assert!(
        fixture.run(&["context", "pkg/b.py", "--symbol", "target"])["callers"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn shared_adapter_selects_indirect_tests_and_unknown_inventory_broadens() {
    use conductor_native::graph_context::dispatch;
    use serde_json::json;
    let fixture = Fixture::new();
    fixture.write("pkg/leaf.py", "def leaf():\n    return 1\n");
    fixture.write(
        "pkg/wrapper.py",
        "from pkg.leaf import leaf\ndef wrapper():\n    return leaf()\n",
    );
    fixture.write(
        "tests/test_indirect.py",
        "from pkg.wrapper import wrapper\ndef test_indirect():\n    return wrapper() == 1\n",
    );
    fixture.write(
        "tests/test_other.py",
        "def test_other():\n    return True\n",
    );
    fixture.run(&["index"]);
    let input = json!({"repo": fixture.0, "paths": ["pkg/leaf.py"]});
    let plan = dispatch("test_selection", &input).unwrap();
    assert_eq!(plan["complete"], true, "{plan}");
    assert_eq!(plan["paths"], json!(["tests/test_indirect.py"]));
    let relations = dispatch(
        "relationships",
        &json!({"repo": fixture.0, "file_path": "pkg/leaf.py", "target_symbol": "leaf"}),
    )
    .unwrap();
    assert_eq!(relations["status"], "ok");
    assert_eq!(
        relations["callers"][0]["qualified_name"],
        "pkg/wrapper.py::wrapper"
    );
    fixture.write(
        "tests/test_added.py",
        "from pkg.leaf import leaf\ndef test_added():\n    return leaf()\n",
    );
    let dirty = dispatch("test_selection", &input).unwrap();
    assert_eq!(dirty["complete"], false);
    assert_eq!(dirty["scope"], "full-test-inventory-fallback");
    assert!(dirty["paths"]
        .as_array()
        .unwrap()
        .contains(&json!("tests/test_added.py")));
    assert!(dirty["paths"]
        .as_array()
        .unwrap()
        .contains(&json!("tests/test_other.py")));
    fixture.run(&["index"]);
    let refreshed = dispatch("test_selection", &input).unwrap();
    assert_eq!(refreshed["complete"], true);
    assert_eq!(
        refreshed["paths"],
        json!(["tests/test_added.py", "tests/test_indirect.py"])
    );
}

#[test]
fn bounded_subgraphs_keep_requested_source_and_reject_old_expansion_generations() {
    let fixture = Fixture::new();
    fixture.write("pkg/leaf.py", "def leaf():\n    return 1\n");
    for index in 0..8 {
        fixture.write(
            &format!("pkg/caller{index}.py"),
            "from pkg.leaf import leaf\ndef caller():\n    return leaf()\n",
        );
    }
    fixture.run(&["index"]);
    let output = fixture.run(&[
        "context",
        "pkg/leaf.py",
        "--symbol",
        "leaf",
        "--max-tokens",
        "1024",
        "--max-nodes",
        "1",
        "--max-depth",
        "2",
    ]);
    let bytes = serde_json::to_vec(&output).unwrap().len();
    assert!(bytes <= 1024);
    assert!(output["estimated_tokens"].as_u64().unwrap() as usize >= bytes);
    assert_eq!(output["tokenizer"], "utf8-byte-upper-bound");
    assert!(output["source"]["text"]
        .as_str()
        .unwrap()
        .contains("def leaf"));
    assert!(
        output["callers"].as_array().unwrap().len() + output["callees"].as_array().unwrap().len()
            <= 1
    );
    let depth_zero = fixture.run(&["context", "pkg/leaf.py", "--max-depth", "0"]);
    assert!(depth_zero["callers"].as_array().unwrap().is_empty());
    fixture.write("pkg/leaf.py", "def leaf():\n    return 2\n");
    fixture.run(&["index"]);
    fixture.fails(
        &[
            "context",
            "pkg/leaf.py",
            "--expected-generation",
            output["generation"].as_str().unwrap(),
        ],
        "graph generation changed",
    );
}

#[test]
fn unchanged_index_reuses_all_facts_and_an_edit_only_parses_one_file() {
    let fixture = Fixture::new();
    for index in 0..200 {
        let source = (0..30)
            .map(|name| format!("def op{name}():\n    return {name}\n"))
            .collect::<String>();
        fixture.write(&format!("pkg/module{index}.py"), &source);
    }
    let cold = fixture.run(&["index"]);
    let snapshot = fs::read(fixture.db()).unwrap();
    let reused = fixture.run(&["index"]);
    assert_eq!(cold["parsed_files"], 200);
    assert_eq!(reused["parsed_files"], 0);
    assert_eq!(reused["reused_files"], 200);
    assert_eq!(fs::read(fixture.db()).unwrap(), snapshot);
    fixture.write("pkg/module17.py", "def changed():\n    return 17\n");
    let edited = fixture.run(&["index"]);
    assert_eq!(edited["parsed_files"], 1);
    assert_eq!(edited["reused_files"], 199);
    eprintln!("graph refresh fixture (200 files/6000 definitions): cold={} ms, unchanged={} ms, one-edit={} ms", cold["elapsed_ms"], reused["elapsed_ms"], edited["elapsed_ms"]);
}

#[test]
fn same_size_import_edit_retargets_edges_and_invalidates_generation() {
    let fixture = Fixture::new();
    fixture.write("pkg/a.py", "def op(): return 1\n");
    fixture.write("pkg/b.py", "def op(): return 2\n");
    fixture.write(
        "pkg/caller.py",
        "from pkg.a import op\ndef caller(): return op()\n",
    );
    fixture.run(&["index"]);
    let before = fixture.run(&["context", "pkg/caller.py", "--symbol", "caller"]);
    assert_eq!(names(&before["callees"]), vec!["pkg/a.py::op"]);
    fixture.write(
        "pkg/caller.py",
        "from pkg.b import op\ndef caller(): return op()\n",
    );
    let report = fixture.run(&["index"]);
    assert_eq!(report["parsed_files"], 1);
    assert_eq!(report["reused_files"], 2);
    let after = fixture.run(&["context", "pkg/caller.py", "--symbol", "caller"]);
    assert_eq!(names(&after["callees"]), vec!["pkg/b.py::op"]);
    assert_ne!(before["generation"], after["generation"]);
    assert_eq!(after["cache_status"], "miss");
}

#[test]
fn large_unresolved_inventory_uses_per_file_flags_without_metadata_overflow() {
    use conductor_native::graph_context::dispatch;
    use serde_json::json;
    let fixture = Fixture::new();
    let mut noisy_paths = Vec::new();
    for index in 0..160 {
        let path = format!("pkg/component_with_a_long_but_valid_name_{index:03}.py");
        fixture.write(&path, "def noisy(): return unknown_operation()\n");
        noisy_paths.push(path);
    }
    assert!(serde_json::to_vec(&noisy_paths).unwrap().len() > 4096);
    fixture.write("pkg/clean.py", "def clean(): return 1\n");
    fixture.write(
        "tests/test_clean.py",
        "from pkg.clean import clean\ndef test_clean(): return clean()\n",
    );
    let report = fixture.run(&["index"]);
    assert_eq!(report["unresolved_calls"], 160);
    assert_eq!(
        fixture.run(&["context", "pkg/clean.py", "--symbol", "clean"])["graph_status"],
        "ok"
    );
    let clean = dispatch(
        "test_selection",
        &json!({"repo": fixture.0, "paths": ["pkg/clean.py"]}),
    )
    .unwrap();
    assert_eq!(clean["complete"], true, "{clean}");
    assert_eq!(clean["paths"], json!(["tests/test_clean.py"]));
    assert_eq!(clean["metadata"]["unresolved_files_count"], "160");
    assert!(clean["metadata"].get("unresolved_files").is_none());
    let noisy = dispatch(
        "test_selection",
        &json!({"repo": fixture.0, "paths": [noisy_paths[0]]}),
    )
    .unwrap();
    assert_eq!(noisy["complete"], false);
    assert!(noisy["reasons"].as_array().unwrap().contains(&json!(
        "affected source has unresolved or dynamic dependencies"
    )));
    assert_eq!(fixture.run(&["index"])["parsed_files"], 0);
}
