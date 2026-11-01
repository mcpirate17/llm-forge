#[allow(dead_code)]
mod support;
use serde_json::{json, Value};
use std::fs;
use std::io::Write;
use std::process::{Output, Stdio};

fn call(
    host: &support::Host,
    caller: &support::Host,
    event: &str,
    protocol: &str,
    input: &str,
    host_flag: &str,
) -> Output {
    let mut child = caller
        .command()
        .args([
            "hook",
            event,
            "--protocol",
            protocol,
            host_flag,
            host.0.to_str().unwrap(),
        ])
        .env("PATH", "")
        .env("PROJECT_DIR", &caller.0)
        .env("CLAUDE_PROJECT_DIR", &caller.0)
        .env("REPO_ROOT", &caller.0)
        .env("FORGE_MODE", "enforce")
        .env_remove("FORGE_HOOK_STANDALONE")
        .env_remove("FORGE_NATIVE_HOOKS")
        .env("HOME", host.0.join("home"))
        .env("XDG_STATE_HOME", host.0.join("state"))
        .env("CONTEXT_TELEMETRY_PATH", host.0.join("telemetry.jsonl"))
        .env("FORGE_JOURNAL_DIR", "journal")
        .env("OBSIDIAN_VAULT_ROOT", host.0.join("vault"))
        .env_remove("CRG_DATA_DIR")
        .env_remove("CRG_GATE_STATE_DIR")
        .env_remove("PROJECT_HOOK_DIR")
        .env_remove("A2A_AGENT_NAME")
        .env("CONDUCTOR_PYTHON", "/forbidden-python")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    if let Err(error) = child.stdin.take().unwrap().write_all(input.as_bytes()) {
        assert_eq!(error.kind(), std::io::ErrorKind::BrokenPipe);
    }
    child.wait_with_output().unwrap()
}
fn value(output: Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn native_mcp_post_edit_refreshes_only_applied_successes_and_does_not_reformat() {
    let host = support::Host::new();
    let caller = support::Host::new();
    git2::Repository::init(&host.0).unwrap();
    let source = "fn answer( )->u32{42}\n";
    fs::write(host.0.join("main.rs"), source).unwrap();
    for (apply, failed) in [(false, false), (true, true)] {
        value(call(
            &host,
            &caller,
            "PostToolUse",
            "claude",
            &json!({
            "tool_name":"mcp__forge__code_replace",
            "tool_input":{"path":"main.rs","apply":apply},
            "tool_response":{"isError":failed}})
            .to_string(),
            "--host",
        ));
        assert!(!host.0.join(".forge/graph.db").exists());
        assert!(!host.0.join(".code-review-graph/refresh.pending").exists());
    }
    value(call(
        &host,
        &caller,
        "PostToolUse",
        "claude",
        &json!({
        "tool_name":"mcp__forge__code_replace",
        "tool_input":{"path":"main.rs","apply":true},
        "tool_response":{"isError":false}})
        .to_string(),
        "--host",
    ));
    let output = host
        .command()
        .env("PATH", "")
        .env_remove("CRG_DATA_DIR")
        .args(["graph", "--host"])
        .arg(&host.0)
        .args(["wait", "--timeout-seconds", "10"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(host.0.join(".forge/graph.db").exists());
    assert_eq!(fs::read_to_string(host.0.join("main.rs")).unwrap(), source);
}

#[test]
fn provider_projection_preserves_native_denials_and_malformed_input_refusals() {
    let host = support::Host::new();
    let caller = support::Host::new();
    for payload in [
        json!({"tool_name":"functions.exec_command","tool_input":{"cmd":"git reset --hard"}})
            .to_string(),
        "{\"tool_name\":\"Read\",\"tool_name\":\"Bash\"}".to_owned(),
    ] {
        for protocol in ["claude", "codex"] {
            let answer = value(call(
                &host,
                &caller,
                "PreToolUse",
                protocol,
                &payload,
                "--host",
            ));
            assert_eq!(answer["hookSpecificOutput"]["permissionDecision"], "deny");
            assert_eq!(answer["hookSpecificOutput"]["hookEventName"], "PreToolUse");
        }
    }
}

#[test]
fn codex_plain_allow_is_quiet_and_claude_retains_its_decision() {
    let host = support::Host::new();
    let caller = support::Host::new();
    let input = json!({"tool_name":"Bash","tool_input":{"command":"echo fixture"}}).to_string();
    let claude = value(call(
        &host,
        &caller,
        "PreToolUse",
        "claude",
        &input,
        "--host",
    ));
    assert_eq!(claude["hookSpecificOutput"]["permissionDecision"], "allow");
    let codex = value(call(
        &host,
        &caller,
        "PreToolUse",
        "codex",
        &input,
        "--host",
    ));
    assert!(codex["hookSpecificOutput"]
        .get("permissionDecision")
        .is_none());
}

#[test]
fn explicit_host_and_legacy_flag_scope_startup_and_post_tool_writes() {
    let host = support::Host::new();
    let caller = support::Host::new();
    for protocol in ["claude", "codex"] {
        let answer = value(call(
            &host,
            &caller,
            "SessionStart",
            protocol,
            "{}",
            "--project-dir",
        ));
        assert_eq!(
            answer["hookSpecificOutput"]["hookEventName"],
            "SessionStart"
        );
        assert!(!answer["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap()
            .is_empty());
        assert!(host.0.join("conductor/active_state.json").is_file());
        assert!(!caller.0.join("conductor").exists());
    }
    fs::write(host.0.join("note.md"), "fixture").unwrap();
    let input = json!({"tool_name":"Edit","session_id":"scoped-native","tool_input":{"file_path":"note.md"}}).to_string();
    let output = value(call(
        &host,
        &caller,
        "PostToolUse",
        "codex",
        &input,
        "--host",
    ));
    assert!(output["hookSpecificOutput"]
        .get("updatedMCPToolOutput")
        .is_none());
    assert!(host.0.join("journal").is_dir());
    assert!(!caller.0.join("journal").exists());
}

#[test]
fn unsupported_protocol_fails_before_any_hook_writes() {
    let host = support::Host::new();
    let caller = support::Host::new();
    let output = call(&host, &caller, "SessionStart", "invented", "{}", "--host");
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert!(!host.0.join("conductor").exists());
    assert!(!caller.0.join("conductor").exists());
}
