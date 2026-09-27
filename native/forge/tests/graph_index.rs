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
