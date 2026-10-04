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

fn python_record() -> Value {
    json!({
        "mutation_engine": "fest",
        "generator": {"source": ["pkg/subject.py"]},
        "test_sha256": {"pkg/test_feedback.py": "old"},
        "survivor_baseline": ["engine-kept"],
        "survivor_baseline_recorded": true,
        "survivor_baseline_note": "engine note",
        "survivor_baseline_recorded_at": "2026-09-09T00:00:00Z"
    })
}

fn python_refresh_with_extra(root: &Tree, existing: Value, extras: Value) -> Result<Value, String> {
    let request: RefreshRequest = serde_json::from_value(json!({
        "language": "python", "repo_root": root.path(),
        "manifest_path": root.path().join("campaigns/recorded.json"),
        "campaign_id": "recorded", "existing": existing,
        "extra_tests": extras, "run_timeout_seconds": 91
    }))
    .unwrap();
    compute_refresh(&request)
}

#[test]
fn python_refresh_scopes_before_pairing_and_unions_recorded_paired_and_new_tests() {
    let root = Tree::new();
    root.write("pkg/subject.py", "x = 2\n");
    root.write("pkg/test_subject.py", "def test_subject(): assert 2 == 2\n");
    root.write(
        "pkg/test_feedback.py",
        "def test_feedback(): assert 2 == 2\n",
    );
    root.write(
        "pkg/test_selection.py",
        "def test_selection(): assert 2 == 2\n",
    );
    root.write(".claude/hooks/dispatch.py", "def dispatch(): pass\n");
    root.write(
        "first/tests/test_dispatch.py",
        "def test_dispatch(): pass\n",
    );
    root.write(
        "second/tests/test_dispatch.py",
        "def test_dispatch(): pass\n",
    );
    let existing = python_record();
    let result = python_refresh_with_extra(
        &root,
        existing.clone(),
        json!({
            "pkg/subject.py": ["pkg/test_selection.py", "pkg/test_feedback.py"]
        }),
    )
    .unwrap();
    assert_eq!(result["generator"]["source"], json!(["pkg/subject.py"]));
    assert_eq!(
        result["test_sha256"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>(),
        [
            "pkg/test_feedback.py",
            "pkg/test_selection.py",
            "pkg/test_subject.py"
        ]
    );
    assert_eq!(
        &result["test_argv"].as_array().unwrap()[5..],
        &[
            json!("pkg/test_feedback.py"),
            json!("pkg/test_selection.py"),
            json!("pkg/test_subject.py")
        ]
    );
    for field in [
        "survivor_baseline",
        "survivor_baseline_recorded",
        "survivor_baseline_note",
        "survivor_baseline_recorded_at",
    ] {
        assert_eq!(result[field], existing[field]);
    }
    assert_ne!(result["test_sha256"]["pkg/test_feedback.py"], "old");
}

#[test]
fn python_refresh_carries_recorded_tests_after_a_basename_pair_appears() {
    let root = Tree::new();
    root.write("pkg/subject.py", "x = 2\n");
    root.write(
        "pkg/test_feedback.py",
        "def test_feedback(): assert 2 == 2\n",
    );
    let first = python_refresh_with_extra(&root, python_record(), json!({})).unwrap();
    root.write("pkg/test_subject.py", "def test_subject(): assert 2 == 2\n");
    let second = python_refresh_with_extra(&root, first, json!({})).unwrap();
    assert_eq!(second["test_sha256"].as_object().unwrap().len(), 2);
    assert_eq!(second["survivor_baseline"], json!(["engine-kept"]));
}

#[test]
fn python_refresh_rejects_foreign_and_missing_test_bindings() {
    let root = Tree::new();
    root.write("pkg/subject.py", "x = 2\n");
    root.write("pkg/test_subject.py", "def test_subject(): assert 2 == 2\n");
    root.write(
        "pkg/test_feedback.py",
        "def test_feedback(): assert 2 == 2\n",
    );
    let foreign = python_refresh_with_extra(
        &root,
        python_record(),
        json!({
            "pkg/other.py": ["pkg/test_feedback.py"]
        }),
    )
    .unwrap_err();
    assert!(foreign.contains("different Python source"), "{foreign}");
    for test in [
        "pkg/test_missing.py",
        "../test_escape.py",
        "/tmp/test_escape.py",
        "pkg/subject.py",
    ] {
        let error = python_refresh_with_extra(
            &root,
            python_record(),
            json!({
                "pkg/subject.py": [test]
            }),
        )
        .unwrap_err();
        assert!(
            error.contains("existing repository-relative Python test file"),
            "{error}"
        );
    }
    fs::remove_file(root.path().join("pkg/test_feedback.py")).unwrap();
    let missing = python_refresh_with_extra(&root, python_record(), json!({})).unwrap_err();
    assert!(missing.contains("pkg/test_feedback.py"), "{missing}");
}

#[test]
fn python_refresh_still_refuses_ambiguity_for_the_selected_source() {
    let root = Tree::new();
    root.write("pkg/subject.py", "x = 2\n");
    root.write(
        "pkg/test_feedback.py",
        "def test_feedback(): assert 2 == 2\n",
    );
    root.write(
        "first/test_subject.py",
        "def test_subject(): assert 2 == 2\n",
    );
    root.write(
        "second/test_subject.py",
        "def test_subject(): assert 2 == 2\n",
    );
    let error = python_refresh_with_extra(&root, python_record(), json!({})).unwrap_err();
    assert!(error.contains("several unrelated files share"), "{error}");
}

fn refresh_python_worker_width(
    root: &Tree,
    existing: Value,
    width: Option<i64>,
) -> Result<Value, String> {
    let request: RefreshRequest = serde_json::from_value(json!({
        "language": "python", "repo_root": root.path(), "campaign_id": "recorded",
        "manifest_path": root.path().join("campaigns/recorded.json"), "existing": existing,
        "python_jobs": width
    }))
    .unwrap();
    compute_refresh(&request)
}

#[test]
fn python_refresh_retains_width_unless_explicitly_overridden_and_keeps_ratchet() {
    let root = Tree::new();
    root.write("pkg/subject.py", "x = 2\n");
    root.write("pkg/test_feedback.py", "def test_feedback(): pass\n");
    let legacy = refresh_python_worker_width(&root, python_record(), None).unwrap();
    assert!(legacy["generator"].get("jobs").is_none());
    let two = refresh_python_worker_width(&root, legacy.clone(), Some(2)).unwrap();
    assert_eq!(two["generator"]["jobs"], 2);
    let retained = refresh_python_worker_width(&root, two.clone(), None).unwrap();
    assert_eq!(retained["generator"]["jobs"], 2);
    let three = refresh_python_worker_width(&root, retained, Some(3)).unwrap();
    assert_eq!(three["generator"]["jobs"], 3);
    for field in [
        "survivor_baseline",
        "survivor_baseline_recorded",
        "survivor_baseline_note",
        "survivor_baseline_recorded_at",
    ] {
        assert_eq!(three[field], legacy[field]);
    }
    assert_eq!(three["test_sha256"], legacy["test_sha256"]);
}

#[test]
fn python_refresh_rejects_invalid_requested_or_recorded_worker_widths() {
    let root = Tree::new();
    root.write("pkg/subject.py", "x = 2\n");
    root.write("pkg/test_feedback.py", "def test_feedback(): pass\n");
    for width in [0, -1] {
        assert!(
            refresh_python_worker_width(&root, python_record(), Some(width))
                .unwrap_err()
                .contains("worker count must be positive")
        );
    }
    for invalid in [json!(0), json!(-1), json!("two")] {
        let mut recorded = python_record();
        recorded["generator"]["jobs"] = invalid;
        assert!(refresh_python_worker_width(&root, recorded, None)
            .unwrap_err()
            .contains("worker count"));
    }
}
