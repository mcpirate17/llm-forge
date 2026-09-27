#![cfg(feature = "python-compat-tests")]
//! Rust-owned PyO3 parity for the frozen 44-row PostToolUse corpus.

#[path = "python_contracts/agent_comm_support.rs"]
#[allow(dead_code)]
mod comm_support;
#[path = "python_contracts/post_tool_parity_cases.rs"]
mod post_tool_cases;
#[path = "python_contracts/post_tool_parity_obsidian.rs"]
mod post_tool_obsidian;
#[path = "python_contracts/post_tool_parity_support.rs"]
mod post_tool_support;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use post_tool_support::{apply_env, fixture, install_paths, load, reset_env};
use pyo3::prelude::*;
use serde_json::Value;
use std::path::Path;
use support::{module, Case};

fn run_case(
    case: &Value,
    parent: &Path,
    env: &mut Case,
    base_path: &str,
    obsidian_index: usize,
) -> Value {
    reset_env(env);
    let kind = case["kind"].as_str().unwrap();
    if kind == "obsidian_edit" {
        return post_tool_obsidian::run(case, parent, env, obsidian_index);
    }
    apply_env(env, &case["env"]);
    match kind {
        "report_post" => post_tool_cases::report_post(case, parent, env),
        "graph_bash" => post_tool_cases::graph_bash(case, parent, env, base_path),
        "read_budget" => post_tool_cases::read_budget(case, parent, env),
        "telemetry_record" => post_tool_cases::telemetry_record(case),
        "telemetry_hook_context" => post_tool_cases::telemetry_hook_context(case),
        "telemetry_path" => post_tool_cases::telemetry_path(case),
        "post_edit" => post_tool_cases::post_edit(case, parent, env, base_path),
        "graph_edit" => post_tool_cases::graph_edit(case, parent, env, base_path),
        other => panic!("unknown kind in corpus case {:?}: {other:?}", case["id"]),
    }
}

#[test]
fn fixture_files_exist_and_are_shared_with_the_rust_test() {
    let _case = Case::new();
    assert!(fixture("post_tool_corpus.json").is_file());
    assert!(fixture("post_tool_expected.json").is_file());
    let corpus = load("post_tool_corpus.json");
    let expected = load("post_tool_expected.json");
    assert_eq!(
        corpus.as_array().unwrap().len(),
        expected.as_object().unwrap().len()
    );
    assert!(corpus.as_array().unwrap().len() >= 40);
}

#[test]
fn python_hooks_match_the_frozen_corpus() {
    let mut case = Case::new();
    Python::attach(|py| {
        install_paths(py);
        module(py, "conductor.context_telemetry");
    });
    let corpus = load("post_tool_corpus.json");
    let expected = load("post_tool_expected.json");
    let base_path = std::env::var("PATH").expect("PATH");
    let base_home = std::env::var_os("HOME");
    let parent = case.mkdir("pt-twin");
    let mut failures = Vec::new();
    let mut obsidian_index = 0;
    for row in corpus.as_array().unwrap() {
        if row["kind"] == "obsidian_edit" {
            obsidian_index += 1;
        }
        let actual = run_case(row, &parent, &mut case, &base_path, obsidian_index);
        reset_env(&mut case);
        let label = row["id"].as_str().unwrap();
        let frozen = &expected[label];
        for (field, value) in actual.as_object().unwrap() {
            let want = &frozen[field];
            let matches = if field == "path_suffix" {
                value.as_str().unwrap().ends_with(want.as_str().unwrap())
            } else {
                value == want
            };
            if !matches {
                failures.push(format!(
                    "case {label:?} ({field}): python={value:?} expected={want:?}"
                ));
            }
        }
    }
    reset_env(&mut case);
    case.set_env("PATH", &base_path);
    if let Some(home) = base_home {
        case.set_env("HOME", home.to_str().unwrap());
    }
    assert!(
        failures.is_empty(),
        "{} parity mismatches:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
