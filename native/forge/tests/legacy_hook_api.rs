//! Old Python body APIs now call these same Rust kernels through `forge`.
//! These cases cover the CLI wire format and its side effects, while the
//! module tests and frozen corpora cover each kernel's full behavior.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "forge-legacy-{label}-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

fn call(op: &str, input: &Value, env: &[(&str, &str)]) -> (Value, String) {
    use std::io::Write;
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_forge"));
    cmd.args(["legacy-hook", op]);
    for (key, value) in env {
        cmd.env(key, value);
    }
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.to_string().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{op}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    (
        serde_json::from_slice(&output.stdout).unwrap(),
        String::from_utf8(output.stderr).unwrap(),
    )
}

#[test]
fn write_targets_and_guard_share_the_native_shell_parser() {
    let command = "echo 'git push --force' > report.txt; git push --force-with-lease";
    let (targets, _) = call(
        "repo-write-targets",
        &json!({"command": command, "repo_root": "/repo"}),
        &[],
    );
    assert_eq!(targets, json!(["report.txt"]));
    let (reason, _) = call("guard-check", &json!({"command": command}), &[]);
    assert_eq!(reason, Value::Null);
    let (reason, _) = call(
        "guard-check",
        &json!({"command": "git reset --hard HEAD"}),
        &[],
    );
    assert!(reason.as_str().unwrap().contains("reset --hard"));
    let (commands, _) = call(
        "split-commands",
        &json!({"tokens": ["echo", "hi", ";", "touch", "x"]}),
        &[],
    );
    assert_eq!(commands, json!([["echo", "hi"], ["touch", "x"]]));
}

#[test]
fn impact_cli_reports_native_tier_and_context() {
    let (tier, _) = call("impact-classify", &json!({"command": "ls -la"}), &[]);
    assert_eq!(tier, json!(["allow", ""]));
    let (context, _) = call("impact-context", &json!({"command": "ls -la"}), &[]);
    assert_eq!(context, Value::Null);
}

#[test]
fn quiet_cli_bounds_and_spills_using_the_shared_native_kernel() {
    let scratch = Scratch::new("quiet");
    let large = "x\n".repeat(9000);
    let config = json!({
        "payload": {"tool_response": {"stdout": large, "stderr": ""}},
        "repo_root": scratch.path(),
        "save_dir": scratch.path().join("spill"),
        "stamp": "20260926T120000",
        "output_field": "updatedToolOutput",
        "limit_bytes": 8000,
    });
    let (result, _) = call("bash-quiet-envelope", &config, &[]);
    let output = result["hookSpecificOutput"]["updatedToolOutput"]["stdout"]
        .as_str()
        .unwrap();
    assert!(output.contains("[elided"));
    assert_eq!(
        std::fs::read_dir(scratch.path().join("spill"))
            .unwrap()
            .count(),
        1
    );
    let (result, _) = call(
        "tool-quiet-envelope",
        &json!({
            "payload": {"tool_response": {"type": "text", "file": {"content": "a\n".repeat(9000)}}},
            "repo_root": scratch.path(), "save_dir": scratch.path().join("spill"),
            "stamp": "20260926T120000", "output_field": "updatedToolOutput", "limit_bytes": 16000,
        }),
        &[],
    );
    assert!(
        result["hookSpecificOutput"]["updatedToolOutput"]["file"]["content"]
            .as_str()
            .unwrap()
            .contains("Read(offset=")
    );
}

#[test]
fn nonpositive_quiet_cap_disables_legacy_rewrite() {
    let scratch = Scratch::new("quiet-off");
    let request = json!({
        "payload": {"tool_response": "oversized text"},
        "repo_root": scratch.path(), "save_dir": scratch.path().join("spill"),
        "stamp": "20260926T120000", "output_field": "updatedToolOutput",
        "limit_bytes": -1,
    });
    let (tool, _) = call("tool-quiet-envelope", &request, &[]);
    let (bash, _) = call("bash-quiet-envelope", &request, &[]);
    let quiet = json!({"hookSpecificOutput": {"hookEventName": "PostToolUse"}});
    assert_eq!(tool, quiet);
    assert_eq!(bash, quiet);
    assert!(!scratch.path().join("spill").exists());
}

#[test]
fn read_budget_and_gate_start_write_native_state() {
    let scratch = Scratch::new("state");
    let path = scratch.path().to_str().unwrap();
    let (first, _) = call(
        "read-budget",
        &json!({
            "payload": {"session_id": "s1", "tool_response": "x".repeat(160)},
            "state_dir": path,
        }),
        &[("READ_BUDGET_STEP_TOKENS", "50")],
    );
    assert!(first["hookSpecificOutput"]
        .get("additionalContext")
        .is_none());
    let (second, _) = call(
        "read-budget",
        &json!({
            "payload": {"session_id": "s1", "tool_response": "x".repeat(160)},
            "state_dir": path,
        }),
        &[("READ_BUDGET_STEP_TOKENS", "50")],
    );
    assert!(second["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap()
        .contains("READ BUDGET"));
    let (result, _) = call(
        "gate-start",
        &json!({"payload": {"session_id": "s2"}}),
        &[("CRG_GATE_STATE_DIR", path)],
    );
    assert_eq!(result, Value::Null);
    assert!(std::fs::read_dir(scratch.path()).unwrap().any(|entry| entry
        .unwrap()
        .file_name()
        .to_string_lossy()
        .ends_with(".pending")));
}

#[test]
fn unknown_operation_fails_closed_with_a_useful_error() {
    use std::io::Write;
    let mut child = Command::new(env!("CARGO_BIN_EXE_forge"))
        .args(["legacy-hook", "not-an-operation"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"{}").unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("unknown legacy hook operation"));
}

#[test]
fn legacy_shell_propagates_native_guard_and_impact_failures() {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;

    let scratch = Scratch::new("shell-errors");
    let bin = scratch.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let fake_python = bin.join("python3");
    std::fs::write(
        &fake_python,
        "#!/bin/sh\nif [ \"$1\" = '-c' ]; then cat >/dev/null; printf 'git status\\n'; exit 0; fi\ncase \"$1\" in\n  *_bash_guard.py) exit \"$FAKE_GUARD_EXIT\";;\n  *_bash_impact.py) echo 'impact unavailable' >&2; exit 2;;\nesac\nexit 2\n",
    )
    .unwrap();
    std::fs::set_permissions(&fake_python, std::fs::Permissions::from_mode(0o755)).unwrap();
    let shell =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../src/tooling/hooks/claude/pre-bash.sh");
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    for (guard_exit, expected) in [
        ("2", "native guard failed"),
        ("0", "native impact analysis failed"),
    ] {
        let mut child = Command::new(&shell)
            .env("PATH", &path)
            .env("FAKE_GUARD_EXIT", guard_exit)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(b"{\"tool_input\":{\"command\":\"git status\"}}")
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        assert!(String::from_utf8_lossy(&output.stderr).contains(expected));
    }
}
