//! Integration tests for `forge hook <Event>`'s delegation to a Python dispatcher.
//!
//! `tests/fixtures/stub_dispatch.py` stands in for the real
//! `tooling.hooks.dispatch` module -- installed as a fake project's
//! `.venv/bin/python` so `interpreter::resolve_python` finds it exactly the way it
//! would find a real venv. This proves the forwarding contract (stdin passthrough,
//! stdout/stderr passthrough, exit codes 0/2/nonzero, telemetry) without needing a
//! real Python interpreter or the actual `tooling` package installed.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn tempdir() -> TempDir {
    let base = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    let unique = format!(
        "forge-hook-test-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    );
    let path = base.join(unique);
    fs::create_dir_all(&path).expect("create tempdir");
    TempDir(path)
}

/// Installs `tests/fixtures/stub_dispatch.py` as `<project>/.venv/bin/python`.
fn stub_project(project: &Path) {
    let venv_bin = project.join(".venv").join("bin");
    fs::create_dir_all(&venv_bin).expect("create fake .venv/bin");
    let stub_src = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/stub_dispatch.py");
    let dest = venv_bin.join("python");
    fs::copy(&stub_src, &dest).expect("install stub dispatcher");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&dest).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&dest, perms).unwrap();
    }
}

fn run_forge(
    project: &Path,
    event: &str,
    stdin: &str,
    telemetry_path: Option<&Path>,
) -> std::process::Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_forge"));
    cmd.arg("hook")
        .arg(event)
        .env("CLAUDE_PROJECT_DIR", project)
        // The stub venv below is the interpreter this test is about; a snapshot
        // export leaking in from the mutation engine's environment would rank
        // above it and run the host's python instead of the stub.
        .env_remove("CONDUCTOR_SNAPSHOT_PYTHON")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    match telemetry_path {
        Some(p) => {
            cmd.env("CONTEXT_TELEMETRY_PATH", p);
        }
        None => {
            cmd.env_remove("CONTEXT_TELEMETRY_PATH");
        }
    }
    let mut child = cmd.spawn().expect("spawn forge");
    child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(stdin.as_bytes())
        .expect("write stdin");
    child.wait_with_output().expect("wait for forge")
}

/// Like `run_forge`, but also controls `FORGE_NATIVE_HOOKS` on forge's own
/// process: `Some(v)` sets it to `v` (use `Some("")` for the escape hatch),
/// `None` removes it from the child's environment entirely so no leftover
/// value from the test runner's own shell can leak in.
fn run_forge_with_native_hooks(
    project: &Path,
    event: &str,
    stdin: &str,
    native_hooks: Option<&str>,
) -> std::process::Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_forge"));
    cmd.arg("hook")
        .arg(event)
        .env("CLAUDE_PROJECT_DIR", project)
        .env_remove("CONTEXT_TELEMETRY_PATH")
        .env_remove("CONDUCTOR_SNAPSHOT_PYTHON")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    match native_hooks {
        Some(v) => {
            cmd.env("FORGE_NATIVE_HOOKS", v);
        }
        None => {
            cmd.env_remove("FORGE_NATIVE_HOOKS");
        }
    }
    let mut child = cmd.spawn().expect("spawn forge");
    child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(stdin.as_bytes())
        .expect("write stdin");
    child.wait_with_output().expect("wait for forge")
}

/// State 1 of 3 required by the port: **all-native**. `FORGE_NATIVE_HOOKS`
/// unset entirely defaults to every Bash `PreToolUse` hook the Python
/// registry lists (`handlers::BASH_PRETOOLUSE_HOOK_NAMES`) plus
/// `bash_write_targets`, so `bash_pretooluse_fully_native` is true and
/// `dispatch::run_hook` must answer the merged verdict itself and never
/// start Python for this event -- proven here by the stub dispatcher never
/// running at all: were it invoked, stdout would carry its `"echoed"` /
/// `"forge_native_hooks"` shape instead of forge's own
/// `hookSpecificOutput` shape.
#[test]
fn default_native_coverage_answers_bash_pretooluse_without_starting_python() {
    let project = tempdir();
    stub_project(project.path());
    let payload = r#"{"tool_name":"Bash","tool_input":{"command":"rm -rf /"}}"#;

    let out = run_forge_with_native_hooks(project.path(), "PreToolUse", payload, None);

    assert_eq!(out.status.code(), Some(0), "a deny verdict still exits 0");
    let stdout: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json stdout");
    assert!(
        stdout.get("echoed").is_none(),
        "the stub dispatcher must never have run: {stdout}"
    );
    assert_eq!(
        stdout["hookSpecificOutput"]["permissionDecision"], "deny",
        "forge's own merged verdict: {stdout}"
    );
}

/// State 2 of 3: **partial coverage** -- one of the four Bash `PreToolUse`
/// hooks the Python registry lists (`current_work_guard_bash` here) is *not*
/// in the opted-in native set, whether because forge does not know it or the
/// operator excluded it. `bash_pretooluse_fully_native` must be false, so
/// Python still starts, but only for the hooks forge did not already answer
/// -- the three it did answer are spliced in via `FORGE_NATIVE_HOOKS` /
/// `FORGE_NATIVE_ANSWERS` rather than re-run.
#[test]
fn partial_native_coverage_still_starts_python_for_the_remaining_hook() {
    let project = tempdir();
    stub_project(project.path());
    let payload = r#"{"tool_name":"Bash","tool_input":{"command":"echo hi"}}"#;

    let out = run_forge_with_native_hooks(
        project.path(),
        "PreToolUse",
        payload,
        Some("crg_refresh_report_pre,crg_gate_verify_bash,pre_bash"),
    );

    assert_eq!(out.status.code(), Some(0), "Python still runs and allows");
    let stdout: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json stdout");
    assert_eq!(
        stdout["echoed"]["tool_name"], "Bash",
        "the stub dispatcher must have run: {stdout}"
    );
    let served: std::collections::HashSet<&str> = stdout["forge_native_hooks"]
        .as_str()
        .expect("forge_native_hooks is a string")
        .split(',')
        .collect();
    assert_eq!(
        served,
        std::collections::HashSet::from([
            "crg_refresh_report_pre",
            "crg_gate_verify_bash",
            "pre_bash"
        ]),
        "current_work_guard_bash was left for Python to answer itself"
    );
    let answers: serde_json::Value = serde_json::from_str(
        stdout["forge_native_answers"]
            .as_str()
            .expect("answers json"),
    )
    .expect("valid json");
    assert_eq!(
        answers["pre_bash"]["hookSpecificOutput"]["permissionDecision"],
        "allow"
    );
    assert!(answers.get("current_work_guard_bash").is_none());
}

/// State 3 of 3: `FORGE_NATIVE_HOOKS=""` is the documented escape hatch back
/// to the pre-port, all-Python behaviour.
#[test]
fn empty_native_hooks_is_the_escape_hatch_back_to_all_python() {
    let project = tempdir();
    stub_project(project.path());
    let payload = r#"{"tool_name":"Bash","tool_input":{"command":"rm -rf /"}}"#;

    let out = run_forge_with_native_hooks(project.path(), "PreToolUse", payload, Some(""));

    assert_eq!(out.status.code(), Some(0));
    let stdout: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json stdout");
    assert_eq!(stdout["forge_native_hooks"], "");
    assert_eq!(stdout["forge_native_answers"], "");
}

#[test]
fn a_stale_env_native_hooks_value_never_leaks_past_a_non_bash_payload() {
    let project = tempdir();
    stub_project(project.path());
    // Simulates a caller whose shell still has FORGE_NATIVE_HOOKS from an
    // earlier Bash call set when this call is for a different tool: forge
    // must still override it to "" for the child rather than let Python
    // believe `pre_bash` was served when no answer exists for it.
    let payload = r#"{"tool_name":"Write","tool_input":{"file_path":"x","content":"y"}}"#;

    let out = run_forge_with_native_hooks(project.path(), "PreToolUse", payload, Some("pre_bash"));

    assert_eq!(out.status.code(), Some(0));
    let stdout: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json stdout");
    assert_eq!(stdout["forge_native_hooks"], "");
    assert_eq!(stdout["forge_native_answers"], "");
}

#[test]
fn forwards_stdin_to_the_dispatcher_and_its_allow_exit_code() {
    let project = tempdir();
    stub_project(project.path());
    let payload = r#"{"tool_name":"Bash","tool_input":{"command":"echo hi"}}"#;

    // Forces the escape hatch: this test's purpose is the generic
    // stdin/exit-code forwarding contract, not nativity, and the default
    // native coverage would otherwise answer `echo hi` itself and never
    // start the stub dispatcher this test inspects.
    let out = run_forge_with_native_hooks(project.path(), "PreToolUse", payload, Some(""));

    assert_eq!(out.status.code(), Some(0));
    let stdout: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json stdout");
    assert_eq!(stdout["echoed"]["tool_name"], "Bash");
}

#[test]
fn forwards_stderr_output_unchanged() {
    let project = tempdir();
    stub_project(project.path());

    // The escape hatch keeps this test about the generic stderr-forwarding
    // contract: with defaults, a PostToolUse payload without a `tool_name`
    // is fully native now (both matcher-`.*` names ported) and never starts
    // the stub dispatcher at all.
    let out = run_forge_with_native_hooks(project.path(), "PostToolUse", "{}", Some(""));

    assert_eq!(out.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&out.stderr).contains("soft warning on stderr"));
}

/// The PostToolUse twin of
/// `default_native_coverage_answers_bash_pretooluse_without_starting_python`:
/// a Read call's every registry-matched hook is native by default, so the
/// stub dispatcher must never run -- proven by its `"ok"` stdout shape never
/// appearing.
#[test]
fn default_native_coverage_answers_post_tooluse_without_starting_python() {
    let project = tempdir();
    stub_project(project.path());
    let payload = r#"{"session_id":"s-1","tool_name":"Read","tool_input":{"file_path":"a.py"},"tool_response":{"type":"text","text":"one line"}}"#;

    let out = run_forge_with_native_hooks(project.path(), "PostToolUse", payload, None);

    assert_eq!(out.status.code(), Some(0));
    let stdout: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json stdout");
    assert!(
        stdout.get("ok").is_none(),
        "the stub dispatcher must never have run: {stdout}"
    );
    assert_eq!(stdout["hookSpecificOutput"]["hookEventName"], "PostToolUse");
    // A short response crosses no budget step and bounds nothing: bare.
    assert!(stdout["hookSpecificOutput"]
        .get("additionalContext")
        .is_none());
}

/// An Edit call's every registry-matched hook (`crg_refresh_report_post`,
/// `crg_graph_refresh`, `post_edit`, `obsidian_post_edit`,
/// `context_telemetry`) is native by default now, so the stub dispatcher
/// must never run -- the edit-family twin of the Read test above. The
/// obsidian body appends its accumulator line for the session id, so the
/// payload uses a dedicated id and the line is cleaned up after.
#[test]
fn an_edit_tool_post_tooluse_is_fully_native_by_default() {
    let project = tempdir();
    stub_project(project.path());
    let payload = r#"{"session_id":"s-edit-native","tool_name":"Edit","tool_input":{"file_path":"a.py","old_string":"x","new_string":"y"},"tool_response":{"filePath":"a.py"}}"#;

    let out = run_forge_with_native_hooks(project.path(), "PostToolUse", payload, None);

    assert_eq!(out.status.code(), Some(0));
    let stdout: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json stdout");
    assert!(
        stdout.get("ok").is_none(),
        "the stub dispatcher must never have run: {stdout}"
    );
    assert_eq!(stdout["hookSpecificOutput"]["hookEventName"], "PostToolUse");
    let _ = std::fs::remove_file("/tmp/claude-session-journal/s-edit-native.tsv");
}

/// With a partial opt-in, an Edit call delegates the names not opted in and
/// gets the opted-in, matcher-eligible ones' precomputed answers spliced
/// back in via the env pair, exactly like a partially-native Bash
/// `PreToolUse` call -- here the two matcher-`.*` names
/// (`crg_refresh_report_post`, `context_telemetry`), with the three
/// edit-family names left to Python.
#[test]
fn an_edit_tool_post_tooluse_splices_the_wildcard_names_on_partial_opt_in() {
    let project = tempdir();
    stub_project(project.path());
    let payload = r#"{"session_id":"s-2","tool_name":"Edit","tool_input":{"file_path":"a.py","old_string":"x","new_string":"y"},"tool_response":{"filePath":"a.py"}}"#;

    let out = run_forge_with_native_hooks(
        project.path(),
        "PostToolUse",
        payload,
        Some("crg_refresh_report_post,context_telemetry"),
    );

    assert_eq!(out.status.code(), Some(0));
    let stdout: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json stdout");
    assert_eq!(
        stdout["ok"],
        serde_json::json!(true),
        "the stub ran: {stdout}"
    );
    let served: std::collections::HashSet<&str> = stdout["forge_native_hooks"]
        .as_str()
        .expect("forge_native_hooks is a string")
        .split(',')
        .collect();
    assert_eq!(
        served,
        std::collections::HashSet::from(["crg_refresh_report_post", "context_telemetry"]),
        "exactly the matcher-.* names were answered natively"
    );
}

#[test]
fn forwards_the_deny_exit_code() {
    let project = tempdir();
    stub_project(project.path());

    let out = run_forge(project.path(), "Deny", "{}", None);

    assert_eq!(out.status.code(), Some(2));
    let stdout: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json stdout");
    assert_eq!(stdout["decision"], "block");
}

#[test]
fn forwards_a_nonzero_non_deny_exit_code() {
    let project = tempdir();
    stub_project(project.path());

    let out = run_forge(project.path(), "Explode", "{}", None);

    assert_eq!(out.status.code(), Some(7));
    assert!(String::from_utf8_lossy(&out.stderr).contains("fatal"));
}

#[test]
fn writes_one_telemetry_line_when_the_env_var_is_set() {
    let project = tempdir();
    stub_project(project.path());
    let telemetry = project.path().join("telemetry/events.jsonl");

    let out = run_forge(project.path(), "PreToolUse", "{}", Some(&telemetry));

    assert_eq!(out.status.code(), Some(0));
    let contents = fs::read_to_string(&telemetry).expect("telemetry file written");
    let mut lines = contents.lines();
    let line = lines.next().expect("one telemetry line");
    assert!(
        lines.next().is_none(),
        "expected exactly one line, got: {contents:?}"
    );
    let record: serde_json::Value = serde_json::from_str(line).expect("telemetry line is json");
    assert_eq!(record["event"], "PreToolUse");
    assert_eq!(record["delegated"], true);
    assert!(
        record["elapsed_ms"]
            .as_f64()
            .expect("elapsed_ms is a number")
            >= 0.0
    );
    assert!(record["ts"].as_str().expect("ts is a string").contains('T'));
}

#[test]
fn skips_telemetry_entirely_when_the_env_var_is_unset() {
    let project = tempdir();
    stub_project(project.path());
    let telemetry = project.path().join("telemetry/events.jsonl");

    let out = run_forge(project.path(), "PreToolUse", "{}", None);

    assert_eq!(out.status.code(), Some(0));
    assert!(!telemetry.exists());
}

/// `SessionStart` has no fully-native path: `forge` must read the payload,
/// compute the opted-in `workspace_exposure_session` answer and hand it to the
/// Python dispatcher as splice env, then propagate the dispatcher's exit code.
#[test]
fn session_start_delegates_and_carries_the_exposure_answer() {
    let project = tempdir();
    stub_project(project.path());
    let payload = r#"{"session_id":"stub-session"}"#;

    let out = run_forge(project.path(), "SessionStart", payload, None);

    assert_eq!(out.status.code(), Some(0), "delegation exits 0");
    let stdout: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("stub dispatcher ran");
    assert_eq!(stdout["event"], "SessionStart");
    let answers = stdout["forge_native_answers"].as_str().unwrap_or("");
    let parsed: serde_json::Value = serde_json::from_str(answers)
        .unwrap_or_else(|err| panic!("answers must be JSON ({err}): {answers}"));
    assert!(
        parsed.get("workspace_exposure_session").is_some(),
        "the exposure answer must be spliced in: {answers}"
    );
}

/// Events with no native handlers at all (`SessionEnd` today) take the bare
/// delegation arm: payload forwarded, no native env claimed beyond the
/// documented empty escape hatch. Since design step 6 the native SessionEnd
/// handler runs its ledger rollup first -- this payload carries no
/// `transcript_path`, so the rollup is a stderr note and nothing else.
#[test]
fn events_without_native_handlers_still_delegate() {
    let project = tempdir();
    stub_project(project.path());
    let payload = r#"{"reason":"clear"}"#;

    let out = run_forge(project.path(), "SessionEnd", payload, None);

    assert_eq!(out.status.code(), Some(0));
    let stdout: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("stub dispatcher ran");
    assert_eq!(stdout["event"], "SessionEnd");
    assert_eq!(
        stdout["forge_native_hooks"].as_str(),
        Some(""),
        "no native hooks are claimed for an event forge does not answer"
    );
    assert_eq!(stdout["echoed"]["reason"], "clear");
}

/// Design step 6, end to end: `forge hook SessionEnd` rolls the ending
/// session's own transcript (the payload's `transcript_path`) into the ledger
/// via a 2 s-bounded `forge ledger rollup` child, then still delegates the
/// event -- the rollup writes under `LEDGER_ROOT` and the stub dispatcher's
/// echo proves both halves ran, in that order, with one process exit code 0.
#[test]
fn session_end_rolls_the_ending_session_into_the_ledger() {
    let project = tempdir();
    stub_project(project.path());
    let transcript = project.path().join("transcript.jsonl");
    std::fs::write(
        &transcript,
        concat!(
            r#"{"uuid":"u1","session_id":"sess-end-1","timestamp":"2026-09-13T00:00:00Z","type":"assistant","message":{"role":"assistant","model":"m","usage":{"input_tokens":1,"output_tokens":1,"cache_read_input_tokens":0,"cache_creation_input_tokens":0},"content":[{"type":"text","text":"x"}]}}"#,
            "\n"
        ),
    )
    .expect("write transcript");
    let ledger_root = tempdir();

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_forge"));
    cmd.arg("hook")
        .arg("SessionEnd")
        .env("CLAUDE_PROJECT_DIR", project.path())
        .env("LEDGER_ROOT", ledger_root.path())
        .env_remove("CONTEXT_TELEMETRY_PATH")
        .env_remove("CONDUCTOR_SNAPSHOT_PYTHON")
        .env_remove("FORGE_NATIVE_HOOKS")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let payload = format!(
        r#"{{"session_id":"s","reason":"clear","transcript_path":"{}"}}"#,
        transcript.display()
    );
    let mut child = cmd.spawn().expect("spawn forge");
    child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(payload.as_bytes())
        .expect("write stdin");
    let out = child.wait_with_output().expect("wait for forge");

    assert_eq!(
        out.status.code(),
        Some(0),
        "the hook still exits 0 whatever the rollup did: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let rollup = ledger_root
        .path()
        .join("session_rollup")
        .join("2026-09-13.jsonl");
    let text = std::fs::read_to_string(&rollup)
        .unwrap_or_else(|err| panic!("SessionEnd rollup missing ({err}): {}", rollup.display()));
    assert!(
        text.contains(r#""session_id":"sess-end-1""#),
        "the ending session's own row: {text}"
    );
    let stdout: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("stub dispatcher ran after the rollup");
    assert_eq!(stdout["event"], "SessionEnd");
    assert_eq!(stdout["echoed"]["session_id"], "s");
}

/// An empty `CLAUDE_PROJECT_DIR` must fall back to the working directory (the
/// `Path::cwd` arm of `interpreter::project_root`), not to an empty path that
/// resolves no venv: run forge from inside the stub project with the variable
/// set but empty and the stub must still be the dispatcher that answers.
#[test]
fn an_empty_project_dir_falls_back_to_the_working_directory() {
    let project = tempdir();
    stub_project(project.path());
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_forge"));
    cmd.arg("hook")
        .arg("SessionEnd")
        .env("CLAUDE_PROJECT_DIR", "")
        .current_dir(project.path())
        .env_remove("CONTEXT_TELEMETRY_PATH")
        .env_remove("CONDUCTOR_SNAPSHOT_PYTHON")
        .env_remove("FORGE_NATIVE_HOOKS")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("spawn forge");
    child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(br#"{}"#)
        .expect("write stdin");
    let out = child.wait_with_output().expect("wait for forge");

    assert_eq!(out.status.code(), Some(0), "cwd-rooted delegation exits 0");
    let stdout: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("stub dispatcher ran");
    assert_eq!(stdout["event"], "SessionEnd");
    // The dispatcher must also see the fallback root: forge re-exports
    // CLAUDE_PROJECT_DIR from project_root(), so an empty-variable bug would
    // arrive here as the empty string.
    let echoed = stdout["claude_project_dir"].as_str().unwrap_or_default();
    assert_eq!(
        std::fs::canonicalize(echoed).ok(),
        std::fs::canonicalize(project.path()).ok(),
        "project_root must fall back to the working directory"
    );
}

/// A set-but-empty `CONDUCTOR_SNAPSHOT_PYTHON` is not an export: the filter in
/// `interpreter::resolve_python` must drop it and let the project venv (the
/// stub) win.
#[test]
fn an_empty_snapshot_export_still_lets_the_project_venv_win() {
    let project = tempdir();
    stub_project(project.path());
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_forge"));
    cmd.arg("hook")
        .arg("SessionEnd")
        .env("CLAUDE_PROJECT_DIR", project.path())
        .env("CONDUCTOR_SNAPSHOT_PYTHON", "")
        .env_remove("CONTEXT_TELEMETRY_PATH")
        .env_remove("FORGE_NATIVE_HOOKS")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("spawn forge");
    child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(br#"{}"#)
        .expect("write stdin");
    let out = child.wait_with_output().expect("wait for forge");

    assert_eq!(out.status.code(), Some(0));
    let stdout: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("stub dispatcher ran");
    assert_eq!(stdout["event"], "SessionEnd");
}

/// `FORGE_HOOK_STANDALONE=1` (`docs/routing.md`'s "Warn-only mode and
/// standalone install" section): a `PreToolUse` `Bash` payload -- no
/// `agent_id`, so the live cap check is a fast-path `NoOp`, and `tool_name`
/// isn't `Agent`, so routing has nothing to say either -- must produce
/// empty stdout and exit 0, never forge's own native Bash-guard verdict
/// (contrast `default_native_coverage_answers_bash_pretooluse_without_
/// starting_python` above, which is the same payload *without* the flag and
/// gets a real `deny` verdict back).
///
/// This also proves no Python child is spawned, but only by construction,
/// not by a dynamic sentinel: `dispatch::run_pre_tool_use_standalone`'s
/// only branches are the live cap check, the `Agent` routing branch, and a
/// bare `Ok(0)` fallthrough -- none of the three calls `interpreter::
/// resolve_python` or spawns a `Command` on this payload's path. A wrapper
/// `.venv/bin/python` that flips a marker file would exercise exactly the
/// same static fact this comment already states from reading the source,
/// since standalone's `PreToolUse` arm has no code path that reaches
/// `delegate()` at all -- so it is not built here; this is the documented
/// limitation the brief allows in place of a dynamic "no spawn" proof.
#[test]
fn standalone_mode_silences_a_bash_payload_it_has_no_opinion_on() {
    let project = tempdir();
    stub_project(project.path());
    let payload = r#"{"tool_name":"Bash","tool_input":{"command":"rm -rf /"}}"#;

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_forge"));
    cmd.arg("hook")
        .arg("PreToolUse")
        .env("CLAUDE_PROJECT_DIR", project.path())
        .env("FORGE_HOOK_STANDALONE", "1")
        .env_remove("CONTEXT_TELEMETRY_PATH")
        .env_remove("CONDUCTOR_SNAPSHOT_PYTHON")
        .env_remove("FORGE_NATIVE_HOOKS")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("spawn forge");
    child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(payload.as_bytes())
        .expect("write stdin");
    let out = child.wait_with_output().expect("wait for forge");

    assert_eq!(out.status.code(), Some(0));
    assert!(
        out.stdout.is_empty(),
        "standalone must print nothing for a payload it has no opinion on: {:?}",
        String::from_utf8_lossy(&out.stdout)
    );
}

/// The `Agent` half of standalone mode: routing still runs and answers on
/// its own, with no `crg_refresh_report_pre` branch merged in (standalone
/// never runs the native Bash-guard/report-refresh branches).
#[test]
fn standalone_mode_still_answers_an_agent_payload_with_routing() {
    let project = tempdir();
    stub_project(project.path());
    let payload = r#"{"tool_name":"Agent","tool_input":{"subagent_type":"Explore"}}"#;

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_forge"));
    cmd.arg("hook")
        .arg("PreToolUse")
        .env("CLAUDE_PROJECT_DIR", project.path())
        .env("FORGE_HOOK_STANDALONE", "1")
        .env_remove("FORGE_MODE")
        .env_remove("CONTEXT_TELEMETRY_PATH")
        .env_remove("CONDUCTOR_SNAPSHOT_PYTHON")
        .env_remove("FORGE_NATIVE_HOOKS")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("spawn forge");
    child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(payload.as_bytes())
        .expect("write stdin");
    let out = child.wait_with_output().expect("wait for forge");

    assert_eq!(out.status.code(), Some(0));
    let stdout: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json stdout");
    assert_eq!(
        stdout["hookSpecificOutput"]["permissionDecision"], "allow",
        "Explore routes to an allow with a model: {stdout}"
    );
    assert!(
        stdout.get("echoed").is_none(),
        "the stub dispatcher must never have run: {stdout}"
    );
}
