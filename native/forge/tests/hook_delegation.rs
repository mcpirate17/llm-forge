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

#[test]
fn forwards_stdin_to_the_dispatcher_and_its_allow_exit_code() {
    let project = tempdir();
    stub_project(project.path());
    let payload = r#"{"tool_name":"Bash","tool_input":{"command":"echo hi"}}"#;

    let out = run_forge(project.path(), "PreToolUse", payload, None);

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
