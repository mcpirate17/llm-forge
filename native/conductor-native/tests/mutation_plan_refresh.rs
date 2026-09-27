use conductor_native::mutation_plan::{compute_refresh, RefreshRequest};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Tree(PathBuf);

impl Tree {
    fn new() -> Self {
        let serial = NEXT.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "conductor-mutation-refresh-{}-{serial}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        Self(root)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn write(&self, relative: &str, body: &str) {
        let path = self.0.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn refresh(
    root: &Tree,
    language: &str,
    existing: Value,
    sources: &[&str],
) -> Result<Value, String> {
    let request: RefreshRequest = serde_json::from_value(json!({
        "language": language,
        "repo_root": root.path(),
        "manifest_path": root.path().join("campaigns/recorded.json"),
        "campaign_id": "recorded",
        "existing": existing,
        "sources": sources,
        "jobs": 2,
        "run_timeout_seconds": 91
    }))
    .unwrap();
    compute_refresh(&request)
}

fn rust_tree() -> Tree {
    let root = Tree::new();
    root.write(
        "crate/Cargo.toml",
        "[package]\nname = \"widget\"\nversion = \"0.1.0\"\n",
    );
    root.write(
        "crate/src/lib.rs",
        "pub fn one() {}\n#[cfg(test)] mod tests { #[test] fn one() {} }\n",
    );
    root.write("crate/src/extra.rs", "pub fn two() {}\n");
    root
}

fn rust_record() -> Value {
    json!({
        "mutation_engine": "cargo-mutants",
        "generator": {
            "options": {"package": "widget", "manifest_path": "crate/Cargo.toml"},
            "source": ["src/lib.rs"]
        },
        "survivor_baseline": ["engine-kept"],
        "survivor_baseline_recorded": true,
        "survivor_baseline_note": "engine note",
        "survivor_baseline_recorded_at": "2026-09-09T00:00:00Z"
    })
}

#[test]
fn python_refresh_rebinds_source_and_test_pins_without_erasing_ratchet() {
    let root = Tree::new();
    root.write("pkg/subject.py", "x = 2\n");
    root.write("pkg/test_subject.py", "def test_subject(): assert 2 == 2\n");
    let existing = json!({
        "mutation_engine": "fest",
        "generator": {"source": ["pkg/subject.py"]},
        "source_sha256": {"pkg/subject.py": "old"},
        "test_sha256": {"pkg/test_subject.py": "old"},
        "survivor_baseline": ["engine-kept"],
        "survivor_baseline_recorded": true,
        "survivor_baseline_note": "engine note"
    });
    let result = refresh(&root, "python", existing, &[]).unwrap();
    assert_eq!(result["generator"]["source"], json!(["pkg/subject.py"]));
    assert_eq!(result["generator"]["run_timeout_seconds"], 91);
    assert_eq!(
        result["test_argv"].as_array().unwrap().last().unwrap(),
        "pkg/test_subject.py"
    );
    assert_ne!(result["source_sha256"]["pkg/subject.py"], "old");
    assert_ne!(result["test_sha256"]["pkg/test_subject.py"], "old");
    assert_eq!(result["survivor_baseline"], json!(["engine-kept"]));
    assert_eq!(result["survivor_baseline_recorded"], true);
    assert_eq!(result["survivor_baseline_note"], "engine note");
    assert!(!root.path().join("campaigns/recorded.json").exists());
}

#[test]
fn python_extra_test_refresh_requires_recorded_test_list() {
    let root = Tree::new();
    root.write("pkg/_quiet.py", "LIMIT = 4000\n");
    root.write("pkg/test_quiet.py", "def test_limit(): assert True\n");
    let mut existing = json!({
        "mutation_engine": "fest",
        "generator": {"source": ["pkg/_quiet.py"]},
        "test_sha256": {"pkg/test_quiet.py": "old"},
        "survivor_baseline": ["kept"]
    });
    let result = refresh(&root, "python", existing.clone(), &[]).unwrap();
    assert_eq!(result["test_sha256"].as_object().unwrap().len(), 1);
    assert_eq!(
        result["test_argv"].as_array().unwrap().last().unwrap(),
        "pkg/test_quiet.py"
    );
    assert_eq!(result["survivor_baseline"], json!(["kept"]));
    existing["test_sha256"] = json!({});
    assert!(refresh(&root, "python", existing, &[])
        .unwrap_err()
        .contains("no recorded test list"));
}

#[test]
fn rust_refresh_scopes_pins_and_keeps_every_recorded_ratchet_field() {
    let root = rust_tree();
    let result = refresh(&root, "rust", rust_record(), &["crate/src/extra.rs"]).unwrap();
    assert_eq!(result["generator"]["source"], json!(["src/extra.rs"]));
    assert_eq!(result["generator"]["jobs"], 2);
    assert_eq!(result["generator"]["run_timeout_seconds"], 91);
    assert_eq!(result["source_sha256"].as_object().unwrap().len(), 1);
    assert_eq!(result["source_sha256"], result["test_sha256"]);
    assert_eq!(result["survivor_baseline"], json!(["engine-kept"]));
    assert_eq!(result["survivor_baseline_recorded"], true);
    assert_eq!(result["survivor_baseline_note"], "engine note");
    assert_eq!(
        result["survivor_baseline_recorded_at"],
        "2026-09-09T00:00:00Z"
    );
    let error = refresh(&root, "rust", rust_record(), &["elsewhere/lib.rs"]).unwrap_err();
    assert!(
        error.contains("scoped source is not in 'widget'"),
        "{error}"
    );
}

#[test]
fn rust_implicit_scope_rejects_empty_malformed_and_unbound_paths() {
    let root = rust_tree();
    for (source, expected) in [
        (json!([]), "no non-empty generated Rust source scope"),
        (
            json!(["../escape.rs"]),
            "malformed generated Rust source scope",
        ),
        (json!(["src/absent.rs"]), "outside its current crate"),
    ] {
        let mut existing = rust_record();
        existing["generator"]["source"] = source;
        let error = refresh(&root, "rust", existing, &[]).unwrap_err();
        assert!(error.contains(expected), "{error}");
    }
    let result = refresh(&root, "rust", rust_record(), &[]).unwrap();
    assert_eq!(result["generator"]["source"], json!(["src/lib.rs"]));
}

#[test]
fn rust_legacy_manifest_path_repairs_only_an_unambiguous_package() {
    let root = rust_tree();
    let mut existing = rust_record();
    existing["generator"]["options"]["manifest_path"] = json!("gone/Cargo.toml");
    let result = refresh(&root, "rust", existing.clone(), &[]).unwrap();
    assert_eq!(
        result["generator"]["options"]["manifest_path"],
        "crate/Cargo.toml"
    );
    root.write(
        "another/Cargo.toml",
        "[package]\nname = \"widget\"\nversion = \"0.1.0\"\n",
    );
    root.write("another/src/lib.rs", "pub fn third() {}\n");
    let error = refresh(&root, "rust", existing, &[]).unwrap_err();
    assert!(error.contains("no longer exists"), "{error}");
}
