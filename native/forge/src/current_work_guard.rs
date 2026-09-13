//! Native port of `conductor.current_work_guard`: cooperative PreToolUse
//! gates for `.current_work.md` access and local-AI clerical enforcement.
//! Backs the `current_work_guard_bash` hook.

use std::path::Path;

use serde_json::{json, Value};

use crate::local_ai_policy::deny_local_ai_command;
use crate::write_targets::posix_shlex::{self, PUNCTUATION_CHARS_NARROW};

pub const CURRENT_WORK_NAME: &str = ".current_work.md";

pub const MUTATION_HINT: &str =
    "Mutation evidence covers ONLY the files you changed and the tests that \
exercise them -- never the whole repo. `make mutation-plan`, then \
`make mutation-generate`, then `make mutation-engine-run`. Automatic \
engines only; hand-authored mutants, manifests, survivor baselines and \
receipts are forbidden (KB-MUT-02). A changed test with no current \
receipt is debt to record in the PR body, not a reason to hold the branch.";

const DENY_HINT: &str = "Use `python -m conductor.kb_retrieve query ...` and \
`python -m conductor.memory_index query ...` to read workspace context. \
Write findings to research/notes/<topic>.md, then \
`python -m conductor.memory_index index`. Short status: \
`python -m conductor.handoff append --owner <you> --title '...' --body '...'` \
(max 12 lines).";

const TEST_SUFFIXES: [&str; 14] = [
    "_test.py",
    "_test.c",
    "_test.cc",
    "_test.cpp",
    "_test.cxx",
    ".test.js",
    ".test.jsx",
    ".test.ts",
    ".test.tsx",
    ".spec.js",
    ".spec.jsx",
    ".spec.ts",
    ".spec.tsx",
    "Test.java",
];

const SHELL_OPERATORS: [&str; 5] = [";", "&", "&&", "|", "||"];
const HARMLESS_MENTION_COMMANDS: [&str; 2] = ["echo", "printf"];
const SEARCH_COMMANDS: [&str; 2] = ["grep", "rg"];
const SHELL_TOOLS: [&str; 4] = ["bash", "shell", "run_shell_command", "exec_command"];

fn basename(token: &str) -> String {
    Path::new(token)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

pub fn is_test_creation_path(path: &str) -> bool {
    if path.is_empty() {
        return false;
    }
    let target = Path::new(path);
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let has_test_part = target.components().any(|c| c.as_os_str() == "test");
    has_test_part
        || name.starts_with("test_")
        || TEST_SUFFIXES.iter().any(|suf| name.ends_with(suf))
}

pub fn is_current_work_path(path: &str) -> bool {
    if path.is_empty() {
        return false;
    }
    basename(path) == CURRENT_WORK_NAME
}

fn tool_input(payload: &Value) -> Value {
    for key in ["tool_input", "toolInput", "arguments", "input"] {
        if let Some(value) = payload.get(key) {
            if value.is_object() {
                return value.clone();
            }
        }
    }
    json!({})
}

fn file_path(tool_input: &Value) -> String {
    for key in ["file_path", "path", "filePath", "target_file"] {
        if let Some(value) = tool_input.get(key).and_then(Value::as_str) {
            if !value.is_empty() {
                return value.to_string();
            }
        }
    }
    String::new()
}

fn tool_name(payload: &Value) -> String {
    payload
        .get("tool_name")
        .or_else(|| payload.get("toolName"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn shell_command(tool_input: &Value) -> String {
    tool_input
        .get("command")
        .or_else(|| tool_input.get("cmd"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn command_segments(command: &str) -> Vec<Vec<String>> {
    let Ok(tokens) = posix_shlex::tokenize_no_comments(command, PUNCTUATION_CHARS_NARROW) else {
        return Vec::new();
    };
    let mut segments: Vec<Vec<String>> = vec![Vec::new()];
    for token in tokens {
        if SHELL_OPERATORS.contains(&token.as_str()) {
            if !segments.last().unwrap().is_empty() {
                segments.push(Vec::new());
            }
            continue;
        }
        segments.last_mut().unwrap().push(token);
    }
    segments.into_iter().filter(|s| !s.is_empty()).collect()
}

fn path_token(token: &str) -> bool {
    basename(token.trim_end_matches('/')) == CURRENT_WORK_NAME
}

fn first_positional(tokens: &[String]) -> Option<usize> {
    for (index, token) in tokens.iter().enumerate() {
        if token == "--" {
            return if index + 1 < tokens.len() {
                Some(index + 1)
            } else {
                None
            };
        }
        if !token.starts_with('-') {
            return Some(index);
        }
    }
    None
}

fn segment_reads_raw_log(segment: &[String]) -> bool {
    let mut segment = segment;
    while let Some(first) = segment.first() {
        if first.contains('=') && !(first.starts_with('/') || first.starts_with("./")) {
            segment = &segment[1..];
        } else {
            break;
        }
    }
    if let Some(first) = segment.first() {
        let name = basename(first);
        if name == "env" || name == "sudo" {
            segment = &segment[1..];
        }
    }
    if segment.is_empty() {
        return false;
    }
    let executable = basename(&segment[0]);
    let arguments = &segment[1..];
    let target_indices: Vec<usize> = arguments
        .iter()
        .enumerate()
        .filter(|(_, t)| path_token(t))
        .map(|(i, _)| i)
        .collect();
    if target_indices.is_empty() || HARMLESS_MENTION_COMMANDS.contains(&executable.as_str()) {
        return false;
    }
    if SEARCH_COMMANDS.contains(&executable.as_str()) {
        return match first_positional(arguments) {
            Some(pattern_index) => target_indices.iter().any(|&index| index > pattern_index),
            None => false,
        };
    }
    true
}

pub fn deny_shell_access(command: &str) -> Option<String> {
    if !command.contains(CURRENT_WORK_NAME) {
        return None;
    }
    if command_segments(command)
        .iter()
        .any(|s| segment_reads_raw_log(s))
    {
        return Some(format!(
            "BLOCKED: direct shell access to {CURRENT_WORK_NAME} is unsupported. {DENY_HINT}"
        ));
    }
    None
}

/// `evaluate_payload`: the deny reason for this call, or `None` to allow.
pub fn evaluate_payload(payload: &Value) -> Option<String> {
    let input = tool_input(payload);
    let path = file_path(&input);
    if is_current_work_path(&path) {
        let name = tool_name(payload);
        let name = if name.is_empty() {
            "file tool".to_string()
        } else {
            name
        };
        return Some(format!(
            "BLOCKED: {name} cannot access {CURRENT_WORK_NAME} directly. {DENY_HINT}"
        ));
    }
    let name = tool_name(payload).to_lowercase();
    if SHELL_TOOLS.contains(&name.as_str()) {
        let command = shell_command(&input);
        let local_runtime = std::env::var("LOCAL_AI_RUNTIME")
            .map(|v| matches!(v.to_lowercase().as_str(), "1" | "true" | "yes"))
            .unwrap_or(false);
        return deny_local_ai_command(&command, local_runtime)
            .or_else(|| deny_shell_access(&command));
    }
    None
}

/// `advisory_for_payload`: the mutation-evidence reminder for a new test file,
/// or `None`.
pub fn advisory_for_payload(payload: &Value) -> Option<String> {
    if evaluate_payload(payload).is_some() {
        return None;
    }
    let path = file_path(&tool_input(payload));
    if is_test_creation_path(&path) {
        return Some(MUTATION_HINT.to_string());
    }
    None
}

/// `hook_protocol`: "grok" (bare `toolName`/`pre_tool_use` shape) or "codex"
/// (`hookSpecificOutput` shape) -- everything else that reaches this hook.
pub fn hook_protocol(payload: &Value) -> &'static str {
    let event_name = payload.get("hookEventName").and_then(Value::as_str);
    if payload.get("toolName").is_some() || event_name == Some("pre_tool_use") {
        "grok"
    } else {
        "codex"
    }
}

/// `hook_response`: a protocol-correct deny or advisory response, or
/// `Value::Null` for neither (mirrors Python's `None`, meaning: print
/// nothing).
pub fn hook_response(reason: Option<&str>, protocol: &str, advisory: Option<&str>) -> Value {
    if let Some(reason) = reason {
        return if protocol == "grok" {
            json!({"decision": "deny", "reason": reason})
        } else {
            json!({
                "hookSpecificOutput": {
                    "hookEventName": "PreToolUse",
                    "permissionDecision": "deny",
                    "permissionDecisionReason": reason,
                }
            })
        };
    }
    let Some(advisory) = advisory.filter(|a| !a.is_empty()) else {
        return Value::Null;
    };
    if protocol == "grok" {
        json!({"hookSpecificOutput": {"additionalContext": advisory}})
    } else {
        json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "additionalContext": advisory,
            }
        })
    }
}

/// `current_work_guard_bash`'s full verdict for one payload -- `Value::Null`
/// for "no contribution" (allow, no advisory).
pub fn run(payload: &Value) -> Value {
    let protocol = hook_protocol(payload);
    let reason = evaluate_payload(payload);
    let advisory = advisory_for_payload(payload);
    hook_response(reason.as_deref(), protocol, advisory.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload(tool_name: &str, tool_input: Value) -> Value {
        json!({"tool_name": tool_name, "tool_input": tool_input})
    }

    #[test]
    fn edit_of_current_work_file_is_denied() {
        let p = payload("Edit", json!({"file_path": "/repo/.current_work.md"}));
        assert!(evaluate_payload(&p).is_some());
    }

    #[test]
    fn cat_of_current_work_file_is_denied() {
        let p = payload("Bash", json!({"command": "cat .current_work.md"}));
        assert!(evaluate_payload(&p).is_some());
    }

    #[test]
    fn echo_mentioning_current_work_file_is_allowed() {
        let p = payload("Bash", json!({"command": "echo see .current_work.md"}));
        assert_eq!(evaluate_payload(&p), None);
    }

    #[test]
    fn grep_pattern_after_target_is_allowed_grep_before_target_denies() {
        let allowed = payload("Bash", json!({"command": "grep foo .current_work.md"}));
        assert!(evaluate_payload(&allowed).is_some());
    }

    #[test]
    fn plain_bash_command_is_allowed() {
        let p = payload("Bash", json!({"command": "ls -la"}));
        assert_eq!(evaluate_payload(&p), None);
    }

    #[test]
    fn new_test_file_gets_mutation_advisory() {
        let p = payload("Write", json!({"file_path": "src/conductor/test_foo.py"}));
        assert_eq!(advisory_for_payload(&p), Some(MUTATION_HINT.to_string()));
    }

    #[test]
    fn grok_protocol_uses_bare_decision_shape() {
        let p = json!({
            "toolName": "Edit",
            "tool_input": {"file_path": ".current_work.md"},
        });
        let out = run(&p);
        assert_eq!(out["decision"], json!("deny"));
    }
}
