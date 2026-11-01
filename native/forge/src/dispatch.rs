//! Native hook dispatch. No event delegates to an interpreter. Every matching
//! native guard runs; obsolete partial-selection environment cannot disable it.
use crate::hook_protocol::Protocol;
use crate::{handlers, interpreter, telemetry};
use anyhow::{ensure, Context, Result};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::time::Instant;

pub fn run_hook(event: &str, protocol: Protocol, host: Option<&std::path::Path>) -> Result<u8> {
    if let Some(host) = host {
        let host = host.canonicalize().context("resolve hook host")?;
        ensure!(host.is_dir(), "hook host must be a directory");
        // This CLI branch runs before any worker/thread is started.
        std::env::set_current_dir(&host)?;
        for key in ["PROJECT_DIR", "CLAUDE_PROJECT_DIR", "REPO_ROOT"] {
            std::env::set_var(key, &host);
        }
    }
    let emit = |answer: &Value| print_answer(answer, protocol, event);
    let standalone = std::env::var("FORGE_HOOK_STANDALONE").as_deref() == Ok("1");
    if standalone && !matches!(event, "PreToolUse" | "SubagentStop") {
        return Ok(0);
    }
    if std::env::var_os("FORGE_NATIVE_HOOKS").is_some() {
        eprintln!("forge: FORGE_NATIVE_HOOKS is obsolete; all native guards remain enabled");
    }
    let start = Instant::now();
    let result = match event {
        "PreToolUse" => match read_tool_payload() {
            Ok(payload) => emit(&pretool_answer(&payload, standalone)),
            Err(error) => emit(&deny(&format!("Invalid hook payload: {error:#}"))),
        },
        "PostToolUse" => {
            let payload = read_tool_payload()?;
            emit(&handlers::run_post_tooluse_fully_native(&payload))
        }
        "SessionStart" => {
            let (input, _) = read_payload()?;
            emit(&crate::session_start::run(
                &interpreter::project_root(),
                &input,
            )?)
        }
        "SessionEnd" => {
            let (input, _) = read_payload()?;
            crate::session_end::rollup_ending_session(&input);
            crate::session_end::journal_ending_session(&input);
            Ok(0)
        }
        "SubagentStop" => {
            crate::subagent_stop::rollup_ending_agent(&read_payload()?.0);
            Ok(0)
        }
        "Stop" | "UserPromptSubmit" | "Notification" | "PreCompact" | "PostToolUseFailure"
        | "PermissionRequest" | "SubagentStart" => Ok(0),
        _ => anyhow::bail!("unsupported hook event {event:?}"),
    };
    telemetry::record_native(event, start.elapsed().as_secs_f64() * 1000.0);
    result
}

fn read_payload() -> Result<(String, Value)> {
    let mut input = String::new();
    std::io::stdin()
        .take(1024 * 1024 + 1)
        .read_to_string(&mut input)?;
    ensure!(input.len() <= 1024 * 1024, "hook payload exceeds 1 MiB");
    if input.trim().is_empty() {
        input = "{}".to_owned();
    }
    let parsed = forge::tool_schema::parse_document(&input)?;
    ensure!(parsed.is_object(), "hook payload must be an object");
    Ok((input, parsed))
}

fn read_tool_payload() -> Result<Value> {
    let (_, mut payload) = read_payload()?;
    if let (Some(one), Some(two)) = (payload.get("tool_name"), payload.get("toolName")) {
        ensure!(one == two, "conflicting tool_name and toolName");
    }
    let name = payload
        .get("tool_name")
        .or_else(|| payload.get("toolName"))
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        .context("tool_name must be a nonempty string")?;
    ensure!(name.len() <= 1024, "tool name exceeds 1024 bytes");
    payload["tool_name"] = Value::String(name.to_owned());
    normalize_shell_payload(&mut payload)?;
    Ok(payload)
}

/// Command bytes and provider-specific fields survive canonicalization intact.
fn normalize_shell_payload(payload: &mut Value) -> Result<()> {
    let Some(name) = payload
        .get("tool_name")
        .or_else(|| payload.get("toolName"))
        .and_then(Value::as_str)
    else {
        return Ok(());
    };
    let lower = name.to_ascii_lowercase();
    if !matches!(
        lower.strip_prefix("functions.").unwrap_or(&lower),
        "bash"
            | "shell"
            | "exec_command"
            | "shell_command"
            | "run_shell_command"
            | "run_terminal_command"
    ) {
        return Ok(());
    }
    let input = payload
        .get("tool_input")
        .and_then(Value::as_object)
        .with_context(|| format!("shell tool {name:?} requires an object tool_input"))?;
    let command = input
        .get("command")
        .or_else(|| input.get("cmd"))
        .and_then(Value::as_str)
        .with_context(|| {
            format!(
                "shell tool {name:?} requires a string command or cmd; argv arrays are unsupported"
            )
        })?;
    if let Some(cmd) = input.get("cmd") {
        ensure!(
            cmd.as_str() == Some(command),
            "shell tool {name:?} has conflicting or malformed command and cmd values"
        );
    }
    let command = command.to_owned();
    payload["tool_name"] = json!("Bash");
    payload["tool_input"]["command"] = json!(command);
    Ok(())
}

fn deny(reason: &str) -> Value {
    json!({"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":reason}})
}

fn pretool_answer(payload: &Value, standalone: bool) -> Value {
    let warning = match crate::cap_enforce::check(payload) {
        crate::cap_enforce::CapCheck::NoOp => None,
        crate::cap_enforce::CapCheck::Warn(context) => Some(context),
        crate::cap_enforce::CapCheck::Deny(reason) => return deny(&reason),
    };
    let mut answer = match payload["tool_name"].as_str().unwrap_or("") {
        "Bash" => handlers::run_bash_pretooluse_fully_native(
            payload,
            &handlers::default_native_hook_names(),
        ),
        "Agent" if standalone => crate::merge::merge(
            "PreToolUse",
            &[crate::route::hook_outcome_for_agent(payload)],
        ),
        "Agent" => handlers::run_agent_pretooluse_fully_native(payload),
        "Read" => handlers::run_read_pretooluse_fully_native(payload),
        _ if crate::pre_edit::is_edit_payload(payload)
            || crate::pre_edit::is_graph_payload(payload) =>
        {
            crate::pre_edit::run_fully_native(payload)
        }
        _ => handlers::run_generic_pretooluse_fully_native(payload),
    };
    if let Some(warning) = warning {
        // A budget warning is context, never an early allow that bypasses guards.
        let existing = answer["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap_or("");
        let combined = if existing.is_empty() {
            warning
        } else {
            format!("{existing}\n\n{warning}")
        };
        if !answer["hookSpecificOutput"].is_object() {
            answer["hookSpecificOutput"] = json!({"hookEventName":"PreToolUse"});
        }
        answer["hookSpecificOutput"]["additionalContext"] = json!(combined);
    }
    answer
}

fn print_answer(answer: &Value, protocol: Protocol, event: &str) -> Result<u8> {
    let projected;
    let answer = if protocol == Protocol::Claude {
        answer
    } else {
        projected = crate::hook_protocol::project(protocol, event, answer);
        &projected
    };
    writeln!(std::io::stdout(), "{answer}").context("write native hook answer")?;
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn provider_shell_aliases_keep_command_bytes_and_fields() {
        let command = "set -euo pipefail\nrg -n 'quoted text' -- 'path with spaces'";
        for alias in [
            "Bash",
            "bash",
            "shell",
            "exec_command",
            "shell_command",
            "run_shell_command",
            "run_terminal_command",
            "functions.exec_command",
            "functions.shell",
            "functions.run_shell_command",
        ] {
            for field in ["tool_name", "toolName"] {
                for key in ["command", "cmd"] {
                    let mut payload = json!({field:alias,"tool_input":{key:command,"timeout":600},"session_id":"keep"});
                    normalize_shell_payload(&mut payload).unwrap();
                    assert_eq!(payload["tool_name"], "Bash");
                    assert_eq!(payload["tool_input"]["command"], command);
                    assert_eq!(payload["tool_input"][key], command);
                    assert_eq!(payload["tool_input"]["timeout"], 600);
                    assert_eq!(payload["session_id"], "keep");
                }
            }
        }
    }
    #[test]
    fn malformed_shell_inputs_fail_loudly() {
        for mut payload in [
            json!({"tool_name":"exec_command"}),
            json!({"tool_name":"shell","tool_input":{"cmd":["sh","-c","echo ok"]}}),
            json!({"tool_name":"Bash","tool_input":{"command":null}}),
            json!({"tool_name":"functions.exec_command","tool_input":{"cmd":false}}),
            json!({"tool_name":"Bash","tool_input":{"command":"one","cmd":"two"}}),
        ] {
            assert!(normalize_shell_payload(&mut payload)
                .unwrap_err()
                .to_string()
                .contains("shell tool"));
        }
    }
    #[test]
    fn unknown_namespaced_tools_are_unchanged() {
        let mut payload = json!({"tool_name":"mcp__custom__exec_command","tool_input":{"cmd":[]}});
        let original = payload.clone();
        normalize_shell_payload(&mut payload).unwrap();
        assert_eq!(payload, original);
    }
}
