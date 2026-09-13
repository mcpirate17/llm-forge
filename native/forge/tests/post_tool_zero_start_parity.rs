//! Differential parity test for the four PostToolUse hooks ported by the
//! zero-interpreter-start slice: `crg_refresh_report_post`
//! (`crg_refresh::failure_output`), `post_bash_graph`
//! (`crg_refresh::full_update_output` behind `git_tree_rewrite_matches`),
//! `read_budget`, and `context_telemetry`'s two record builders.
//!
//! `tests/fixtures/post_tool_corpus.json` describes each case declaratively
//! (kind, payload, env overrides, seed state) and this file rebuilds that
//! state fresh, in Rust, for every case, then compares the live-computed
//! result against the frozen `post_tool_expected.json` -- no interpreter
//! involved. `src/tooling/hooks/claude/test_post_tool_parity_corpus.py` is
//! the Python-side twin: it loads the SAME two fixtures, rebuilds the SAME
//! state with the real Python hook modules, and asserts they still match the
//! same frozen values -- both implementations pinned to one shared ground
//! truth instead of compared to each other at test time (the shape
//! `bash_pretooluse_hooks_parity.rs` established).
//!
//! Determinism mirrors the fixture generator exactly:
//!
//! * Telemetry records pin the two volatile fields (`timestamp`, `pid`) via
//!   `event_record`/`hook_context_record`'s explicit-argument forms
//!   (`STAMP`/`PID`, the same constants the generator rewrote into the dict
//!   the Python builders returned -- a value overwrite never reorders a
//!   dict, so the pinned line is the real builder's byte output).
//! * The two graph-queue cases that spawn a worker point `PATH` at a scratch
//!   bin dir (with a stub `code-review-graph` for the tool-present case, an
//!   empty one for the absent case) so the host's real tool can never leak
//!   into a verdict, and the spawned "worker" is the repo's
//!   `.venv/bin/python` -- a `#!/bin/sh` wrapper over `/bin/sleep 30`, the
//!   same stand-in the generator used -- so the spawn really happens
//!   (process group, `refresh.log`) without running any refresh.

#[path = "../src/civil.rs"]
mod civil;
#[path = "../src/context_telemetry.rs"]
mod context_telemetry;
#[path = "../src/crg_refresh.rs"]
mod crg_refresh;
#[path = "../src/instant.rs"]
mod instant;
// `crg_refresh::worker_command` resolves the worker interpreter through
// `crate::interpreter`; the module is self-contained (same note as
// `bash_pretooluse_hooks_parity.rs`).
#[path = "../src/interpreter.rs"]
mod interpreter;
#[path = "../src/read_budget.rs"]
mod read_budget;

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;

/// The generator's pinned volatile-field constants (see module docs).
const STAMP: &str = "2026-09-13T00:00:00.000+00:00";
const PID: u32 = 3_831_796;

/// Same env-var serialization contract as the other parity binaries:
/// `#[path]`-including a module pulls its `#[cfg(test)] mod tests` in too, so
/// this file's one test shares process-global env vars with `crg_refresh`'s,
/// `read_budget`'s and `context_telemetry`'s own embedded tests and takes
/// their locks alongside this file's own.
static ENV_LOCK: Mutex<()> = Mutex::new(());

static COUNTER: AtomicU32 = AtomicU32::new(0);

struct ScratchDir(PathBuf);

impl ScratchDir {
    fn new(label: &str) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "forge-post-tool-parity-{}-{label}-{n}",
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

/// Every env var the corpus (and the modules under test) may read: set from
/// the case's `env`/kind-specific state, removed otherwise -- a stray
/// inherited value must never leak into a case that does not set it.
const MANAGED: &[&str] = &[
    "CRG_GATE_REPO_ROOT",
    "CRG_DATA_DIR",
    "CRG_GATE_STATE_DIR",
    "READ_BUDGET_STEP_TOKENS",
    "QWEN_PROJECT_DIR",
    "CONTEXT_TELEMETRY_PATH",
    "CLAUDE_PROJECT_DIR",
    "CONDUCTOR_SNAPSHOT_PYTHON",
];

fn clear_managed() {
    for key in MANAGED {
        std::env::remove_var(key);
    }
}

fn apply_case_env(case: &Value) {
    if let Some(env) = case.get("env").and_then(Value::as_object) {
        for (key, value) in env {
            if let Some(text) = value.as_str() {
                std::env::set_var(key, text);
            }
        }
    }
}

/// `#!/bin/sh` + `exec /bin/sleep 30`, executable -- the generator's inert
/// stand-in for the refresh worker.
fn install_sleeper(repo: &Path) {
    let bin = repo.join(".venv/bin");
    std::fs::create_dir_all(&bin).unwrap();
    let stub = bin.join("python");
    std::fs::write(&stub, "#!/bin/sh\nexec /bin/sleep 30\n").unwrap();
    make_executable(&stub);
}

fn install_stub_tool(bin_dir: &Path) {
    std::fs::create_dir_all(bin_dir).unwrap();
    let stub = bin_dir.join("code-review-graph");
    std::fs::write(&stub, "#!/bin/sh\nexit 0\n").unwrap();
    make_executable(&stub);
}

fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut mode = std::fs::metadata(path).unwrap().permissions().mode();
    mode |= 0o111;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

/// The generator's `make_repo`: a directory that reads as a git checkout to
/// `crg_gate`'s `.git/HEAD` lane probe.
fn make_repo(label: &str) -> ScratchDir {
    let repo = ScratchDir::new(&format!("{label}-repo"));
    std::fs::create_dir_all(repo.path().join(".git")).unwrap();
    std::fs::write(repo.path().join(".git/HEAD"), "ref: refs/heads/lane\n").unwrap();
    repo
}

fn read_or_null(path: &Path) -> Value {
    match std::fs::read_to_string(path) {
        Ok(text) => Value::String(text),
        Err(_) => Value::Null,
    }
}

fn ledger_key(session_id: &str) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(session_id.as_bytes()))
}

/// The `graph_bash` arm of `run_case`, extracted only to keep that fn under
/// the 100-line limit: scratch bin dir + store, the adapter's match-then-
/// queue logic, then the frozen `output`/`pending` pair.
fn run_graph_case(
    id: &str,
    payload: &Value,
    seed: &Value,
    tmp_root: &Path,
) -> Vec<(&'static str, Value)> {
    let repo = make_repo(id);
    let bin_dir = tmp_root.join(format!("{id}-bin"));
    if seed.get("stub_tool").and_then(Value::as_bool) == Some(true) {
        install_stub_tool(&bin_dir);
        install_sleeper(repo.path());
    } else {
        std::fs::create_dir_all(&bin_dir).unwrap();
    }
    let store = tmp_root.join(format!("{id}-crgdata"));
    std::fs::create_dir_all(&store).unwrap();
    // The scratch bin dir alone (never the host's PATH) decides whether
    // `code-review-graph` is installed, exactly as the generator pinned it.
    std::env::set_var("PATH", bin_dir.display().to_string());
    std::env::set_var("CRG_DATA_DIR", &store);
    std::env::set_var("CRG_GATE_REPO_ROOT", repo.path());
    // `adapters.post_bash_graph`: quiet unless the command rewrites the git
    // tree, else queue.
    let command = payload
        .get("tool_input")
        .and_then(|input| input.get("command"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let output = if command.is_empty() || !crg_refresh::git_tree_rewrite_matches(command) {
        json!({"hookSpecificOutput": {"hookEventName": "PostToolUse"}})
    } else {
        crg_refresh::full_update_output(repo.path()).unwrap()
    };
    std::env::remove_var("PATH");
    let pending = read_or_null(&store.join("refresh.pending"));
    vec![("output", output), ("pending", pending)]
}

/// The `read_budget` arm of `run_case`, extracted for the same reason: seed
/// ledger, tally, then the frozen `output`/`ledger_after` pair.
fn run_budget_case(
    id: &str,
    payload: &Value,
    seed: &Value,
    tmp_root: &Path,
) -> Vec<(&'static str, Value)> {
    let gate = tmp_root.join(format!("{id}-gate"));
    std::fs::create_dir_all(&gate).unwrap();
    if let Some(ledger) = seed.get("ledger").and_then(Value::as_str) {
        let session = payload["session_id"].as_str().unwrap();
        // The generator seeds `"<total>\n"` -- the tally writer's own
        // format -- and the untouched-seed cases freeze it verbatim.
        std::fs::write(
            gate.join(format!("{}.read-tokens", ledger_key(session))),
            format!("{ledger}\n"),
        )
        .unwrap();
    }
    std::env::set_var("CRG_GATE_STATE_DIR", &gate);
    let output = read_budget::hook_output(payload, &gate).unwrap();
    let ledger_after = match payload.get("session_id").and_then(Value::as_str) {
        Some(session) => read_or_null(&gate.join(format!("{}.read-tokens", ledger_key(session)))),
        None => Value::Null,
    };
    vec![("output", output), ("ledger_after", ledger_after)]
}

/// One case's live computation, returning the same fields the generator
/// froze per kind (as JSON values, so `null` and strings compare uniformly).
fn run_case(case: &Value, tmp_root: &Path) -> Vec<(&'static str, Value)> {
    let id = case["id"].as_str().unwrap();
    let kind = case["kind"].as_str().unwrap();
    let payload = &case["payload"];
    let seed = case.get("seed").cloned().unwrap_or(Value::Null);
    clear_managed();
    apply_case_env(case);

    match kind {
        "report_post" => {
            let store = tmp_root.join(format!("{id}-store"));
            std::fs::create_dir_all(&store).unwrap();
            if let Some(marker) = seed.get("refresh_failed").and_then(Value::as_str) {
                std::fs::write(store.join("refresh.failed"), marker).unwrap();
            }
            std::env::set_var("CRG_DATA_DIR", &store);
            let output = crg_refresh::failure_output("PostToolUse", Path::new("/nonexistent"));
            let failed_after = read_or_null(&store.join("refresh.failed"));
            vec![("output", output), ("failed_after", failed_after)]
        }
        "graph_bash" => run_graph_case(id, payload, &seed, tmp_root),
        "read_budget" => run_budget_case(id, payload, &seed, tmp_root),
        "telemetry_record" => {
            // `_encoded_record` emits the full JSONL line, trailing newline
            // included -- the generator froze exactly that.
            let line = format!("{}\n", context_telemetry::event_record(payload, STAMP, PID));
            vec![("line", Value::String(line))]
        }
        "telemetry_hook_context" => {
            let hook = seed["hook"].as_str().unwrap();
            let hook_json = &seed["hook_json"];
            let session_id = payload["session_id"].as_str().unwrap_or("");
            let line = format!(
                "{}\n",
                context_telemetry::hook_context_record(hook, hook_json, "", session_id, STAMP, PID)
            );
            vec![("line", Value::String(line))]
        }
        "telemetry_path" => {
            let repo = make_repo(id);
            let path = context_telemetry::telemetry_path(repo.path());
            if case
                .get("env")
                .is_some_and(|env| env.get("CONTEXT_TELEMETRY_PATH").is_some())
            {
                // The override case pins the absolute path verbatim.
                vec![("path", Value::String(path.display().to_string()))]
            } else {
                // The default case pins the repo-relative suffix (the
                // checkout root differs per machine), so the inherited
                // `<root>/src/research/tmp/...` shape is what is frozen.
                let suffix = path
                    .strip_prefix(repo.path())
                    .expect("default telemetry path lives under the checkout root");
                vec![("path_suffix", Value::String(suffix.display().to_string()))]
            }
        }
        other => panic!("unknown kind in corpus case {id:?}: {other:?}"),
    }
}

fn load_corpus() -> Vec<Value> {
    let raw = include_str!("fixtures/post_tool_corpus.json");
    serde_json::from_str(raw).expect("post_tool_corpus.json must be valid JSON")
}

fn load_expected() -> std::collections::BTreeMap<String, Value> {
    let raw = include_str!("fixtures/post_tool_expected.json");
    serde_json::from_str(raw).expect("post_tool_expected.json must be valid JSON")
}

#[test]
fn post_tool_use_native_hooks_match_the_frozen_corpus() {
    let _guard = ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let _refresh_guard = crg_refresh::tests::ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let _budget_guard = read_budget::tests::ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let _telemetry_guard = context_telemetry::tests::ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let saved_path = std::env::var_os("PATH");
    clear_managed();

    let corpus = load_corpus();
    // 4 report_post + 4 graph_bash + 5 read_budget + 9 telemetry records +
    // 2 path cases = 24 as authored; the brief demands >= 20 covering all
    // four ported hooks.
    assert!(
        corpus.len() >= 20,
        "expected >= 20 corpus cases covering all four ported hooks, got {}",
        corpus.len()
    );
    let expected = load_expected();
    assert_eq!(
        expected.len(),
        corpus.len(),
        "every corpus case needs exactly one frozen expected verdict"
    );

    let tmp_root = ScratchDir::new("root");
    let mut failures = Vec::new();
    for case in &corpus {
        let id = case["id"].as_str().unwrap();
        let fields = run_case(case, tmp_root.path());
        clear_managed();
        let frozen = expected
            .get(id)
            .unwrap_or_else(|| panic!("no frozen expected verdict for case {id:?}"));
        for (field, live) in &fields {
            let want = frozen
                .get(*field)
                .unwrap_or_else(|| panic!("case {id:?}: frozen verdict lacks {field:?}"));
            if live != want {
                failures.push(format!(
                    "case {id:?} ({field}): rust={live} expected={want}"
                ));
            }
        }
    }

    if let Some(path) = saved_path {
        std::env::set_var("PATH", path);
    }
    assert!(
        failures.is_empty(),
        "{} of {} parity cases disagreed:\n{}",
        failures.len(),
        corpus.len(),
        failures.join("\n")
    );
}
