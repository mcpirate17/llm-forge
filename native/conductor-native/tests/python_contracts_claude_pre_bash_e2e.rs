#![cfg(feature = "python-compat-tests")]
//! Real Claude PreToolUse/Bash entrypoint contracts, asserted in Rust.

use serde_json::{json, Value};
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

fn hook() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../src/tooling/hooks/claude/pre-bash.sh")
}

fn decision(command: &str) -> (String, String) {
    let mut child = Command::new(hook())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let payload = json!({"tool_input": {"command": command}}).to_string();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.as_bytes())
        .unwrap();
    let started = Instant::now();
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if started.elapsed() >= Duration::from_secs(30) {
            child.kill().unwrap();
            let output = child.wait_with_output().unwrap();
            panic!(
                "PreToolUse hook timed out after 30s: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        thread::sleep(Duration::from_millis(10));
    }
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    let specific = &value["hookSpecificOutput"];
    (
        specific["permissionDecision"].as_str().unwrap().to_owned(),
        specific["permissionDecisionReason"]
            .as_str()
            .unwrap_or("")
            .to_owned(),
    )
}

#[test]
fn hook_denies_destructive_commands_with_a_reason() {
    for command in [
        "git push --force origin master",
        "git reset --hard HEAD~1",
        "pip install numpy",
    ] {
        let (verdict, reason) = decision(command);
        assert_eq!(verdict, "deny", "{command}");
        assert!(reason.starts_with("BLOCKED"), "{command}: {reason}");
    }
}

#[test]
fn hook_allows_safe_commands_and_quoted_mentions() {
    for command in [
        "git push --force-with-lease origin master",
        "echo '{\"command\":\"git push --force\"}'",
        "uv pip install numpy",
        "ls -la",
        "git status",
    ] {
        let (verdict, _) = decision(command);
        assert_eq!(verdict, "allow", "{command}");
    }
}
