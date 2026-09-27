//! Differential parity test for the SessionStart EXPOSED line ported to
//! `native/forge/src/workspace_hygiene.rs` (`workspace_exposure_session`).
//!
//! Unlike a live differential test, this file spawns no Python interpreter at
//! `cargo test` time: `tests/fixtures/workspace_exposure_corpus.json` and
//! `workspace_exposure_expected.json` are a frozen corpus, generated once by
//! running the REAL, unmodified `conductor.workspace_hygiene.exposure_line`
//! over repositories rebuilt from the corpus recipes under deterministic
//! conditions (every mtime the line can see pinned to a fixed epoch, left at
//! "now", or pushed into the future; the line embeds no paths) -- this file
//! loads that frozen ground truth via `include_str!`, rebuilds the same
//! repository states from the same recipes, and compares Rust's own
//! computation against it directly.
//!
//! `python_contracts_workspace_exposure_corpus.rs` checks the shipped Python
//! implementation against the same expected lines. Both Rust tests rebuild
//! the recipes with one shared fixture. The original Python test remains
//! active until the test-selection policy retires it.
//!
//! This crate has no lib target: `workspace_hygiene.rs` is pulled in via
//! `#[path]`, the same way every other parity test in this crate includes its
//! module under test.

#[path = "../../conductor-native/tests/fixtures/workspace_exposure_corpus.rs"]
mod corpus_fixture;
#[path = "../src/workspace_hygiene.rs"]
mod workspace_hygiene;

use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;

/// `workspace_hygiene::configured_integration_branch` reads the
/// process-global `CONDUCTOR_INTEGRATION_BRANCH`; the corpus cases must
/// resolve their line from the recipe alone, so it stays unset throughout
/// (held under a lock like every other env-mutating test in this crate).
static ENV_LOCK: Mutex<()> = Mutex::new(());

static COUNTER: AtomicU32 = AtomicU32::new(0);

struct ScratchDir(PathBuf);

impl ScratchDir {
    fn new(label: &str) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "forge-workspace-exposure-parity-{}-{label}-{n}",
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

#[test]
fn workspace_exposure_native_line_matches_the_frozen_corpus() {
    let _guard = ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    std::env::remove_var("CONDUCTOR_INTEGRATION_BRANCH");

    let corpus: Vec<Value> =
        serde_json::from_str(include_str!("fixtures/workspace_exposure_corpus.json"))
            .expect("workspace_exposure_corpus.json must be valid JSON");
    assert!(
        corpus.len() >= 12,
        "expected 12+ corpus cases, got {}",
        corpus.len()
    );
    let expected: BTreeMap<String, String> =
        serde_json::from_str(include_str!("fixtures/workspace_exposure_expected.json"))
            .expect("workspace_exposure_expected.json must be valid JSON");
    assert_eq!(
        expected.len(),
        corpus.len(),
        "every corpus case needs exactly one frozen expected line"
    );

    let mut failures = Vec::new();
    for case in &corpus {
        let id = case["id"].as_str().unwrap();
        let scratch = ScratchDir::new(id);
        let repo = corpus_fixture::build_case(scratch.path(), case);
        let actual = workspace_hygiene::exposure_line(&repo);
        let expected_line = expected
            .get(id)
            .unwrap_or_else(|| panic!("no frozen expected line for case {id:?}"));
        if &actual != expected_line {
            failures.push(format!(
                "case {id:?}:\n  rust=    {actual}\n  expected={expected_line}"
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
