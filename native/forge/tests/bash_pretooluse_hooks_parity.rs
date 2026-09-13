//! Differential parity test for the three Bash `PreToolUse` hooks:
//! `crg_gate_verify_bash`, `crg_refresh_report_pre`, and
//! `current_work_guard_bash`.
//!
//! Unlike `guard_parity.rs`/`bash_impact_parity.rs`, most of these cases need
//! real, ephemeral filesystem state (a `git init`'d repo, a claims store, a
//! graph-used marker) rather than a pure string or a static fixture tree --
//! `tests/fixtures/bash_pretooluse_corpus.json` describes each case
//! declaratively (session, owner, command, claims with FIXED absolute
//! timestamps, etc.) and this file rebuilds that filesystem state fresh, in
//! Rust, for every case, then compares the live-computed verdict against the
//! frozen `bash_pretooluse_expected.json` -- no interpreter involved.
//!
//! `src/tooling/hooks/claude/test_bash_pretooluse_hooks_parity_corpus.py` is
//! the Python-side twin: it loads the SAME two fixtures, independently
//! rebuilds the SAME filesystem state in Python, and asserts Python's own
//! hook implementations still match the same frozen expected values. Together
//! the two tests pin both implementations to one shared ground truth instead
//! of comparing them to each other at test time, so this crate no longer
//! needs a Python interpreter (or a project `.venv`) to run `cargo test`.
//!
//! Claim timestamps in the corpus are fixed absolute dates, not offsets from
//! "now": `ownership::Claim::active` treats a claim dated entirely in the far
//! future as freshly created (never yet idle) for any real "now" before that
//! date, and one dated entirely in the past (less than the 24h
//! `MAX_CLAIM_HOURS` cap apart) as unconditionally lapsed -- both readings
//! are then stable forever, unlike a `now()`-relative offset whose derived
//! `claim_id` (a hash over the timestamps) would change every run.
//!
//! This crate has no lib target: the modules under test are pulled in via
//! `#[path]`, the same way `guard_parity.rs` already does.

#[path = "../src/civil.rs"]
mod civil;
#[path = "../src/crg_gate.rs"]
mod crg_gate;
#[path = "../src/crg_refresh.rs"]
mod crg_refresh;
#[path = "../src/current_work_guard.rs"]
mod current_work_guard;
#[path = "../src/identity.rs"]
mod identity;
#[path = "../src/instant.rs"]
mod instant;
#[path = "../src/local_ai_policy.rs"]
mod local_ai_policy;
#[path = "../src/ownership.rs"]
mod ownership;
#[path = "../src/write_targets.rs"]
mod write_targets;

use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;

/// This file's one `#[test]` fn shares process-global env vars
/// (`CRG_GATE_STATE_DIR`, `CRG_DATA_DIR`, `GOVERNANCE_OWNER`,
/// `LOCAL_AI_RUNTIME`) with `crg_gate.rs`'s and `crg_refresh.rs`'s own
/// embedded `#[cfg(test)] mod tests` blocks -- `#[path]`-including a module
/// pulls its test module in too, so those tests run in *this same binary* on
/// `cargo test`'s default multiple threads, alongside this file's one test.
/// The test fn takes `crg_gate::tests::ENV_LOCK` and
/// `crg_refresh::tests::ENV_LOCK`, plus this file's own lock, to serialize
/// against those modules' own tests too.
static ENV_LOCK: Mutex<()> = Mutex::new(());

static COUNTER: AtomicU32 = AtomicU32::new(0);

struct ScratchDir(PathBuf);

impl ScratchDir {
    fn new(label: &str) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("forge-parity-{}-{label}-{n}", std::process::id()));
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

fn make_git_repo(label: &str) -> ScratchDir {
    let root = ScratchDir::new(&format!("{label}-repo"));
    let status = Command::new("git")
        .args(["init", "-q"])
        .current_dir(root.path())
        .status()
        .expect("git init");
    assert!(status.success(), "git init failed for {label}");
    root
}

fn common_dir_of(root: &Path) -> PathBuf {
    crg_gate::checkout_of(root).unwrap().1
}

fn mark_graph_used(state_dir: &Path, session_id: &str) {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(session_id.as_bytes());
    let key: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    std::fs::write(state_dir.join(format!("{key}.graph-used")), b"1").unwrap();
}

/// Matches the corpus generator's (and Python's) canonical-json + sha256
/// claim id: `serde_json`'s default `Value::Object` is a `BTreeMap`, so
/// fields serialize in alphabetical key order with no `preserve_order`
/// feature -- `BTreeMap` here reproduces that order explicitly rather than
/// depending on the crate default.
fn claim_id(owner: &str, paths: &[String], created_at: &str, expires_at: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut fields: BTreeMap<&str, Value> = BTreeMap::new();
    fields.insert("created_at", Value::String(created_at.to_string()));
    fields.insert("expires_at", Value::String(expires_at.to_string()));
    fields.insert("justification", Value::String("because".to_string()));
    fields.insert("owner", Value::String(owner.to_string()));
    fields.insert(
        "paths",
        Value::Array(paths.iter().cloned().map(Value::String).collect()),
    );
    let canonical = serde_json::to_string(&fields).unwrap();
    let digest = Sha256::digest(canonical.as_bytes());
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    format!("claim-{}", &hex[..20])
}

fn write_claims(root: &Path, claims: &[Value]) {
    let mut out = Vec::new();
    for c in claims {
        let owner = c["owner"].as_str().unwrap();
        let paths: Vec<String> = c["paths"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        let created_at = c["created_at"].as_str().unwrap();
        let expires_at = c["expires_at"].as_str().unwrap();
        out.push(serde_json::json!({
            "claim_id": claim_id(owner, &paths, created_at, expires_at),
            "owner": owner, "paths": paths, "justification": "because",
            "created_at": created_at, "expires_at": expires_at,
        }));
    }
    let dir = common_dir_of(root).join("governance");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("ownership-claims.json"),
        serde_json::to_string(&serde_json::json!({"schema_version": 1, "claims": out})).unwrap(),
    )
    .unwrap();
}

/// See `bash_pretooluse_hooks_parity.rs`'s module doc and the `GitRepo`
/// helper this used to be, in `crg_gate.rs`'s test module, for why: Python's
/// `identity.lane_of` reads `.git/HEAD` directly and returns `""` for
/// anything not shaped `ref: refs/heads/<name>`.
fn detach_head(root: &Path) {
    std::fs::write(
        root.join(".git/HEAD"),
        "0000000000000000000000000000000000000000\n",
    )
    .unwrap();
}

fn bash_payload(session_id: &str, command: &str) -> Value {
    serde_json::json!({"session_id": session_id, "tool_name": "Bash", "tool_input": {"command": command}})
}

fn load_corpus() -> Vec<Value> {
    let raw = include_str!("fixtures/bash_pretooluse_corpus.json");
    serde_json::from_str(raw).expect("bash_pretooluse_corpus.json must be valid JSON")
}

fn load_expected() -> BTreeMap<String, Value> {
    let raw = include_str!("fixtures/bash_pretooluse_expected.json");
    serde_json::from_str(raw).expect("bash_pretooluse_expected.json must be valid JSON")
}

fn run_gate_case(case: &Value) -> Value {
    let root = make_git_repo(case["id"].as_str().unwrap());
    let state_dir = ScratchDir::new(&format!("{}-state", case["id"].as_str().unwrap()));
    if let Some(session) = case["graph_used_session"].as_str() {
        mark_graph_used(state_dir.path(), session);
    }
    if let Some(claims) = case["claims"].as_array() {
        if !claims.is_empty() {
            write_claims(root.path(), claims);
        }
    }
    if case["detach_head"].as_bool().unwrap_or(false) {
        detach_head(root.path());
    }
    std::env::set_var("CRG_GATE_STATE_DIR", state_dir.path());
    let payload = bash_payload(
        case["session_id"].as_str().unwrap(),
        case["command"].as_str().unwrap(),
    );
    let owner = case["owner"].as_str().unwrap();
    crg_gate::verify_bash(
        &payload,
        owner,
        root.path(),
        Some(&common_dir_of(root.path())),
        &HashMap::new(),
    )
}

fn run_refresh_case(case: &Value) -> Value {
    let repo = ScratchDir::new(&format!("refresh-{}", case["id"].as_str().unwrap()));
    std::fs::create_dir_all(repo.path().join(".git")).unwrap();
    std::fs::write(repo.path().join(".git/HEAD"), "ref: refs/heads/lane\n").unwrap();
    let data_dir = ScratchDir::new(&format!("refresh-data-{}", case["id"].as_str().unwrap()));
    if let Some(lines) = case["marker_lines"].as_str() {
        std::fs::write(data_dir.path().join("refresh.failed"), lines).unwrap();
    }
    std::env::set_var("CRG_DATA_DIR", data_dir.path());
    crg_refresh::failure_output("PreToolUse", repo.path())
}

fn run_guard_case(case: &Value) -> Value {
    if let Some(runtime) = case["local_ai_runtime"].as_str() {
        std::env::set_var("LOCAL_AI_RUNTIME", runtime);
    }
    let verdict = current_work_guard::run(&case["payload"]);
    std::env::remove_var("LOCAL_AI_RUNTIME");
    verdict
}

#[test]
fn bash_pretooluse_native_hooks_match_the_frozen_corpus() {
    let _guard = ENV_LOCK.lock().unwrap();
    let _gate_guard = crg_gate::tests::ENV_LOCK.lock().unwrap();
    let _refresh_guard = crg_refresh::tests::ENV_LOCK.lock().unwrap();
    // A stray inherited value from the outer shell must never leak into a
    // case that does not explicitly set it.
    std::env::remove_var("CRG_GATE_STATE_DIR");
    std::env::remove_var("CRG_DATA_DIR");
    std::env::remove_var("GOVERNANCE_OWNER");
    std::env::remove_var("LOCAL_AI_RUNTIME");

    let corpus = load_corpus();
    assert!(
        corpus.len() >= 64,
        "expected at least 20 cases per hook (60+ total; 22 gate + 20 refresh \
         + 22 guard = 64 as authored), got {}",
        corpus.len()
    );
    let expected = load_expected();
    assert_eq!(
        expected.len(),
        corpus.len(),
        "every corpus case needs exactly one frozen expected verdict"
    );

    let mut failures = Vec::new();
    for case in &corpus {
        let id = case["id"].as_str().unwrap();
        let hook = case["hook"].as_str().unwrap();
        let verdict = match hook {
            "crg_gate_verify_bash" => run_gate_case(case),
            "crg_refresh_report_pre" => run_refresh_case(case),
            "current_work_guard_bash" => run_guard_case(case),
            other => panic!("unknown hook in corpus case {id:?}: {other:?}"),
        };
        // Every case that touched `CRG_GATE_STATE_DIR`/`CRG_DATA_DIR` ran
        // against a `ScratchDir` this loop iteration still owns; clear both
        // (and the others) before the next case so none of it leaks into the
        // next case, or past this function's locks into a concurrently
        // running `crg_gate::tests::*`/`crg_refresh::tests::*` test.
        std::env::remove_var("CRG_GATE_STATE_DIR");
        std::env::remove_var("CRG_DATA_DIR");
        std::env::remove_var("GOVERNANCE_OWNER");
        std::env::remove_var("LOCAL_AI_RUNTIME");
        let expected_verdict = expected
            .get(id)
            .unwrap_or_else(|| panic!("no frozen expected verdict for case {id:?}"));
        if &verdict != expected_verdict {
            failures.push(format!(
                "case {id:?}: rust={verdict} expected={expected_verdict}"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} parity cases disagreed:\n{}",
        failures.len(),
        corpus.len(),
        failures.join("\n")
    );
}
