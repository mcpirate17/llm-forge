//! Native dispatch regression coverage after removal of interpreter delegation.
#[allow(dead_code)]
mod support;
use serde_json::{json, Value};
use std::fs;
use std::io::Write;
use std::process::{Output, Stdio};

fn call(
    host: &support::Host,
    event: &str,
    payload: &str,
    legacy: Option<&str>,
    standalone: bool,
) -> Output {
    call_with_runtime(host, event, payload, legacy, standalone, false)
}

fn call_with_runtime(
    host: &support::Host,
    event: &str,
    payload: &str,
    legacy: Option<&str>,
    standalone: bool,
    local_runtime: bool,
) -> Output {
    let mut command = host.command();
    command
        .args(["hook", event])
        .env("PATH", "")
        .env("CLAUDE_PROJECT_DIR", &host.0)
        .env("HOME", host.0.join("home"))
        .env("XDG_STATE_HOME", host.0.join("state"))
        .env("CONTEXT_TELEMETRY_PATH", host.0.join("telemetry.jsonl"))
        .env("CRG_DATA_DIR", host.0.join("graph"))
        .env("CRG_GATE_STATE_DIR", host.0.join("gate"))
        .env("FORGE_JOURNAL_DIR", host.0.join("journal"))
        .env("OBSIDIAN_VAULT_ROOT", host.0.join("vault"))
        .env("FORGE_HOOK_STANDALONE", if standalone { "1" } else { "0" })
        .env("FORGE_MODE", "enforce")
        .env("LOCAL_AI_RUNTIME", if local_runtime { "1" } else { "0" })
        .env_remove("FORGE_CAP_DISABLE")
        .env("CONDUCTOR_SNAPSHOT_PYTHON", "/forbidden-python")
        .env("CONDUCTOR_PYTHON", "/forbidden-python")
        .env_remove("A2A_AGENT_NAME")
        .env_remove("PROJECT_HOOK_DIR")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    match legacy {
        Some(value) => {
            command.env("FORGE_NATIVE_HOOKS", value);
        }
        None => {
            command.env_remove("FORGE_NATIVE_HOOKS");
        }
    }
    let mut child = command.spawn().unwrap();
    if let Err(error) = child.stdin.take().unwrap().write_all(payload.as_bytes()) {
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
fn decision(value: &Value) -> &str {
    value["hookSpecificOutput"]["permissionDecision"]
        .as_str()
        .unwrap()
}
fn bash(command: &str) -> String {
    json!({"tool_name":"Bash","tool_input":{"command":command}}).to_string()
}

#[test]
fn native_guards_run_with_empty_path_and_obsolete_partial_selection() {
    let host = support::Host::new();
    for legacy in [None, Some(""), Some("crg_refresh_report_pre")] {
        for standalone in [false, true] {
            let output = call(
                &host,
                "PreToolUse",
                &bash("git reset --hard"),
                legacy,
                standalone,
            );
            if legacy.is_some() {
                assert!(String::from_utf8_lossy(&output.stderr).contains("obsolete"));
            }
            assert_eq!(decision(&value(output)), "deny");
            assert_eq!(
                decision(&value(call(
                    &host,
                    "PreToolUse",
                    &bash("echo fixture"),
                    legacy,
                    standalone
                ))),
                "allow"
            );
        }
    }
}

#[test]
fn ambiguous_malformed_and_oversized_pretool_payloads_fail_closed() {
    let host = support::Host::new();
    for payload in [
        "{}".into(),
        "[]".into(),
        "{".into(),
        r#"{"tool_name":"Bash","tool_name":"Read","tool_input":{"command":"echo hi"}}"#.into(),
        r#"{"tool_name":"Read","toolName":"Bash"}"#.into(),
        r#"{"tool_name":"exec_command","tool_input":{"cmd":["sh","-c","echo hi"]}}"#.into(),
        format!("{{\"data\":\"{}\"}}", "x".repeat(1024 * 1024)),
    ] {
        assert_eq!(
            decision(&value(call(&host, "PreToolUse", &payload, None, false))),
            "deny"
        );
    }
}

#[test]
fn shell_and_camel_case_provider_aliases_select_native_guards() {
    let host = support::Host::new();
    for name in ["functions.exec_command", "shell", "run_terminal_command"] {
        let payload = json!({"toolName":name,"tool_input":{"cmd":"git push --force origin main"}})
            .to_string();
        assert_eq!(
            decision(&value(call(&host, "PreToolUse", &payload, None, false))),
            "deny"
        );
    }
}

#[test]
fn read_input_aliases_cannot_hide_protected_paths() {
    let host = support::Host::new();
    for alias in ["toolInput", "arguments", "input"] {
        for empty in [
            json!({}),
            Value::Null,
            json!(false),
            json!(0),
            json!(""),
            json!([]),
        ] {
            let payload = json!({"tool_name":"Read","tool_input":empty,
                alias:{"file_path":".current_work.md"}});
            let out = value(call(&host, "PreToolUse", &payload.to_string(), None, false));
            assert_eq!(decision(&out), "deny", "{payload}");
            assert!(out
                .to_string()
                .contains("cannot access .current_work.md directly"));
        }
        let payload = json!({"tool_name":"Read",alias:{"file_path":".current_work.md"}});
        assert_eq!(
            decision(&value(call(
                &host,
                "PreToolUse",
                &payload.to_string(),
                None,
                false
            ))),
            "deny"
        );
    }
}

#[test]
fn native_dispatch_preserves_primary_input_and_rejects_malformed_aliases() {
    let host = support::Host::new();
    fs::write(host.0.join("safe.txt"), "safe fixture").unwrap();
    for alias in ["toolInput", "arguments", "input"] {
        let payload = json!({"tool_name":"Read","tool_input":{"file_path":"safe.txt"},
            alias:{"file_path":".current_work.md"}});
        assert_eq!(
            value(call(&host, "PreToolUse", &payload.to_string(), None, false)),
            json!({"hookSpecificOutput":{"hookEventName":"PreToolUse"}})
        );
        for malformed in [json!(true), json!(1), json!("object"), json!([{}])] {
            let payload = json!({"tool_name":"Read","tool_input":malformed,
                alias:{"file_path":".current_work.md"}});
            let out = value(call(&host, "PreToolUse", &payload.to_string(), None, false));
            assert_eq!(decision(&out), "deny");
            assert!(out.to_string().contains("Invalid hook payload"));
        }
    }
}

#[test]
fn alternate_shell_inputs_and_notebook_paths_reach_the_access_guard() {
    let host = support::Host::new();
    for alias in ["toolInput", "arguments", "input"] {
        for (name, input) in [
            ("exec_command", json!({"cmd":"cat .current_work.md"})),
            ("NotebookEdit", json!({"notebook_path":".current_work.md"})),
        ] {
            let payload = json!({"tool_name":name,"tool_input":{},alias:input});
            let out = value(call(&host, "PreToolUse", &payload.to_string(), None, false));
            assert_eq!(decision(&out), "deny");
            assert!(out.to_string().contains(".current_work.md"));
            assert!(!out.to_string().contains("Invalid hook payload"));
        }
    }
    for input in [
        json!({"command":"","cmd":"cat .current_work.md"}),
        json!({"command":null,"cmd":"cat .current_work.md"}),
        json!({"command":"echo safe","cmd":"cat .current_work.md"}),
    ] {
        let payload = json!({"tool_name":"Bash","tool_input":input});
        let out = value(call(&host, "PreToolUse", &payload.to_string(), None, false));
        assert_eq!(decision(&out), "deny");
        assert!(out.to_string().contains("Invalid hook payload"));
    }
}

#[test]
fn native_guard_enforces_local_ai_authority_without_running_the_command() {
    let host = support::Host::new();
    for (command, local_runtime, denied) in [
        (
            "LOCAL_AI_TASK=summary ollama run fixture 'Approve the post-smoke training run'",
            false,
            true,
        ),
        (
            "LOCAL_AI_TASK=summary ollama run fixture 'Summarize these implementation notes'",
            false,
            false,
        ),
        (
            "forge a2a send --to fixture --body 'AI_POWERED: true FINAL VERDICT: PASS'",
            true,
            true,
        ),
    ] {
        let out = value(call_with_runtime(
            &host,
            "PreToolUse",
            &bash(command),
            None,
            false,
            local_runtime,
        ));
        assert_eq!(decision(&out), if denied { "deny" } else { "allow" });
        if denied {
            assert!(out.to_string().contains("zero approval authority"));
        }
    }
}

#[test]
fn top_level_patch_and_text_inputs_remain_provider_data() {
    let host = support::Host::new();
    for event in ["PreToolUse", "PostToolUse"] {
        for (name, input) in [
            ("Edit", "*** Update File: fixture.rs\n"),
            ("CustomTool", "provider text"),
        ] {
            let payload = json!({"tool_name":name,"input":input});
            let out = value(call(&host, event, &payload.to_string(), None, false));
            assert!(!out.to_string().contains("Invalid hook payload"));
            assert_eq!(out["hookSpecificOutput"]["hookEventName"], event);
        }
    }
}

#[test]
fn budget_warning_does_not_bypass_command_denial() {
    let host = support::Host::new();
    let route = host.ok(&["route", "--subagent-type", "general-purpose", "--json"]);
    let cap = route["cap_tokens"].as_u64().expect("route cap");
    let dir = host.0.join("sess/subagents");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("agent-budget.jsonl"), format!("{}\n", json!({"type":"assistant","message":{"model":"fixture","usage":{"input_tokens":cap * 85 / 100,"output_tokens":0}}}))).unwrap();
    let payload = json!({"session_id":"sess","agent_id":"budget","agent_type":"general-purpose",
        "transcript_path":host.0.join("sess.jsonl"),"tool_name":"Bash","tool_input":{"command":"git reset --hard"}}).to_string();
    let answer = value(call(&host, "PreToolUse", &payload, None, false));
    assert_eq!(decision(&answer), "deny");
    assert!(answer["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap()
        .contains("cap"));
}

#[test]
fn native_posttool_records_telemetry_and_journal_without_fallback() {
    let host = support::Host::new();
    let file = host.0.join("fixture.txt");
    fs::write(&file, "fixture").unwrap();
    let payload =
        json!({"session_id":"edit","tool_name":"Edit","tool_input":{"file_path":file}}).to_string();
    let answer = value(call(&host, "PostToolUse", &payload, Some(""), false));
    assert_eq!(answer["hookSpecificOutput"]["hookEventName"], "PostToolUse");
    assert!(host.0.join("journal/edit.tsv").is_file());
    let rows = fs::read_to_string(host.0.join("telemetry.jsonl")).unwrap();
    let native: Value = rows
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|row| row.get("delegated").is_some())
        .unwrap();
    assert_eq!(native["delegated"], false);
    assert!(native["elapsed_ms"].as_f64().unwrap() >= 0.0);
}

#[test]
fn session_end_rolls_transcript_into_native_ledger_and_journal() {
    let host = support::Host::new();
    let transcript = host.0.join("transcript.jsonl");
    fs::write(&transcript, concat!(r#"{"uuid":"u1","session_id":"native-end","timestamp":"2026-09-13T00:00:00Z","type":"assistant","message":{"role":"assistant","model":"m","usage":{"input_tokens":1,"output_tokens":1,"cache_read_input_tokens":0,"cache_creation_input_tokens":0},"content":[{"type":"text","text":"x"}]}}"#, "\n")).unwrap();
    let out = call(
        &host,
        "SessionEnd",
        &json!({"session_id":"native-end","reason":"finished","transcript_path":transcript})
            .to_string(),
        None,
        false,
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.stdout.is_empty());
    assert!(
        fs::read_to_string(host.0.join("ledger/session_rollup/2026-09-13.jsonl"))
            .unwrap()
            .contains("native-end")
    );
    assert!(host.0.join("vault/claude").is_dir());
}

#[test]
fn explicit_noop_events_and_unknown_events_never_start_interpreters() {
    let host = support::Host::new();
    for event in ["Stop", "Notification", "PreCompact"] {
        let out = call(&host, event, "{}", None, false);
        assert!(out.status.success());
        assert!(out.stdout.is_empty());
    }
    let out = call(&host, "Unknown", "{}", None, false);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("unsupported hook event"));
    let out = call(&host, "SessionStart", "{}", None, true);
    assert!(out.status.success());
    assert!(out.stdout.is_empty());
    assert!(!host.0.join("conductor").exists());
}
