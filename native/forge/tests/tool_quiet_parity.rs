//! Frozen compatibility corpus for native PostToolUse output bounding.
//! Historical outputs were captured before retiring the interpreted hooks.
//! The native test consumes only `tests/fixtures/tool_quiet_{corpus,expected}.json`;
//! it never launches an interpreter or regenerates expectations from the subject.
//! The binary-private module is included directly for these contracts.

#[path = "../src/tool_quiet.rs"]
mod tool_quiet;

use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;
use tool_quiet::QuietConfig;

/// This file's one `#[test]` fn also runs `tool_quiet::tests::*` (pulled in
/// via `#[path]` above) in the same binary on `cargo test`'s default
/// multiple threads. Neither suite touches process-global state (no env
/// vars, no shared files), so this lock exists only to match this crate's
/// established parity-test shape, per the task's explicit instruction that
/// any shared test mutex is locked with `unwrap_or_else(PoisonError::into_inner)`.
static ENV_LOCK: Mutex<()> = Mutex::new(());

static COUNTER: AtomicU32 = AtomicU32::new(0);

/// A private scratch dir, removed on drop -- `repo_root` for a case, with
/// `_tq_scratch` beneath it as `save_dir`. Both languages spill to the same
/// relative layout so `where_display`/`saved.relative_to(REPO_ROOT)` render
/// an identical, portable string regardless of this directory's real
/// absolute path on whatever machine runs the test.
struct ScratchDir(PathBuf);

impl ScratchDir {
    fn new(label: &str) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "forge-tool-quiet-parity-{}-{label}-{n}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).unwrap();
        ScratchDir(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn load_corpus() -> Vec<Value> {
    let raw = include_str!("fixtures/tool_quiet_corpus.json");
    serde_json::from_str(raw).expect("tool_quiet_corpus.json must be valid JSON")
}

fn load_expected() -> BTreeMap<String, Value> {
    let raw = include_str!("fixtures/tool_quiet_expected.json");
    serde_json::from_str(raw).expect("tool_quiet_expected.json must be valid JSON")
}

fn as_usize(value: &Value, key: &str, default: usize) -> usize {
    match value.get(key) {
        None => default,
        Some(v) => v
            .as_u64()
            .unwrap_or_else(|| panic!("{key} must be a non-negative integer, got {v:?}"))
            as usize,
    }
}

fn run_case(case: &Value, repo_root: &Path, save_dir: &Path) -> Value {
    let output_field = case
        .get("output_field")
        .and_then(Value::as_str)
        .unwrap_or("");
    let cfg = QuietConfig {
        save_dir,
        repo_root,
        now_stamp: "20260101T000000",
        output_field,
    };
    let payload = &case["payload"];
    match case["kind"].as_str().unwrap() {
        "bash_quiet" => {
            let limit_bytes = as_usize(case, "limit_bytes", tool_quiet::BASH_QUIET_LIMIT_DEFAULT);
            tool_quiet::rewrite_envelope_bash(payload, limit_bytes, &cfg)
        }
        "post_tool_quiet" => {
            let cap = as_usize(case, "cap_bytes", tool_quiet::TOOL_OUTPUT_QUIET_DEFAULT);
            let cap_disabled = cap == 0;
            let (envelope, _unrecognized) =
                tool_quiet::rewrite_envelope_tool(payload, cap, cap_disabled, &cfg);
            envelope
        }
        other => panic!("unknown kind in corpus case {:?}: {other:?}", case["id"]),
    }
}

#[test]
fn tool_quiet_native_hooks_match_the_frozen_corpus() {
    let _guard = ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    let corpus = load_corpus();
    assert!(
        corpus.len() >= 26,
        "expected at least 6 existing + 20 new cases (26+ total), got {}",
        corpus.len()
    );
    let expected = load_expected();
    assert_eq!(
        expected.len(),
        corpus.len(),
        "every corpus case needs exactly one frozen expected envelope"
    );

    let mut failures = Vec::new();
    for case in &corpus {
        let id = case["id"].as_str().unwrap();
        let repo = ScratchDir::new(id);
        let save_dir = repo.path().join("_tq_scratch");
        let actual = run_case(case, repo.path(), &save_dir);
        let expected_envelope = expected
            .get(id)
            .unwrap_or_else(|| panic!("no frozen expected envelope for case {id:?}"));
        if &actual != expected_envelope {
            failures.push(format!(
                "case {id:?}:\n  rust=    {actual}\n  expected={expected_envelope}"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} parity cases disagreed:\n{}",
        failures.len(),
        corpus.len(),
        failures.join("\n\n")
    );
}
