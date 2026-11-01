//! Cooperative native PreToolUse
//! gates for `.current_work.md` access and local-AI clerical enforcement.
//! These tool-policy checks are not an OS sandbox.

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

const DENY_HINT: &str = "Use `forge cards search ...` and \
`forge memory search ...` to read workspace context. \
Write findings to research/notes/<topic>.md, then \
`forge memory ingest`. Short status: \
`forge session append --host <repo> --owner <you> --title '...' --body '...'` \
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
const PATH_KEYS: [&str; 5] = [
    "file_path",
    "path",
    "filePath",
    "target_file",
    "notebook_path",
];

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

// Match the provider alias order without allocating or cloning the input.
// Empty placeholders defer to an alternate; malformed populated inputs deny.
fn empty_alias(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::Bool(value) => !value,
        Value::Number(value) => value.as_f64() == Some(0.0),
        Value::String(value) => value.is_empty(),
        Value::Array(value) => value.is_empty(),
        Value::Object(value) => value.is_empty(),
    }
}

fn input_key(payload: &Value) -> Result<Option<&'static str>, &'static str> {
    for key in ["tool_input", "toolInput", "arguments", "input"] {
        if let Some(value) = payload.get(key) {
            // Root input also carries raw patch/text data for some providers.
            // Only its object form is a tool-argument alias.
            if key == "input" && value.is_string() {
                continue;
            }
            if !empty_alias(value) {
                return if value.is_object() {
                    Ok(Some(key))
                } else {
                    Err("hook input must select an object from tool_input/toolInput/arguments/input")
                };
            }
        }
    }
    Ok(None)
}

fn tool_input(payload: &Value) -> Result<&Value, &'static str> {
    Ok(input_key(payload)?.map_or(&Value::Null, |key| &payload[key]))
}

/// Normalize before dispatch so access, claim, budget and graph handlers all
/// inspect the same object. The usual canonical input needs no copy.
pub(crate) fn normalize_tool_input(payload: &mut Value) -> Result<(), &'static str> {
    if let Some(key) = input_key(payload)? {
        if key != "tool_input" {
            payload["tool_input"] = payload[key].clone();
        }
    }
    Ok(())
}

fn file_path(tool_input: &Value) -> String {
    for key in PATH_KEYS {
        if let Some(value) = tool_input.get(key).and_then(Value::as_str) {
            if !value.is_empty() {
                return value.to_string();
            }
        }
    }
    String::new()
}

// Direct handler calls accept empty aliases. Dispatch separately rejects
// conflicting tool names and malformed or conflicting shell commands.
fn string_alias(input: &Value, primary: &str, alternate: &str) -> Result<String, &'static str> {
    let value = match input.get(primary) {
        None => input.get(alternate),
        Some(value) if empty_alias(value) => input.get(alternate),
        value => value,
    };
    match value {
        None => Ok(String::new()),
        Some(Value::String(value)) => Ok(value.clone()),
        Some(value) if empty_alias(value) => Ok(String::new()),
        _ => Err("tool name and shell command aliases must select a string"),
    }
}

fn tool_name(payload: &Value) -> Result<String, &'static str> {
    string_alias(payload, "tool_name", "toolName")
}

fn shell_command(tool_input: &Value) -> Result<String, &'static str> {
    string_alias(tool_input, "command", "cmd")
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
    let input = match tool_input(payload) {
        Ok(input) => input,
        Err(reason) => return Some(format!("BLOCKED: {reason}")),
    };
    let name = match tool_name(payload) {
        Ok(name) => name,
        Err(reason) => return Some(format!("BLOCKED: {reason}")),
    };
    // Providers use different path fields. A safe field must not hide a
    // protected notebook/path alias consumed by the actual tool.
    if PATH_KEYS.iter().any(|key| {
        input
            .get(key)
            .and_then(Value::as_str)
            .is_some_and(is_current_work_path)
    }) {
        let name = if name.is_empty() {
            "file tool".to_string()
        } else {
            name
        };
        return Some(format!(
            "BLOCKED: {name} cannot access {CURRENT_WORK_NAME} directly. {DENY_HINT}"
        ));
    }
    let name = name.to_lowercase();
    if SHELL_TOOLS.contains(&name.as_str()) {
        let command = match shell_command(input) {
            Ok(command) => command,
            Err(reason) => return Some(format!("BLOCKED: {reason}")),
        };
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
    let path = file_path(tool_input(payload).ok()?);
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

    #[test]
    fn empty_primary_aliases_cannot_bypass_shell_denials() {
        for primary in [
            json!(""),
            Value::Null,
            json!(false),
            json!(0),
            json!([]),
            json!({}),
        ] {
            for grok in [false, true] {
                let mut p = json!({
                    "tool_name": primary,
                    "toolName": "Bash",
                    "tool_input": {"command": primary, "cmd": "cat .current_work.md"},
                });
                if !grok {
                    p["tool_name"] = json!("Bash");
                    p.as_object_mut().unwrap().remove("toolName");
                }
                let out = run(&p);
                if grok {
                    assert_eq!(out["decision"], "deny");
                } else {
                    assert_eq!(out["hookSpecificOutput"]["permissionDecision"], "deny");
                }
            }
        }
        let safe = payload(
            "Bash",
            json!({"command": "echo safe", "cmd": "cat .current_work.md"}),
        );
        assert_eq!(evaluate_payload(&safe), None);
    }

    #[test]
    fn malformed_primary_aliases_remain_invalid() {
        for primary in [
            json!(true),
            json!(1),
            json!(["Bash"]),
            json!({"name":"Bash"}),
        ] {
            let name = json!({"tool_name":primary,"toolName":"Bash"});
            assert!(tool_name(&name).is_err());
            assert!(evaluate_payload(&name).is_some());
            let input = json!({"command":primary,"cmd":"cat .current_work.md"});
            assert!(shell_command(&input).is_err());
            assert!(evaluate_payload(&payload("Bash", input)).is_some());
        }
    }

    #[test]
    fn input_aliases_share_normalization_and_reject_malformed_objects() {
        for key in ["toolInput", "arguments", "input"] {
            for empty in [
                json!({}),
                Value::Null,
                json!(false),
                json!(0),
                json!(""),
                json!([]),
            ] {
                let mut p = json!({"tool_name":"Read","tool_input":empty,
                    key:{"file_path":".current_work.md"}});
                assert!(evaluate_payload(&p).unwrap().contains("cannot access"));
                let before = run(&p);
                normalize_tool_input(&mut p).unwrap();
                assert_eq!(p["tool_input"], p[key]);
                assert_eq!(run(&p), before);
            }
        }
        for malformed in [json!(true), json!(1), json!("object"), json!([{}])] {
            let mut p = json!({"tool_name":"Read","tool_input":malformed,
                "toolInput":{"file_path":"safe.rs"}});
            assert!(evaluate_payload(&p)
                .unwrap()
                .contains("must select an object"));
            assert!(normalize_tool_input(&mut p).is_err());
        }
        let mut primary = payload("Read", json!({"file_path":"safe.rs"}));
        primary["arguments"] = json!({"file_path":".current_work.md"});
        normalize_tool_input(&mut primary).unwrap();
        assert_eq!(primary["tool_input"]["file_path"], "safe.rs");
        assert_eq!(evaluate_payload(&primary), None);
    }

    #[test]
    fn a_safe_path_cannot_hide_a_protected_notebook_alias() {
        let mut p = payload("NotebookEdit", json!({"notebook_path":".current_work.md"}));
        assert!(evaluate_payload(&p)
            .unwrap()
            .contains("NotebookEdit cannot access"));
        p["tool_input"]["file_path"] = json!("safe.ipynb");
        assert!(evaluate_payload(&p).is_some());
    }

    #[test]
    fn advisories_preserve_protocol_shape_and_denial_precedence() {
        let mut p = json!({"toolName":"Write","toolInput":{"file_path":"e2e/ui.spec.js"}});
        assert_eq!(
            run(&p),
            json!({"hookSpecificOutput":{"additionalContext":MUTATION_HINT}})
        );
        p["toolInput"]["file_path"] = json!("src/handoff.rs");
        assert_eq!(run(&p), Value::Null);
        // The directory would ordinarily trigger a test advisory; denial wins.
        p["toolInput"]["file_path"] = json!("test/.current_work.md");
        assert!(evaluate_payload(&p).is_some());
        assert_eq!(advisory_for_payload(&p), None);
        assert_eq!(run(&p)["decision"], "deny");
        assert!(run(&p).get("hookSpecificOutput").is_none());
        for protocol in ["codex", "grok"] {
            assert_eq!(hook_response(None, protocol, None), Value::Null);
            assert_eq!(
                hook_response(Some("blocked"), protocol, Some("hint")),
                hook_response(Some("blocked"), protocol, None)
            );
        }
    }

    #[test]
    fn shell_segments_keep_reads_distinct_from_mentions_and_invalid_quotes() {
        for command in [
            "env cat .current_work.md",
            "FOO=1 cat .current_work.md",
            "rg foo -- .current_work.md",
            "sudo cat .current_work.md",
            "echo x; cat .current_work.md",
            "rg -- foo .current_work.md",
        ] {
            assert!(deny_shell_access(command).is_some(), "{command}");
        }
        for command in [
            "cat 'unterminated",
            "cat '.current_work.md",
            "FOO=.current_work.md sudo",
            "rg .current_work.md --",
            "rg -- .current_work.md",
            "echo '.current_work.md is a compatibility log'",
        ] {
            assert_eq!(deny_shell_access(command), None, "{command}");
        }
    }
}
