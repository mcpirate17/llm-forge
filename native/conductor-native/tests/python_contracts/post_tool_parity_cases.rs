//! Corpus case execution against the shipped Python PostToolUse APIs.

use crate::comm_support::{json_value, py_json};
use crate::post_tool_support::{
    ledger_key, make_repo, read_or_null, reload, replace_strings, repo_root, sleeper, stub,
    substitute, PID, STAMP,
};
use crate::support::{module, AttrPatch, Case};
use pyo3::prelude::*;
use pyo3::types::PyDict;
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};

fn label(case: &Value) -> &str {
    case["id"].as_str().unwrap()
}

fn seed(case: &Value) -> &Value {
    &case["seed"]
}

pub fn report_post(case: &Value, parent: &Path, env: &mut Case) -> Value {
    let store = parent.join(format!("{}-store", label(case)));
    fs::create_dir(&store).unwrap();
    if let Some(failure) = seed(case)["refresh_failed"].as_str() {
        fs::write(store.join("refresh.failed"), failure).unwrap();
    }
    env.set_env("CRG_DATA_DIR", store.to_str().unwrap());
    let output = Python::attach(|py| {
        reload(py, "crg_gate");
        let refresh = reload(py, "crg_graph_refresh");
        json_value(
            &refresh
                .getattr("failure_output")
                .unwrap()
                .call1(("PostToolUse",))
                .unwrap(),
        )
    });
    json!({"output": output, "failed_after": read_or_null(&store.join("refresh.failed"))})
}

pub fn graph_bash(case: &Value, parent: &Path, env: &mut Case, base_path: &str) -> Value {
    let repo = make_repo(parent, label(case));
    let bin = parent.join(format!("{}-bin", label(case)));
    stub(
        &bin,
        if seed(case)["stub_tool"] == true {
            &["code-review-graph"]
        } else {
            &[]
        },
    );
    let store = parent.join(format!("{}-crgdata", label(case)));
    fs::create_dir(&store).unwrap();
    env.set_env("PATH", bin.to_str().unwrap());
    env.set_env("CRG_GATE_REPO_ROOT", repo.to_str().unwrap());
    env.set_env("CRG_DATA_DIR", store.to_str().unwrap());
    let output = Python::attach(|py| {
        reload(py, "crg_gate");
        let refresh = reload(py, "crg_graph_refresh");
        let _worker = AttrPatch::replace(refresh.as_any(), "worker_command", sleeper(py).as_any());
        let command = case["payload"]["tool_input"]["command"]
            .as_str()
            .unwrap_or("");
        let rewrite = module(py, "tooling.hooks.dispatch.adapters")
            .getattr("GIT_TREE_REWRITE")
            .unwrap();
        if command.is_empty()
            || rewrite
                .call_method1("search", (command,))
                .unwrap()
                .is_none()
        {
            json!({"hookSpecificOutput": {"hookEventName": "PostToolUse"}})
        } else {
            json_value(
                &refresh
                    .getattr("full_update_output")
                    .unwrap()
                    .call0()
                    .unwrap(),
            )
        }
    });
    env.set_env("PATH", base_path);
    json!({"output": output, "pending": read_or_null(&store.join("refresh.pending"))})
}

pub fn read_budget(case: &Value, parent: &Path, env: &mut Case) -> Value {
    let gate = parent.join(format!("{}-gate", label(case)));
    fs::create_dir(&gate).unwrap();
    if let Some(ledger) = seed(case)["ledger"].as_str() {
        let session = case["payload"]["session_id"].as_str().unwrap();
        fs::write(
            gate.join(format!("{}.read-tokens", ledger_key(session))),
            format!("{ledger}\n"),
        )
        .unwrap();
    }
    env.set_env("CRG_GATE_STATE_DIR", gate.to_str().unwrap());
    let (output, state_dir) = Python::attach(|py| {
        let crg = reload(py, "crg_gate");
        let state = crg.getattr("_state_dir").unwrap().call0().unwrap();
        let state_dir: String = state.str().unwrap().extract().unwrap();
        let budget = module(py, "read_budget");
        let result = budget
            .getattr("hook_output")
            .unwrap()
            .call1((py_json(py, case["payload"].clone()), state))
            .unwrap();
        (json_value(&result), PathBuf::from(state_dir))
    });
    let ledger_after = case["payload"]["session_id"]
        .as_str()
        .filter(|session| !session.is_empty())
        .map(|session| {
            read_or_null(&state_dir.join(format!("{}.read-tokens", ledger_key(session))))
        })
        .unwrap_or(Value::Null);
    json!({"output": output, "ledger_after": ledger_after})
}

pub fn telemetry_record(case: &Value) -> Value {
    Python::attach(|py| {
        let telemetry = module(py, "conductor.context_telemetry");
        let record = telemetry
            .getattr("event")
            .unwrap()
            .call1((py_json(py, case["payload"].clone()),))
            .unwrap();
        record.set_item("timestamp", STAMP).unwrap();
        record.set_item("pid", PID).unwrap();
        let encoded = telemetry
            .getattr("_encoded_record")
            .unwrap()
            .call1((record,))
            .unwrap();
        let line: String = encoded
            .call_method1("decode", ("utf-8",))
            .unwrap()
            .extract()
            .unwrap();
        json!({"line": line})
    })
}

pub fn telemetry_hook_context(case: &Value) -> Value {
    Python::attach(|py| {
        let telemetry = module(py, "conductor.context_telemetry");
        let options = PyDict::new(py);
        options
            .set_item(
                "session_id",
                case["payload"]["session_id"].as_str().unwrap(),
            )
            .unwrap();
        let record = telemetry
            .getattr("hook_context_event")
            .unwrap()
            .call(
                (
                    seed(case)["hook"].as_str().unwrap(),
                    py_json(py, seed(case)["hook_json"].clone()),
                ),
                Some(&options),
            )
            .unwrap();
        record.set_item("timestamp", STAMP).unwrap();
        record.set_item("pid", PID).unwrap();
        let encoded = telemetry
            .getattr("_encoded_record")
            .unwrap()
            .call1((record,))
            .unwrap();
        let line: String = encoded
            .call_method1("decode", ("utf-8",))
            .unwrap()
            .extract()
            .unwrap();
        json!({"line": line})
    })
}

pub fn telemetry_path(case: &Value) -> Value {
    if let Some(override_path) = case["env"]["CONTEXT_TELEMETRY_PATH"]
        .as_str()
        .filter(|path| !path.is_empty())
    {
        return json!({"path": override_path});
    }
    let default = Python::attach(|py| {
        module(py, "conductor.context_telemetry")
            .getattr("DEFAULT_PATH")
            .unwrap()
            .str()
            .unwrap()
            .extract::<String>()
            .unwrap()
    });
    let path = PathBuf::from(default);
    let root = repo_root();
    let suffix = path
        .strip_prefix(&root)
        .map(Path::to_path_buf)
        .unwrap_or_else(|_| {
            let tail = path.iter().rev().take(3).collect::<Vec<_>>();
            tail.into_iter().rev().collect()
        });
    json!({"path_suffix": suffix.to_string_lossy()})
}

pub fn post_edit(case: &Value, parent: &Path, env: &mut Case, base_path: &str) -> Value {
    let file_dir = parent.join(format!("{}-file", label(case)));
    fs::create_dir(&file_dir).unwrap();
    let name = seed(case)["file"].as_str().unwrap_or("ghost.py");
    let file = file_dir.join(name);
    if let Some(content) = seed(case)["content"].as_str() {
        fs::write(&file, content).unwrap();
    }
    let bin = parent.join(format!("{}-bin", label(case)));
    stub(&bin, &["ruff", "rustfmt"]);
    env.set_env("PATH", bin.to_str().unwrap());
    let payload = substitute(
        case["payload"].clone(),
        &[("<FILE>".to_owned(), file.to_string_lossy().into_owned())],
    );
    let output = Python::attach(|py| {
        let audit = module(py, "_post_edit_audit");
        json_value(
            &audit
                .getattr("hook_output")
                .unwrap()
                .call1((py_json(py, payload),))
                .unwrap(),
        )
    });
    env.set_env("PATH", base_path);
    json!({"output": replace_strings(output, &[(file_dir.to_string_lossy().into_owned(), "<F>".to_owned())])})
}

pub fn graph_edit(case: &Value, parent: &Path, env: &mut Case, base_path: &str) -> Value {
    let repo = make_repo(parent, label(case));
    if let Some(files) = seed(case)["files"].as_object() {
        for (name, content) in files {
            fs::write(repo.join(name), content.as_str().unwrap()).unwrap();
        }
    }
    let outside = parent.join(format!("{}-outside", label(case)));
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("x.py"), "y = 2\n").unwrap();
    let bin = parent.join(format!("{}-bin", label(case)));
    stub(
        &bin,
        if seed(case)["stub_tool"] == true {
            &["code-review-graph"]
        } else {
            &[]
        },
    );
    let store = parent.join(format!("{}-crgdata", label(case)));
    fs::create_dir(&store).unwrap();
    env.set_env("PATH", bin.to_str().unwrap());
    env.set_env("CRG_GATE_REPO_ROOT", repo.to_str().unwrap());
    env.set_env("CRG_DATA_DIR", store.to_str().unwrap());
    let payload = substitute(
        case["payload"].clone(),
        &[(
            "<OUTSIDE>".to_owned(),
            outside.join("x.py").to_string_lossy().into_owned(),
        )],
    );
    let output = Python::attach(|py| {
        reload(py, "crg_gate");
        let refresh = reload(py, "crg_graph_refresh");
        let _worker = AttrPatch::replace(refresh.as_any(), "worker_command", sleeper(py).as_any());
        json_value(
            &refresh
                .getattr("hook_output")
                .unwrap()
                .call1((py_json(py, payload),))
                .unwrap(),
        )
    });
    env.set_env("PATH", base_path);
    json!({"output": output, "pending": read_or_null(&store.join("refresh.pending"))})
}
