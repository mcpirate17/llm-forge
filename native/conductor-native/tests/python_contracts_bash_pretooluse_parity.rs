#![cfg(feature = "python-compat-tests")]
//! Python Bash PreToolUse hooks must match the frozen 64-row parity corpus.

#[path = "python_contracts/agent_comm_support.rs"]
#[allow(dead_code)]
mod comm_support;
#[path = "python_contracts/bash_pretooluse_support.rs"]
mod parity_support;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use comm_support::{buffer_text, capture, json_value, py_json};
use parity_support::{
    detach_head, fixture, load, make_git_repo, mark_graph_used, write_claims, MANAGED_ENV,
};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyModule};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use support::{module, Case};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn reset_env(case: &mut Case) {
    for name in MANAGED_ENV {
        case.remove_env(name);
    }
}

fn reload<'py>(py: Python<'py>, name: &str) -> Bound<'py, PyModule> {
    let imported = module(py, name);
    module(py, "importlib")
        .getattr("reload")
        .unwrap()
        .call1((&imported,))
        .unwrap()
        .cast_into::<PyModule>()
        .unwrap()
}

fn install_agent_path(py: Python<'_>) {
    let agent = root().join("src/tooling/hooks/agent");
    module(py, "sys")
        .getattr("path")
        .unwrap()
        .call_method1("insert", (0, agent.to_str().unwrap()))
        .unwrap();
}

fn run_gate(py: Python<'_>, payload: Value) -> Value {
    let gate = reload(py, "crg_gate");
    let payload = py_json(py, payload);
    let checkout = gate
        .getattr("session_checkout")
        .unwrap()
        .call1((&payload,))
        .unwrap();
    let identity = module(py, "conductor.candidate_review.identity");
    let owner = match identity
        .getattr("resolve_owner")
        .unwrap()
        .call1((checkout,))
    {
        Ok(owner) => owner.extract::<String>().unwrap(),
        Err(error)
            if error
                .matches(py, &identity.getattr("OwnerIdentityError").unwrap())
                .unwrap() =>
        {
            String::new()
        }
        Err(error) => panic!("unexpected identity error: {error}"),
    };
    let (stdout, _guard) = capture(py, "stdout");
    let kwargs = PyDict::new(py);
    kwargs.set_item("owner", owner).unwrap();
    gate.getattr("verify_bash")
        .unwrap()
        .call((payload,), Some(&kwargs))
        .unwrap();
    let output = buffer_text(&stdout);
    if output.trim().is_empty() {
        Value::Null
    } else {
        serde_json::from_str(output.trim()).unwrap()
    }
}

fn run_refresh(py: Python<'_>) -> Value {
    reload(py, "crg_gate");
    let refresh = reload(py, "crg_graph_refresh");
    json_value(
        &refresh
            .getattr("failure_output")
            .unwrap()
            .call1(("PreToolUse",))
            .unwrap(),
    )
}

fn run_guard(py: Python<'_>, payload: Value) -> Value {
    let guard = module(py, "conductor.current_work_guard");
    let payload = py_json(py, payload);
    let protocol = guard
        .getattr("hook_protocol")
        .unwrap()
        .call1((&payload,))
        .unwrap();
    let verdict = guard
        .getattr("evaluate_payload")
        .unwrap()
        .call1((&payload,))
        .unwrap();
    let advisory = guard
        .getattr("advisory_for_payload")
        .unwrap()
        .call1((&payload,))
        .unwrap();
    let kwargs = PyDict::new(py);
    kwargs.set_item("protocol", protocol).unwrap();
    kwargs.set_item("advisory", advisory).unwrap();
    json_value(
        &guard
            .getattr("hook_response")
            .unwrap()
            .call((verdict,), Some(&kwargs))
            .unwrap(),
    )
}

fn gate_fixture(case: &Value, parent: &Path, env: &mut Case) -> Value {
    let label = case["id"].as_str().unwrap();
    let repo = make_git_repo(parent, label);
    let state_dir = parent.join(format!("{label}-state"));
    fs::create_dir_all(&state_dir).unwrap();
    if let Some(session) = case["graph_used_session"].as_str() {
        mark_graph_used(&state_dir, session);
    }
    if let Some(claims) = case["claims"].as_array() {
        if !claims.is_empty() {
            write_claims(&repo, claims);
        }
    }
    if case["detach_head"].as_bool().unwrap_or(false) {
        detach_head(&repo);
    }
    env.set_env("CRG_GATE_REPO_ROOT", repo.to_str().unwrap());
    env.set_env("CRG_GATE_STATE_DIR", state_dir.to_str().unwrap());
    env.set_env("GOVERNANCE_OWNER", case["owner"].as_str().unwrap());
    let payload = json!({
        "session_id": case["session_id"],
        "tool_name": "Bash",
        "tool_input": {"command": case["command"]},
    });
    Python::attach(|py| run_gate(py, payload))
}

fn refresh_fixture(case: &Value, parent: &Path, env: &mut Case) -> Value {
    let label = case["id"].as_str().unwrap();
    let repo = make_git_repo(parent, label);
    let data_dir = parent.join(format!("{label}-data"));
    fs::create_dir_all(&data_dir).unwrap();
    if let Some(lines) = case["marker_lines"].as_str() {
        fs::write(data_dir.join("refresh.failed"), lines).unwrap();
    }
    env.set_env("CRG_GATE_REPO_ROOT", repo.to_str().unwrap());
    env.set_env("CRG_DATA_DIR", data_dir.to_str().unwrap());
    Python::attach(run_refresh)
}

fn guard_fixture(case: &Value, env: &mut Case) -> Value {
    if let Some(runtime) = case["local_ai_runtime"].as_str() {
        env.set_env("LOCAL_AI_RUNTIME", runtime);
    }
    Python::attach(|py| run_guard(py, case["payload"].clone()))
}

#[test]
fn fixture_files_exist_and_are_shared_with_the_rust_test() {
    let _case = Case::new();
    assert!(fixture("bash_pretooluse_corpus.json").is_file());
    assert!(fixture("bash_pretooluse_expected.json").is_file());
    let corpus = load("bash_pretooluse_corpus.json");
    let expected = load("bash_pretooluse_expected.json");
    assert_eq!(
        corpus.as_array().unwrap().len(),
        expected.as_object().unwrap().len()
    );
    assert!(corpus.as_array().unwrap().len() >= 64);
}

#[test]
fn python_hooks_match_the_frozen_corpus() {
    let mut case = Case::new();
    Python::attach(install_agent_path);
    let corpus = load("bash_pretooluse_corpus.json");
    let expected = load("bash_pretooluse_expected.json");
    let rows = corpus.as_array().unwrap();
    assert_eq!(rows.len(), expected.as_object().unwrap().len());
    assert!(rows.len() >= 64);
    let parent = case.root().to_path_buf();
    let mut failures = Vec::new();
    for row in rows {
        reset_env(&mut case);
        let actual = match row["hook"].as_str().unwrap() {
            "crg_gate_verify_bash" => gate_fixture(row, &parent, &mut case),
            "crg_refresh_report_pre" => refresh_fixture(row, &parent, &mut case),
            "current_work_guard_bash" => guard_fixture(row, &mut case),
            other => panic!("unknown hook in corpus case: {other:?}"),
        };
        reset_env(&mut case);
        let label = row["id"].as_str().unwrap();
        let frozen = &expected[label];
        if &actual != frozen {
            failures.push(format!(
                "case {label:?}: python={actual:?} expected={frozen:?}"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} parity mismatches:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
