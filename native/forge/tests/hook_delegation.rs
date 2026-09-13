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
    stdin: &str,
    native_hooks: Option<&str>,
) -> std::process::Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_forge"));
    cmd.arg("hook")
        .arg("PreToolUse")
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

    let out = run_forge_with_native_hooks(project.path(), payload, None);

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

    let out = run_forge_with_native_hooks(project.path(), payload, Some(""));

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

    let out = run_forge_with_native_hooks(project.path(), payload, Some("pre_bash"));

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
    let out = run_forge_with_native_hooks(project.path(), payload, Some(""));

    assert_eq!(out.status.code(), Some(0));
    let stdout: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json stdout");
    assert_eq!(stdout["echoed"]["tool_name"], "Bash");
}

#[test]
fn forwards_stderr_output_unchanged() {
    let project = tempdir();
    stub_project(project.path());

    let out = run_forge(project.path(), "PostToolUse", "{}", None);

    assert_eq!(out.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&out.stderr).contains("soft warning on stderr"));
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
