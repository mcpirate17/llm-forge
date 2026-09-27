//! Rust-owned fixtures shared by cost-budget and ledger Python API contracts.

use crate::comm_support::{bind_signature, py_json, signature};
use crate::support::{module, path, AttrPatch, Case};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyCFunction, PyDict, PyList, PyModule};
use serde_json::{json, Value};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub fn equal(actual: &Bound<'_, PyAny>, expected: &Bound<'_, PyAny>) {
    assert!(
        actual.eq(expected).unwrap(),
        "actual={actual:?}, expected={expected:?}"
    );
}

pub fn strict_callback<'py, F>(
    py: Python<'py>,
    positional: &[&str],
    keyword_only: &[&str],
    body: F,
) -> Bound<'py, PyCFunction>
where
    F: Fn(Python<'_>, Bound<'_, PyAny>) -> PyResult<Py<PyAny>> + Send + Sync + 'static,
{
    let sig = signature(py, positional, keyword_only);
    PyCFunction::new_closure(py, None, None, move |args, kwargs| {
        let bound = bind_signature(&sig, args, kwargs)?;
        body(args.py(), bound.getattr("arguments")?)
    })
    .unwrap()
}

pub fn kwargs_callback<'py, F>(py: Python<'py>, body: F) -> Bound<'py, PyCFunction>
where
    F: Fn(Python<'_>, Option<&Bound<'_, PyDict>>) -> PyResult<Py<PyAny>> + Send + Sync + 'static,
{
    PyCFunction::new_closure(py, None, None, move |args, kwargs| {
        if !args.is_empty() {
            return Err(pyo3::exceptions::PyTypeError::new_err(
                "unexpected positional arguments",
            ));
        }
        body(args.py(), kwargs)
    })
    .unwrap()
}

pub fn completed<'py>(
    py: Python<'py>,
    payload: &str,
    returncode: i32,
    stderr: &str,
    args: &[&str],
) -> Bound<'py, PyAny> {
    let subprocess = py.import("subprocess").unwrap();
    subprocess
        .getattr("CompletedProcess")
        .unwrap()
        .call1((PyList::new(py, args).unwrap(), returncode, payload, stderr))
        .unwrap()
}

pub fn metric(status: &str, value: Option<f64>, baseline: Option<f64>) -> Value {
    json!({
        "value":value,
        "n":10,
        "baseline":baseline,
        "delta_pct":baseline.map(|base| (value.unwrap_or(0.0)-base)/base*100.0),
        "status":status
    })
}

pub fn audit_payload(status: &str, metric_status: &str) -> Value {
    let mut metrics = serde_json::Map::new();
    for name in [
        "median_hook_ms",
        "resend_bytes_per_session",
        "tokens_per_landed_pr",
        "cap_breach_rate",
        "cheap_tier_rework_rate",
    ] {
        metrics.insert(name.to_owned(), metric(metric_status, Some(1.0), Some(0.9)));
    }
    json!({
        "window":{"from":"2026-09-06","to":"2026-09-13","days":7},
        "metrics":metrics,
        "status":status
    })
}

pub fn patch_constant<'py>(
    py: Python<'py>,
    subject: &Bound<'py, PyAny>,
    name: &str,
    value: &Bound<'py, PyAny>,
    positional: &[&str],
) -> AttrPatch {
    let captured = value.clone().unbind();
    let callback = strict_callback(py, positional, &[], move |py, _args| {
        Ok(captured.clone_ref(py))
    });
    AttrPatch::replace(subject, name, callback.as_any())
}

pub fn patch_kwargs_result(
    py: Python<'_>,
    subject: &Bound<'_, PyAny>,
    name: &str,
    value: &Bound<'_, PyAny>,
) -> AttrPatch {
    let captured = value.clone().unbind();
    let callback = kwargs_callback(py, move |py, _kwargs| Ok(captured.clone_ref(py)));
    AttrPatch::replace(subject, name, callback.as_any())
}

pub fn json_python<'py>(py: Python<'py>, value: Value) -> Bound<'py, PyAny> {
    py_json(py, value)
}

pub fn forge_script(case: &mut Case) -> (PathBuf, PathBuf, PathBuf) {
    let script = case.write(
        "stub-forge.sh",
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$FORGE_ARGV\"\nexit \"${FORGE_EXIT:-0}\"\n",
    );
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    let binary = case.root().join("forge");
    std::os::unix::fs::symlink(&script, &binary).unwrap();
    let argv_file = case.root().join("argv.txt");
    case.set_env("FORGE_ARGV", argv_file.to_str().unwrap());
    (binary, script, argv_file)
}

pub fn write_script(script: &Path, contents: &str) {
    fs::write(script, contents).unwrap();
    fs::set_permissions(script, fs::Permissions::from_mode(0o700)).unwrap();
}

pub fn config<'py>(py: Python<'py>, root: &Path) -> Bound<'py, PyAny> {
    let transcripts = root.join("-home-x-project");
    fs::create_dir(&transcripts).unwrap();
    let kwargs = PyDict::new(py);
    kwargs
        .set_item("ledger_root", path(py, &root.join("ledger")))
        .unwrap();
    kwargs
        .set_item("repo_path", path(py, &root.join("repo")))
        .unwrap();
    kwargs.set_item("project", "-home-x-project").unwrap();
    kwargs
        .set_item("transcripts_dir", path(py, &transcripts))
        .unwrap();
    kwargs
        .set_item(
            "baseline",
            path(py, &root.join("repo/ledger/cost_budget_baseline.json")),
        )
        .unwrap();
    module(py, "conductor.cost_ledger")
        .getattr("LedgerConfig")
        .unwrap()
        .call((), Some(&kwargs))
        .unwrap()
}

pub fn argv(path: &Path) -> Vec<String> {
    fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect()
}

/// Replaces a sys.modules entry and restores the exact prior object on drop.
pub struct ModulePatch {
    modules: Py<PyDict>,
    name: String,
    old: Option<Py<PyAny>>,
}

impl ModulePatch {
    pub fn replace(py: Python<'_>, name: &str, value: &Bound<'_, PyAny>) -> Self {
        let modules = PyModule::import(py, "sys")
            .unwrap()
            .getattr("modules")
            .unwrap()
            .cast_into::<PyDict>()
            .unwrap();
        let old = modules.get_item(name).unwrap().map(Bound::unbind);
        modules.set_item(name, value).unwrap();
        Self {
            modules: modules.unbind(),
            name: name.to_owned(),
            old,
        }
    }
}

impl Drop for ModulePatch {
    fn drop(&mut self) {
        Python::attach(|py| {
            let modules = self.modules.bind(py);
            match &self.old {
                Some(value) => modules.set_item(&self.name, value.bind(py)).unwrap(),
                None => modules.del_item(&self.name).unwrap(),
            }
        });
    }
}

type TokenCalls = Arc<Mutex<Vec<(Py<PyAny>, Py<PyAny>)>>>;

/// Recorded count_tokens responses, preserving Python request objects for exact assertions.
pub struct TokenStub {
    responses: Arc<Mutex<Vec<i64>>>,
    calls: TokenCalls,
}

impl TokenStub {
    pub fn new(values: &[i64]) -> Self {
        Self {
            responses: Arc::new(Mutex::new(values.to_vec())),
            calls: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn install(&self, py: Python<'_>, case: &mut Case) -> (ModulePatch, Py<PyAny>) {
        case.set_env("ANTHROPIC_API_KEY", "test-key");
        let responses = Arc::clone(&self.responses);
        let calls = Arc::clone(&self.calls);
        let count = strict_callback(py, &["model", "messages"], &[], move |py, args| {
            let model = args.get_item("model")?.unbind();
            let messages = args.get_item("messages")?.unbind();
            calls.lock().unwrap().push((model, messages));
            let next = responses.lock().unwrap().remove(0);
            let result = PyDict::new(py);
            result.set_item("input_tokens", next)?;
            Ok(result.into_any().unbind())
        });
        let fields = PyDict::new(py);
        fields.set_item("count_tokens", &count).unwrap();
        let client = py
            .import("types")
            .unwrap()
            .getattr("SimpleNamespace")
            .unwrap()
            .call((), Some(&fields))
            .unwrap();
        client.setattr("messages", &client).unwrap();
        let client_ref = client.unbind();
        let for_test = client_ref.clone_ref(py);
        let anthropic_ctor = strict_callback(py, &["api_key"], &[], move |py, _args| {
            Ok(client_ref.clone_ref(py))
        });
        let module = PyModule::new(py, "anthropic").unwrap();
        module.add("Anthropic", anthropic_ctor).unwrap();
        (
            ModulePatch::replace(py, "anthropic", module.as_any()),
            for_test,
        )
    }

    pub fn calls(&self) -> &TokenCalls {
        &self.calls
    }
}

pub fn assistant_line(uuid: &str, model: &str) -> String {
    json!({
        "uuid":uuid,
        "type":"assistant",
        "message":{
            "role":"assistant",
            "model":model,
            "usage":{"input_tokens":1,"output_tokens":1},
            "content":[{"type":"text","text":"x"}]
        }
    })
    .to_string()
}

pub fn sample_row(uuid: &str, billed: i64, session: &str) -> Value {
    json!({
        "session_id":session,
        "turn_uuid":uuid,
        "turn_index":0,
        "bytes_by_block_type":{
            "text":1,"tool_result":0,"tool_use":0,"image":0,"other":0,"thinking":0
        },
        "billed_input":billed
    })
}

pub fn tiny_transcript(case: &Case) -> PathBuf {
    let user = json!({
        "type":"user",
        "message":{"role":"user","content":[{"type":"text","text":"hello"}]}
    });
    case.write(
        "t.jsonl",
        &format!("{user}\n{}\n", assistant_line("t1", "claude-sonnet-5")),
    )
}

pub fn window<'py>(
    py: Python<'py>,
    session: &str,
    billed: i64,
    text_chars: i64,
) -> Bound<'py, PyAny> {
    let kwargs = PyDict::new(py);
    kwargs.set_item("session_id", session).unwrap();
    kwargs
        .set_item("turn_uuid", format!("{session}-{billed}"))
        .unwrap();
    kwargs.set_item("model", "m").unwrap();
    kwargs.set_item("billed_input", billed).unwrap();
    kwargs
        .set_item(
            "chars_by_block_type",
            py_json(
                py,
                json!({
                    "text":text_chars,"tool_result":0,"tool_use":0,
                    "image":0,"other":0,"thinking":0
                }),
            ),
        )
        .unwrap();
    let payloads = PyDict::new(py);
    for kind in ["text", "tool_result", "tool_use", "image", "other"] {
        payloads.set_item(kind, PyList::empty(py)).unwrap();
    }
    kwargs.set_item("payloads", payloads).unwrap();
    module(py, "conductor.ledger_calibrate")
        .getattr("TurnWindow")
        .unwrap()
        .call((), Some(&kwargs))
        .unwrap()
}

pub fn full_window<'py>(py: Python<'py>) -> Bound<'py, PyAny> {
    let kwargs = PyDict::new(py);
    kwargs.set_item("session_id", "s").unwrap();
    kwargs.set_item("turn_uuid", "u").unwrap();
    kwargs.set_item("model", "m").unwrap();
    kwargs.set_item("billed_input", 100).unwrap();
    kwargs
        .set_item(
            "chars_by_block_type",
            py_json(
                py,
                json!({
                    "text":60,"tool_result":40,"tool_use":0,
                    "image":0,"other":0,"thinking":0
                }),
            ),
        )
        .unwrap();
    let text = "x".repeat(60);
    let result = "y".repeat(40);
    let payloads = py_json(
        py,
        json!({
            "text":[{"type":"text","text":text}],
            "tool_result":[{"type":"tool_result","content":result}],
            "tool_use":[],"image":[],"other":[]
        }),
    );
    kwargs.set_item("payloads", payloads).unwrap();
    module(py, "conductor.ledger_calibrate")
        .getattr("TurnWindow")
        .unwrap()
        .call((), Some(&kwargs))
        .unwrap()
}
