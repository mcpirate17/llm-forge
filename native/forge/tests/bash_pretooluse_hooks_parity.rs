//! Differential parity test for the three Bash `PreToolUse` hooks ported this
//! change: `crg_gate_verify_bash`, `crg_refresh_report_pre`, and
//! `current_work_guard_bash`. Each corpus case is run through BOTH this
//! crate's own native port (in-process, direct function calls) AND the real
//! Python implementation (batched through `tests/fixtures/parity_driver.py`
//! inside the project's `.venv/bin/python`), and the two verdicts must be
//! structurally identical JSON.
//!
//! `crg_gate.py`'s `REPO_ROOT`/`_REPO_CHECKOUT`/`REPO_COMMON_DIR` (and
//! `crg_graph_refresh.py`'s own copy, imported by name) are frozen at
//! *import* time from `CRG_GATE_REPO_ROOT` -- seeing every case's own value
//! correctly requires a fresh import per case; `parity_driver.py`'s module
//! docstring explains how it uses `importlib.reload` to get that without
//! paying for one process per case.
//!
//! `crg_gate.rs`'s own `ownership::load_claims` is filesystem-only (a
//! disclosed, deliberate divergence from Python's `git rev-parse
//! --git-common-dir` subprocess -- see `ownership.rs`'s module docs), so it
//! does not itself need a real Git repository. Python's claim path does, so
//! every `crg_gate_verify_bash` case here still builds one with `git init`:
//! that exercises Python's real code path and costs the Rust side nothing.
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

use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;

/// This file's one `#[test]` fn shares process-global env vars
/// (`CRG_GATE_STATE_DIR`, `CRG_DATA_DIR`, `GOVERNANCE_OWNER`,
/// `LOCAL_AI_RUNTIME`) with `crg_gate.rs`'s and `crg_refresh.rs`'s *own*
/// embedded `#[cfg(test)] mod tests` blocks -- `#[path]`-including a module
/// pulls its test module in too, so those tests run in *this same binary*,
/// on `cargo test`'s default multiple threads, alongside this file's one
/// test. This lock alone does not prevent that race (it only guards this
/// file against a second copy of itself); the test fn additionally takes
/// `crg_gate::tests::ENV_LOCK` and `crg_refresh::tests::ENV_LOCK` -- see the
/// comment at its top -- to serialize against those modules' own tests too.
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

/// A real `git init`'d repo (Python's claim path shells to real `git`; the
/// Rust port never does, but a real repo satisfies both) plus its own
/// process-global-state directory.
struct GitRepo {
    root: ScratchDir,
    state_dir: ScratchDir,
}

impl GitRepo {
    fn new(label: &str) -> Self {
        let root = ScratchDir::new(&format!("{label}-repo"));
        let status = Command::new("git")
            .args(["init", "-q"])
            .current_dir(root.path())
            .status()
            .expect("git init");
        assert!(status.success(), "git init failed for {label}");
        GitRepo {
            root,
            state_dir: ScratchDir::new(&format!("{label}-state")),
        }
    }

    fn common_dir(&self) -> PathBuf {
        crg_gate::checkout_of(self.root.path()).unwrap().1
    }

    fn mark_graph_used(&self, session_id: &str) {
        let p = json!({"session_id": session_id});
        let key = crg_gate_test_state_key(&p);
        std::fs::write(
            self.state_dir.path().join(format!("{key}.graph-used")),
            b"1",
        )
        .unwrap();
    }

    /// Rewrites `.git/HEAD` to a raw commit sha (detached), so Python's
    /// `identity.lane_of` -- which reads this file directly and returns `""`
    /// for anything not shaped `ref: refs/heads/<name>` -- can no longer
    /// derive a branch-name fallback identity. `checkout_of`/`session_checkout`
    /// never read `HEAD`'s content (only whether `.git` exists), so this is
    /// invisible to the Rust side.
    fn detach_head(&self) {
        std::fs::write(
            self.root.path().join(".git/HEAD"),
            "0000000000000000000000000000000000000000\n",
        )
        .unwrap();
    }

    /// Writes one claims store containing `claims`, each
    /// `(owner, paths, created_offset_secs, expires_offset_secs)` relative to
    /// now -- matches the schema `ownership::load_claims` (and Python's
    /// `load_claims`) both require, including the claim id's content-binding
    /// hash (`crg_gate.rs`'s own private test helper establishes the same
    /// pattern; this is its generalisation to more than one claim).
    fn write_claims(&self, claims: &[(&str, &[&str], f64, f64)]) {
        use sha2::{Digest, Sha256};
        let now = instant::now();
        let mut out = Vec::new();
        for (owner, paths, created_off, expires_off) in claims {
            let created = instant::isoformat_utc(now + created_off);
            let expires = instant::isoformat_utc(now + expires_off);
            let fields = json!({
                "owner": owner, "paths": paths, "justification": "because",
                "created_at": created, "expires_at": expires,
            });
            let canonical = serde_json::to_string(&fields).unwrap();
            let digest = Sha256::digest(canonical.as_bytes());
            let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
            out.push(json!({
                "claim_id": format!("claim-{}", &hex[..20]),
                "owner": owner, "paths": paths, "justification": "because",
                "created_at": created, "expires_at": expires,
            }));
        }
        let dir = self.common_dir().join("governance");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("ownership-claims.json"),
            serde_json::to_string(&json!({"schema_version": 1, "claims": out})).unwrap(),
        )
        .unwrap();
    }
}

/// Matches `crg_gate.rs`'s private `state_key` exactly (sha256 hex of the
/// session id) -- duplicated here because that function is not `pub`.
fn crg_gate_test_state_key(payload: &Value) -> String {
    use sha2::{Digest, Sha256};
    let session_id = payload.get("session_id").and_then(Value::as_str).unwrap();
    let digest = Sha256::digest(session_id.as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

fn bash_payload(session_id: &str, command: &str) -> Value {
    json!({"session_id": session_id, "tool_name": "Bash", "tool_input": {"command": command}})
}

/// One case queued for the batched Python driver, paired with the verdict
/// this crate's own native port already produced for it.
struct Pending {
    id: &'static str,
    rust_verdict: Value,
    driver_case: Value,
    /// Keeps a case's scratch repo/dir alive until the Python subprocess (run
    /// after every case is queued) has actually read it.
    _keepalive: Option<Box<dyn std::any::Any>>,
}

fn run_python_batch(cases: &[Value]) -> Vec<Value> {
    let driver = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/parity_driver.py");
    let python = python_bin();
    let mut child = Command::new(&python)
        .arg(&driver)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap_or_else(|e| panic!("failed to launch {python:?} {driver:?}: {e}"));
    child
        .stdin
        .take()
        .unwrap()
        .write_all(serde_json::to_string(cases).unwrap().as_bytes())
        .expect("write parity corpus to python driver stdin");
    let out = child.wait_with_output().expect("wait for python driver");
    assert!(
        out.status.success(),
        "parity_driver.py exited {:?}",
        out.status.code()
    );
    serde_json::from_slice(&out.stdout).expect("parity_driver.py must print one JSON array")
}

/// The project's own `.venv` interpreter -- the same one every other test and
/// CI job in this repo uses, found relative to this crate's manifest without
/// depending on `PATH` or an activated venv.
fn python_bin() -> PathBuf {
    let candidate = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.venv/bin/python");
    assert!(
        candidate.is_file(),
        "expected a project venv at {candidate:?} -- run `uv venv .venv && uv sync` first"
    );
    candidate
}

#[test]
fn bash_pretooluse_native_hooks_match_python_over_a_wide_corpus() {
    let _guard = ENV_LOCK.lock().unwrap();
    // `crg_gate.rs`'s and `crg_refresh.rs`'s own embedded `#[cfg(test)] mod
    // tests` blocks are `#[path]`-included into *this* binary too (they
    // compile into every test binary that pulls those files in -- see the
    // module docs at the top of this file), and their tests mutate the very
    // same `CRG_GATE_STATE_DIR`/`CRG_DATA_DIR` process-global env vars this
    // function does, each serialized only against a `Mutex` private to their
    // own module. `cargo test` runs a binary's `#[test]` fns on multiple
    // threads by default, so without also taking *their* locks here, one of
    // their tests can race a `set_var` against a case in this function's
    // corpus and observe the wrong directory (this raced and failed
    // intermittently before this fix: `crg_gate::tests::
    // an_empty_owner_is_denied_with_the_identity_message` losing the race
    // against a concurrently running gate case here). Holding all three
    // locks for the whole function serializes this corpus against every
    // such test in both modules.
    let _gate_guard = crg_gate::tests::ENV_LOCK.lock().unwrap();
    let _refresh_guard = crg_refresh::tests::ENV_LOCK.lock().unwrap();
    // A stray inherited value from the outer shell must never leak into a
    // case that does not explicitly set it.
    std::env::remove_var("CRG_GATE_STATE_DIR");
    std::env::remove_var("CRG_DATA_DIR");
    std::env::remove_var("GOVERNANCE_OWNER");
    std::env::remove_var("LOCAL_AI_RUNTIME");

    let mut pending: Vec<Pending> = Vec::new();

    // ---- crg_gate_verify_bash -------------------------------------------
    {
        let owner = "llm-b0";
        macro_rules! gate_case {
            ($id:expr, $repo:expr, $session:expr, $owner:expr, $command:expr) => {{
                let payload = bash_payload($session, $command);
                let env = HashMap::new();
                // `crg_gate::state_dir()` reads `CRG_GATE_STATE_DIR` from the
                // real process environment (it is not threaded through as a
                // parameter), exactly like Python's own `_state_dir()` reads
                // it from `os.environ` -- so the in-process Rust call must set
                // it to this case's own scratch dir too, or every case
                // silently shares (and pollutes) the real default
                // `/tmp/claude-crg-gate`.
                std::env::set_var("CRG_GATE_STATE_DIR", $repo.state_dir.path());
                let verdict = crg_gate::verify_bash(
                    &payload,
                    $owner,
                    $repo.root.path(),
                    Some(&$repo.common_dir()),
                    &env,
                );
                pending.push(Pending {
                    id: $id,
                    rust_verdict: verdict,
                    driver_case: json!({
                        "hook": "crg_gate_verify_bash",
                        "payload": payload,
                        "env": {
                            "CRG_GATE_REPO_ROOT": $repo.root.path().to_str().unwrap(),
                            "CRG_GATE_STATE_DIR": $repo.state_dir.path().to_str().unwrap(),
                            "GOVERNANCE_OWNER": $owner,
                        },
                    }),
                    _keepalive: None,
                });
            }};
        }

        // 1. Read-only command: no write targets, always allowed.
        let r = GitRepo::new("gate-readonly");
        gate_case!("gate-readonly", r, "s1", owner, "git status");
        pending.last_mut().unwrap()._keepalive = Some(Box::new(r));

        // 2. Empty command: allowed.
        let r = GitRepo::new("gate-empty-cmd");
        gate_case!("gate-empty-cmd", r, "s1", owner, "");
        pending.last_mut().unwrap()._keepalive = Some(Box::new(r));

        // 3. Write target, but this session never touched the graph: denied.
        let r = GitRepo::new("gate-no-graph-use");
        gate_case!("gate-no-graph-use", r, "s1", owner, "echo hi > out.txt");
        pending.last_mut().unwrap()._keepalive = Some(Box::new(r));

        // 4. Graph used, but no claims at all: denied.
        let r = GitRepo::new("gate-no-claims");
        r.mark_graph_used("s1");
        gate_case!("gate-no-claims", r, "s1", owner, "echo hi > out.txt");
        pending.last_mut().unwrap()._keepalive = Some(Box::new(r));

        // 5. Graph used, exact-path claim covers the single target: allowed.
        let r = GitRepo::new("gate-exact-claim-allows");
        r.mark_graph_used("s1");
        r.write_claims(&[(owner, &["out.txt"], -60.0, 3600.0)]);
        gate_case!(
            "gate-exact-claim-allows",
            r,
            "s1",
            owner,
            "echo hi > out.txt"
        );
        pending.last_mut().unwrap()._keepalive = Some(Box::new(r));

        // 6. Directory-prefix claim covers a nested target: allowed.
        let r = GitRepo::new("gate-prefix-claim-allows");
        r.mark_graph_used("s1");
        r.write_claims(&[(owner, &["docs"], -60.0, 3600.0)]);
        gate_case!(
            "gate-prefix-claim-allows",
            r,
            "s1",
            owner,
            "echo hi > docs/out.txt"
        );
        pending.last_mut().unwrap()._keepalive = Some(Box::new(r));

        // 7. Claim exists but for a disjoint path: denied.
        let r = GitRepo::new("gate-claim-disjoint");
        r.mark_graph_used("s1");
        // No trailing slash: Python's `normalize_claim_path` strips one
        // before re-validating the claim's content-binding hash, so a claim
        // written (and hashed) with one would fail that unrelated check
        // instead of exercising the disjoint-path behavior this case is for.
        r.write_claims(&[(owner, &["other"], -60.0, 3600.0)]);
        gate_case!("gate-claim-disjoint", r, "s1", owner, "echo hi > out.txt");
        pending.last_mut().unwrap()._keepalive = Some(Box::new(r));

        // 8. Claim held by a different owner: denied.
        let r = GitRepo::new("gate-claim-other-owner");
        r.mark_graph_used("s1");
        r.write_claims(&[("someone-else", &["out.txt"], -60.0, 3600.0)]);
        gate_case!(
            "gate-claim-other-owner",
            r,
            "s1",
            owner,
            "echo hi > out.txt"
        );
        pending.last_mut().unwrap()._keepalive = Some(Box::new(r));

        // 9. This owner's own claim has expired: denied (lapsed, not "no claim").
        let r = GitRepo::new("gate-claim-expired");
        r.mark_graph_used("s1");
        r.write_claims(&[(owner, &["out.txt"], -7200.0, -3600.0)]);
        gate_case!("gate-claim-expired", r, "s1", owner, "echo hi > out.txt");
        pending.last_mut().unwrap()._keepalive = Some(Box::new(r));

        // 10. Two targets, only one covered by a claim: denied.
        let r = GitRepo::new("gate-partial-claim");
        r.mark_graph_used("s1");
        r.write_claims(&[(owner, &["a.txt"], -60.0, 3600.0)]);
        gate_case!(
            "gate-partial-claim",
            r,
            "s1",
            owner,
            "for f in a.txt b.txt; do rm \"$f\"; done"
        );
        pending.last_mut().unwrap()._keepalive = Some(Box::new(r));

        // 11. Two targets, both covered by (different) claims: allowed.
        let r = GitRepo::new("gate-both-claimed");
        r.mark_graph_used("s1");
        r.write_claims(&[
            (owner, &["a.txt"], -60.0, 3600.0),
            (owner, &["b.txt"], -60.0, 3600.0),
        ]);
        gate_case!(
            "gate-both-claimed",
            r,
            "s1",
            owner,
            "for f in a.txt b.txt; do rm \"$f\"; done"
        );
        pending.last_mut().unwrap()._keepalive = Some(Box::new(r));

        // 12. sed -i on a claimed file: allowed.
        let r = GitRepo::new("gate-sed-claimed");
        r.mark_graph_used("s1");
        r.write_claims(&[(owner, &["config.toml"], -60.0, 3600.0)]);
        gate_case!(
            "gate-sed-claimed",
            r,
            "s1",
            owner,
            "sed -i 's/a/b/' config.toml"
        );
        pending.last_mut().unwrap()._keepalive = Some(Box::new(r));

        // 13. sed -i on an unclaimed file: denied.
        let r = GitRepo::new("gate-sed-unclaimed");
        r.mark_graph_used("s1");
        gate_case!(
            "gate-sed-unclaimed",
            r,
            "s1",
            owner,
            "sed -i 's/a/b/' config.toml"
        );
        pending.last_mut().unwrap()._keepalive = Some(Box::new(r));

        // 14. tee append to a claimed file: allowed.
        let r = GitRepo::new("gate-tee-claimed");
        r.mark_graph_used("s1");
        r.write_claims(&[(owner, &["log.txt"], -60.0, 3600.0)]);
        gate_case!(
            "gate-tee-claimed",
            r,
            "s1",
            owner,
            "echo hi | tee -a log.txt"
        );
        pending.last_mut().unwrap()._keepalive = Some(Box::new(r));

        // 15. Opaque interpreter write (target unresolvable): denied, distinct message.
        let r = GitRepo::new("gate-opaque");
        r.mark_graph_used("s1");
        gate_case!(
            "gate-opaque",
            r,
            "s1",
            owner,
            "python -c \"open(sys.argv[1],'w').write('x')\""
        );
        pending.last_mut().unwrap()._keepalive = Some(Box::new(r));

        // 16. Interpreter write with a literal path, claimed: allowed.
        let r = GitRepo::new("gate-python-literal-claimed");
        r.mark_graph_used("s1");
        r.write_claims(&[(owner, &["out.txt"], -60.0, 3600.0)]);
        gate_case!(
            "gate-python-literal-claimed",
            r,
            "s1",
            owner,
            "python -c \"open('out.txt','w').write('x')\""
        );
        pending.last_mut().unwrap()._keepalive = Some(Box::new(r));

        // 17. Empty owner (no governance identity): denied before any claim
        // is even consulted. The repo's HEAD must be detached, not just
        // handed `owner=""` on the Rust side -- otherwise Python's own
        // `resolve_owner` (which the parity driver calls independently, the
        // same way the real hook's `main()` does) falls back to the real
        // git branch name of a `git init`'d repo and resolves a legitimate
        // (non-empty) lane identity instead of raising, breaking the very
        // symmetry this case means to test.
        let r = GitRepo::new("gate-empty-owner");
        r.detach_head();
        r.mark_graph_used("s1");
        gate_case!("gate-empty-owner", r, "s1", "", "echo hi > out.txt");
        pending.last_mut().unwrap()._keepalive = Some(Box::new(r));

        // 18. Different session id than the one that used the graph: denied.
        let r = GitRepo::new("gate-wrong-session");
        r.mark_graph_used("s1");
        r.write_claims(&[(owner, &["out.txt"], -60.0, 3600.0)]);
        gate_case!("gate-wrong-session", r, "s2", owner, "echo hi > out.txt");
        pending.last_mut().unwrap()._keepalive = Some(Box::new(r));

        // 19. git apply is a write form: claimed patch target allowed.
        let r = GitRepo::new("gate-git-apply-claimed");
        r.mark_graph_used("s1");
        r.write_claims(&[(owner, &["patch.diff"], -60.0, 3600.0)]);
        gate_case!(
            "gate-git-apply-claimed",
            r,
            "s1",
            owner,
            "git apply patch.diff"
        );
        pending.last_mut().unwrap()._keepalive = Some(Box::new(r));

        // 20. git apply, unclaimed: denied.
        let r = GitRepo::new("gate-git-apply-unclaimed");
        r.mark_graph_used("s1");
        gate_case!(
            "gate-git-apply-unclaimed",
            r,
            "s1",
            owner,
            "git apply patch.diff"
        );
        pending.last_mut().unwrap()._keepalive = Some(Box::new(r));

        // 21. dd write, claimed: allowed.
        let r = GitRepo::new("gate-dd-claimed");
        r.mark_graph_used("s1");
        r.write_claims(&[(owner, &["disk.img"], -60.0, 3600.0)]);
        gate_case!(
            "gate-dd-claimed",
            r,
            "s1",
            owner,
            "dd if=/dev/zero of=disk.img bs=1M"
        );
        pending.last_mut().unwrap()._keepalive = Some(Box::new(r));

        // 22. Append redirect (>>) to an unclaimed file: denied.
        let r = GitRepo::new("gate-append-unclaimed");
        r.mark_graph_used("s1");
        gate_case!(
            "gate-append-unclaimed",
            r,
            "s1",
            owner,
            "cat a.txt >> combined.log"
        );
        pending.last_mut().unwrap()._keepalive = Some(Box::new(r));
    }

    // ---- crg_refresh_report_pre ------------------------------------------
    {
        fn refresh_case(pending: &mut Vec<Pending>, id: &'static str, marker_lines: Option<&str>) {
            let repo = ScratchDir::new(&format!("refresh-{id}"));
            std::fs::create_dir_all(repo.path().join(".git")).unwrap();
            std::fs::write(repo.path().join(".git/HEAD"), "ref: refs/heads/lane\n").unwrap();
            // `take_notices` reads-and-DELETES `refresh.failed` on both sides
            // (it is a consume-once queue), so the Rust in-process call and
            // the later batched Python call must never point at the same
            // directory -- whichever ran first would leave the other reading
            // an already-drained, nonexistent marker. Two directories with
            // identical content give each side its own copy to consume.
            let rust_data_dir = ScratchDir::new(&format!("refresh-data-rust-{id}"));
            let python_data_dir = ScratchDir::new(&format!("refresh-data-python-{id}"));
            if let Some(lines) = marker_lines {
                std::fs::write(rust_data_dir.path().join("refresh.failed"), lines).unwrap();
                std::fs::write(python_data_dir.path().join("refresh.failed"), lines).unwrap();
            }
            // `crg_refresh::store_dir` reads `CRG_DATA_DIR` from the real
            // process environment (like `crg_gate::state_dir` reads
            // `CRG_GATE_STATE_DIR`), so the in-process Rust call must set it
            // to this case's own scratch dir too.
            std::env::set_var("CRG_DATA_DIR", rust_data_dir.path());
            let verdict = crg_refresh::failure_output("PreToolUse", repo.path());
            let case = json!({
                "hook": "crg_refresh_report_pre",
                "payload": {},
                "env": {
                    "CRG_GATE_REPO_ROOT": repo.path().to_str().unwrap(),
                    "CRG_DATA_DIR": python_data_dir.path().to_str().unwrap(),
                },
            });
            pending.push(Pending {
                id,
                rust_verdict: verdict,
                driver_case: case,
                _keepalive: Some(Box::new((repo, rust_data_dir, python_data_dir))),
            });
        }

        refresh_case(&mut pending, "refresh-no-marker", None);
        refresh_case(&mut pending, "refresh-empty-marker", Some(""));
        refresh_case(
            &mut pending,
            "refresh-one-failure",
            Some(r#"{"kind":"failure","paths":["a.py"],"error":"boom"}"#),
        );
        refresh_case(
            &mut pending,
            "refresh-one-warning",
            Some(r#"{"kind":"warning","paths":["b.py"],"text":"slow"}"#),
        );
        refresh_case(
            &mut pending,
            "refresh-failure-then-warning",
            Some(
                "{\"kind\":\"failure\",\"paths\":[\"a.py\"],\"error\":\"boom\"}\n\
                 {\"kind\":\"warning\",\"paths\":[\"b.py\"],\"text\":\"slow\"}\n",
            ),
        );
        refresh_case(
            &mut pending,
            "refresh-two-failures",
            Some(
                "{\"kind\":\"failure\",\"paths\":[\"a.py\"],\"error\":\"boom\"}\n\
                 {\"kind\":\"failure\",\"paths\":[\"c.py\",\"d.py\"],\"error\":\"bang\"}\n",
            ),
        );
        refresh_case(
            &mut pending,
            "refresh-malformed-line-falls-back-to-failure",
            Some("not json at all\n"),
        );
        // NOTE: a "failure" notice missing "paths" entirely, or missing both
        // "text" and "error", is not a realistic input -- the only real
        // writer (`_record` in crg_refresh_state.py, used by
        // `record_failure`/`record_warning`) always writes all three of
        // "kind", "paths" and "text". The former shape crashes the
        // *reference* Python `take_notices` outright (bare `item['paths']`
        // subscript for the "failure" kind, no fallback) rather than
        // disagreeing with it, and the latter exercises a `None` vs. `""`
        // body-default divergence that the real writer never triggers either
        // -- both are read-side tolerance for malformed data, not shapes any
        // native caller ever needs to match byte-for-byte. Test the exact
        // shape the real writer produces instead: "kind"/"paths"/"text" all
        // present, matching `record_failure`'s own `_record(...)` call.
        refresh_case(
            &mut pending,
            "refresh-failure-real-writer-shape",
            Some(
                r#"{"at":1700000000.0,"kind":"failure","paths":["z.py"],"text":"RuntimeError: boom"}"#,
            ),
        );
        refresh_case(
            &mut pending,
            "refresh-default-kind-is-failure",
            Some(r#"{"paths":["a.py"],"error":"boom"}"#),
        );
        refresh_case(
            &mut pending,
            "refresh-text-field-preferred-over-error",
            Some(r#"{"kind":"failure","paths":["a.py"],"text":"t","error":"e"}"#),
        );
        refresh_case(
            &mut pending,
            "refresh-two-warnings",
            Some(
                "{\"kind\":\"warning\",\"paths\":[\"a.py\"],\"text\":\"slow a\"}\n\
                 {\"kind\":\"warning\",\"paths\":[\"b.py\"],\"text\":\"slow b\"}\n",
            ),
        );
        refresh_case(
            &mut pending,
            "refresh-failure-empty-paths",
            Some(r#"{"kind":"failure","paths":[],"error":"boom"}"#),
        );
        refresh_case(
            &mut pending,
            "refresh-combined-multiple-failures-and-warnings",
            Some(
                "{\"kind\":\"failure\",\"paths\":[\"a.py\"],\"error\":\"boom\"}\n\
                 {\"kind\":\"warning\",\"paths\":[\"b.py\"],\"text\":\"slow\"}\n\
                 {\"kind\":\"failure\",\"paths\":[\"c.py\"],\"error\":\"bang\"}\n",
            ),
        );
        refresh_case(
            &mut pending,
            "refresh-warning-empty-paths",
            Some(r#"{"kind":"warning","paths":[],"text":"slow"}"#),
        );
        refresh_case(
            &mut pending,
            "refresh-failure-with-multiple-paths",
            Some(r#"{"kind":"failure","paths":["a.py","b.py"],"error":"boom"}"#),
        );
        refresh_case(
            &mut pending,
            "refresh-warning-then-failure",
            Some(
                "{\"kind\":\"warning\",\"paths\":[\"a.py\"],\"text\":\"slow\"}\n\
                 {\"kind\":\"failure\",\"paths\":[\"b.py\"],\"text\":\"boom\"}\n",
            ),
        );
        refresh_case(
            &mut pending,
            "refresh-single-failure-with-long-text",
            Some(
                r#"{"kind":"failure","paths":["deep/nested/module.py"],"text":"TimeoutError: refresh exceeded 30s while walking the dependency graph"}"#,
            ),
        );
        refresh_case(
            &mut pending,
            "refresh-many-failures-preserves-order",
            Some(
                "{\"kind\":\"failure\",\"paths\":[\"a.py\"],\"text\":\"first\"}\n\
                 {\"kind\":\"failure\",\"paths\":[\"b.py\"],\"text\":\"second\"}\n\
                 {\"kind\":\"failure\",\"paths\":[\"c.py\"],\"text\":\"third\"}\n",
            ),
        );
        refresh_case(
            &mut pending,
            "refresh-warning-then-two-failures",
            Some(
                "{\"kind\":\"warning\",\"paths\":[\"w.py\"],\"text\":\"slow\"}\n\
                 {\"kind\":\"failure\",\"paths\":[\"a.py\"],\"text\":\"boom\"}\n\
                 {\"kind\":\"failure\",\"paths\":[\"b.py\"],\"text\":\"bang\"}\n",
            ),
        );
        refresh_case(
            &mut pending,
            "refresh-failure-and-warning-shared-path",
            Some(
                "{\"kind\":\"failure\",\"paths\":[\"shared.py\"],\"text\":\"boom\"}\n\
                 {\"kind\":\"warning\",\"paths\":[\"shared.py\"],\"text\":\"slow\"}\n",
            ),
        );
    }

    // ---- current_work_guard_bash ------------------------------------------
    {
        fn guard_case(pending: &mut Vec<Pending>, id: &'static str, payload: Value) {
            let verdict = current_work_guard::run(&payload);
            pending.push(Pending {
                id,
                rust_verdict: verdict,
                driver_case: json!({
                    "hook": "current_work_guard_bash",
                    "payload": payload,
                    "env": {},
                }),
                _keepalive: None,
            });
        }
        fn guard_case_env(
            pending: &mut Vec<Pending>,
            id: &'static str,
            payload: Value,
            local_ai_runtime: &str,
        ) {
            std::env::set_var("LOCAL_AI_RUNTIME", local_ai_runtime);
            let verdict = current_work_guard::run(&payload);
            std::env::remove_var("LOCAL_AI_RUNTIME");
            pending.push(Pending {
                id,
                rust_verdict: verdict,
                driver_case: json!({
                    "hook": "current_work_guard_bash",
                    "payload": payload,
                    "env": {"LOCAL_AI_RUNTIME": local_ai_runtime},
                }),
                _keepalive: None,
            });
        }

        guard_case(
            &mut pending,
            "guard-edit-current-work-denied",
            json!({"tool_name": "Edit", "tool_input": {"file_path": "/repo/.current_work.md"}}),
        );
        guard_case(
            &mut pending,
            "guard-write-current-work-denied",
            json!({"tool_name": "Write", "tool_input": {"file_path": ".current_work.md"}}),
        );
        guard_case(
            &mut pending,
            "guard-read-current-work-denied",
            json!({"tool_name": "Read", "tool_input": {"file_path": "notes/.current_work.md"}}),
        );
        guard_case(
            &mut pending,
            "guard-notebookedit-current-work-denied",
            json!({"tool_name": "NotebookEdit", "tool_input": {"path": ".current_work.md"}}),
        );
        guard_case(
            &mut pending,
            "guard-edit-other-file-allowed",
            json!({"tool_name": "Edit", "tool_input": {"file_path": "src/main.py"}}),
        );
        guard_case(
            &mut pending,
            "guard-bash-cat-denied",
            bash_payload("s1", "cat .current_work.md"),
        );
        guard_case(
            &mut pending,
            "guard-bash-head-denied",
            bash_payload("s1", "head -n5 .current_work.md"),
        );
        guard_case(
            &mut pending,
            "guard-bash-echo-mention-allowed",
            bash_payload("s1", "echo see .current_work.md"),
        );
        guard_case(
            &mut pending,
            "guard-bash-grep-pattern-after-target-allowed",
            bash_payload("s1", "grep foo .current_work.md"),
        );
        guard_case(
            &mut pending,
            "guard-bash-grep-pattern-before-target-denied",
            bash_payload("s1", "grep -f .current_work.md foo.txt"),
        );
        guard_case(
            &mut pending,
            "guard-bash-unrelated-command-allowed",
            bash_payload("s1", "ls -la"),
        );
        guard_case(
            &mut pending,
            "guard-bash-sed-current-work-denied",
            bash_payload("s1", "sed -n 1p .current_work.md"),
        );
        guard_case(
            &mut pending,
            "guard-bash-mv-current-work-denied",
            bash_payload("s1", "mv .current_work.md backup.md"),
        );
        guard_case(
            &mut pending,
            "guard-shell-tool-alias-denied",
            json!({"tool_name": "shell", "tool_input": {"command": "cat .current_work.md"}}),
        );
        guard_case(
            &mut pending,
            "guard-grok-protocol-denied",
            json!({
                "toolName": "Bash",
                "toolInput": {"command": "cat .current_work.md"},
            }),
        );
        guard_case(
            &mut pending,
            "guard-grok-protocol-allowed",
            json!({"toolName": "Bash", "toolInput": {"command": "echo hi"}}),
        );
        guard_case(
            &mut pending,
            "guard-mutation-hint-advisory-on-new-test-file",
            json!({"tool_name": "Write", "tool_input": {"file_path": "src/foo_test.py"}}),
        );
        guard_case(
            &mut pending,
            "guard-no-advisory-on-non-test-new-file",
            json!({"tool_name": "Write", "tool_input": {"file_path": "src/foo.py"}}),
        );
        guard_case_env(
            &mut pending,
            "guard-local-ai-runtime-off-shell-echo-allowed",
            bash_payload("s1", "echo hi"),
            "0",
        );
        guard_case_env(
            &mut pending,
            "guard-local-ai-runtime-on-plain-command-allowed",
            bash_payload("s1", "ls -la"),
            "1",
        );
        guard_case(
            &mut pending,
            "guard-run-shell-command-alias-current-work-denied",
            json!({
                "tool_name": "run_shell_command",
                "tool_input": {"command": "tail .current_work.md"},
            }),
        );
        guard_case(&mut pending, "guard-empty-payload-allowed", json!({}));
    }

    // Every case above that needed `CRG_GATE_STATE_DIR`/`CRG_DATA_DIR` ran its
    // Rust side inline while building `pending`, against a `ScratchDir` this
    // function still owns; nothing past this point reads those vars again.
    // Left set, they point at directories `ScratchDir::Drop` removes at the
    // end of this scope -- and, worse, they leak past this function's locks
    // to whichever `crg_gate::tests::*`/`crg_refresh::tests::*` test runs
    // next once `_gate_guard`/`_refresh_guard` release, making it read the
    // wrong (or a since-deleted) store directory instead of its own. That
    // observed, intermittently: `crg_refresh::tests::*` panicking on a
    // `Value::Null` it didn't expect because `CRG_DATA_DIR` was still
    // pointing at this function's last `refresh_case`'s scratch dir.
    std::env::remove_var("CRG_GATE_STATE_DIR");
    std::env::remove_var("CRG_DATA_DIR");
    std::env::remove_var("GOVERNANCE_OWNER");
    std::env::remove_var("LOCAL_AI_RUNTIME");

    assert!(
        pending.len() >= 64,
        "expected at least 20 cases per hook (60+ total; 22 gate + 20 refresh \
         + 22 guard = 64 as authored), got {}",
        pending.len()
    );

    let driver_cases: Vec<Value> = pending.iter().map(|p| p.driver_case.clone()).collect();
    let python_verdicts = run_python_batch(&driver_cases);
    assert_eq!(
        python_verdicts.len(),
        pending.len(),
        "parity_driver.py must return exactly one verdict per case"
    );

    let mut failures = Vec::new();
    for (case, python_verdict) in pending.iter().zip(python_verdicts.iter()) {
        if &case.rust_verdict != python_verdict {
            failures.push(format!(
                "case {:?}: rust={} python={}",
                case.id, case.rust_verdict, python_verdict
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} parity cases disagreed:\n{}",
        failures.len(),
        pending.len(),
        failures.join("\n")
    );
}
