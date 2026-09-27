//! The remaining specialized `PreToolUse` hooks: Edit/Write/NotebookEdit
//! claim authorization and graph-tool mark/wait. One native answer per Python
//! registry name supports partial opt-in; complete coverage merges in forge
//! without starting the Python dispatcher.

use anyhow::Result;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::time::Duration;

use crate::{crg_gate, crg_refresh, current_work_guard, identity, merge};

/// Registry order, including the `.*` refresh-failure report.
pub const EDIT_PRETOOLUSE_HOOK_NAMES: [&str; 3] = [
    "crg_refresh_report_pre",
    "crg_gate_verify",
    "current_work_guard_edit",
];
pub const GRAPH_PRETOOLUSE_HOOK_NAMES: [&str; 3] = [
    "crg_gate_mark",
    "crg_refresh_wait",
    "crg_refresh_report_pre",
];

fn tool_name(payload: &Value) -> &str {
    payload
        .get("tool_name")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        .or_else(|| payload.get("toolName").and_then(Value::as_str))
        .unwrap_or("")
}

pub fn is_edit_payload(payload: &Value) -> bool {
    matches!(tool_name(payload), "Edit" | "Write" | "NotebookEdit")
}

pub fn is_graph_payload(payload: &Value) -> bool {
    is_graph_tool_name(tool_name(payload))
}

pub fn is_graph_tool_name(name: &str) -> bool {
    crg_gate::is_graph_tool_name(name)
}

pub fn fully_native(payload: &Value, native_hooks: &HashSet<String>) -> bool {
    let names = if is_edit_payload(payload) {
        &EDIT_PRETOOLUSE_HOOK_NAMES
    } else if is_graph_payload(payload) {
        &GRAPH_PRETOOLUSE_HOOK_NAMES
    } else {
        return false;
    };
    names.iter().all(|name| native_hooks.contains(*name))
}

fn names_for(payload: &Value) -> &'static [&'static str] {
    if is_edit_payload(payload) {
        &EDIT_PRETOOLUSE_HOOK_NAMES
    } else if is_graph_payload(payload) {
        &GRAPH_PRETOOLUSE_HOOK_NAMES
    } else {
        &[]
    }
}

fn run_one(name: &str, payload: &Value) -> Result<Value> {
    match name {
        "crg_refresh_report_pre" => Ok(crg_refresh::failure_output(
            "PreToolUse",
            &crg_refresh::gate_repo_root(),
        )),
        "crg_gate_mark" => {
            crg_gate::mark(payload)?;
            Ok(Value::Null)
        }
        "crg_refresh_wait" => Ok(crg_refresh::wait_output(
            &crg_refresh::gate_repo_root(),
            Duration::from_secs(3),
        )?),
        "crg_gate_verify" => {
            let root = crg_refresh::gate_repo_root();
            let common = crg_gate::checkout_of(&root).map(|(_, common)| common);
            let session = crg_gate::session_checkout(payload, &root, common.as_deref());
            let env: HashMap<String, String> = std::env::vars().collect();
            let owner = identity::resolve_owner(Some(&session), &env).unwrap_or_default();
            Ok(crg_gate::verify(
                payload,
                &owner,
                &root,
                common.as_deref(),
                &env,
            )?)
        }
        "current_work_guard_edit" => Ok(current_work_guard::run(payload)),
        _ => unreachable!("only registry names reach the native pre-edit runner"),
    }
}

/// Only successful native answers are advertised to the Python child. If a
/// handler fails under partial selection, Python still runs its adapter and
/// retains that adapter's original error and fail-closed behavior.
pub fn native_answers(payload: &Value, native_hooks: &HashSet<String>) -> HashMap<String, Value> {
    let mut answers = HashMap::new();
    for name in names_for(payload) {
        if native_hooks.contains(*name) {
            if let Ok(answer) = run_one(name, payload) {
                answers.insert((*name).to_string(), answer);
            }
        }
    }
    answers
}

/// Complete native path: preserve registry merge order and the edit gate's
/// fail-closed error status, including errors from claim-store I/O.
pub fn run_fully_native(payload: &Value) -> Value {
    let outcomes: Vec<_> = names_for(payload)
        .iter()
        .map(|name| {
            let (output, error) = match run_one(name, payload) {
                Ok(value) => (value, None),
                Err(err) => (Value::Null, Some(format!("{err:#}"))),
            };
            merge::HookOutcome {
                name: (*name).to_string(),
                output,
                error,
                fail_closed: *name == "crg_gate_verify",
            }
        })
        .collect();
    merge::merge("PreToolUse", &outcomes)
}
