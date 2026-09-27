#![cfg(feature = "python-compat-tests")]
//! Rust-owned contracts for the legacy Bash output bounding entrypoints.

#[path = "python_contracts/agent_comm_support.rs"]
#[allow(dead_code)]
mod comm_support;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use comm_support::py_json;
use pyo3::prelude::*;
use serde_json::{json, Value};
use std::fs;
use std::io::Write;
use std::process::{Command, Stdio};
use support::{module, path, AttrPatch, Case};

#[test]
fn python_binding_spills_large_bash_output() {
    let case = Case::new();
    let spill = case.root().join("spill");
    let original = "line\n".repeat(9000);
    Python::attach(|py| {
        let quiet = module(py, "tooling.hooks.claude._bash_quiet");
        let _save = AttrPatch::replace(quiet.as_any(), "SAVE_DIR", path(py, &spill).as_any());
        let payload = py_json(py, json!({"tool_response":{"stdout":original,"stderr":""}}));
        let output = quiet
            .getattr("hook_output")
            .unwrap()
            .call1((payload,))
            .unwrap();
        let bounded: String = output
            .get_item("hookSpecificOutput")
            .unwrap()
            .get_item("updatedToolOutput")
            .unwrap()
            .get_item("stdout")
            .unwrap()
            .extract()
            .unwrap();
        assert!(bounded.contains("[elided"));
    });
    let saved: Vec<String> = fs::read_dir(spill)
        .unwrap()
        .map(|entry| fs::read_to_string(entry.unwrap().path()).unwrap())
        .collect();
    assert_eq!(saved, [original]);
}

#[test]
fn legacy_shell_entrypoint_honors_codex_output_field() {
    let case = Case::new();
    let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../src/tooling/hooks/claude/post-bash-quiet.sh");
    let payload = json!({"tool_response":{"stdout":"x\n".repeat(9000),"stderr":""}});
    let mut child = Command::new("bash")
        .arg(source)
        .env("BASH_QUIET_SAVE_DIR", case.root())
        .env("BASH_QUIET_OUTPUT_FIELD", "updatedMCPToolOutput")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.to_string().as_bytes())
        .unwrap();
    let result = child.wait_with_output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let output: Value = serde_json::from_slice(&result.stdout).unwrap();
    let specifics = &output["hookSpecificOutput"];
    assert!(specifics["updatedMCPToolOutput"]["stdout"]
        .as_str()
        .unwrap()
        .contains("[elided"));
    assert!(specifics.get("updatedToolOutput").is_none());
}
