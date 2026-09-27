//! Isolated fixtures shared by the CRG gate Rust contract binaries.

use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyModule};
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::support::{module, path, text, AttrPatch, Case};

pub fn hook_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("src/tooling/hooks/agent")
        .canonicalize()
        .expect("resolve Forge hook directory")
}

pub fn hook_module<'py>(py: Python<'py>, name: &str) -> Bound<'py, PyModule> {
    PyModule::import(py, "sys")
        .unwrap()
        .getattr("path")
        .unwrap()
        .call_method1("insert", (0, hook_dir().to_str().unwrap()))
        .unwrap();
    let imported = module(py, name);
    PyModule::import(py, "importlib")
        .unwrap()
        .call_method1("reload", (&imported,))
        .unwrap()
        .cast_into::<PyModule>()
        .unwrap()
}

pub fn git(repo: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("LC_ALL", "C")
        .output()
        .expect("run git in isolated CRG fixture");
    assert!(
        output.status.success(),
        "fixture git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

pub struct GateCase {
    pub case: Case,
    pub repo: PathBuf,
    pub linked: Option<PathBuf>,
    pub stranger: Option<PathBuf>,
    gate: Py<PyModule>,
}

impl GateCase {
    pub fn new() -> Self {
        let mut case = Case::new();
        let repo = case.mkdir("repo");
        git(&repo, &["init", "--quiet"]);
        case.write("repo/a.py", "A = 1\n");
        case.write("repo/b.py", "B = 1\n");
        case.remove_env("GOVERNANCE_OWNER");
        case.remove_env("CRG_GATE_ENFORCE_WORKTREES");
        case.set_env("CRG_GATE_REPO_ROOT", repo.to_str().unwrap());
        case.set_env(
            "CRG_GATE_STATE_DIR",
            case.root().join("state").to_str().unwrap(),
        );
        let gate = Python::attach(|py| {
            let ownership = module(py, "conductor.candidate_review.ownership");
            for (owner, file) in [("codex-phase22", "a.py"), ("claude", "b.py")] {
                let kwargs = PyDict::new(py);
                kwargs.set_item("owner", owner).unwrap();
                kwargs.set_item("paths", vec![file]).unwrap();
                kwargs.set_item("justification", "j").unwrap();
                kwargs.set_item("max_minutes", 60).unwrap();
                ownership
                    .getattr("create_claim")
                    .unwrap()
                    .call((path(py, &repo),), Some(&kwargs))
                    .unwrap();
            }
            hook_module(py, "crg_gate").unbind()
        });
        Self {
            case,
            repo,
            linked: None,
            stranger: None,
            gate,
        }
    }

    pub fn with_worktree() -> Self {
        let mut fixture = Self::new();
        git(&fixture.repo, &["add", "--", "a.py", "b.py"]);
        git(
            &fixture.repo,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-qm",
                "init",
            ],
        );
        let linked = fixture.case.root().join("linked");
        git(
            &fixture.repo,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                "side",
                linked.to_str().unwrap(),
            ],
        );
        let stranger = fixture.case.mkdir("stranger");
        git(&stranger, &["init", "--quiet"]);
        fixture.linked = Some(linked);
        fixture.stranger = Some(stranger);
        fixture
    }

    pub fn gate<'py>(&self, py: Python<'py>) -> Bound<'py, PyModule> {
        self.gate.bind(py).clone()
    }

    pub fn linked(&self) -> &Path {
        self.linked.as_deref().expect("linked worktree fixture")
    }

    pub fn stranger(&self) -> &Path {
        self.stranger
            .as_deref()
            .expect("foreign repository fixture")
    }

    pub fn exposure_log(&self) -> PathBuf {
        self.repo.join(".git/governance/claim-gate-exposure.jsonl")
    }
}

pub fn payload<'py>(py: Python<'py>, value: &Value) -> Bound<'py, PyAny> {
    PyModule::import(py, "json")
        .unwrap()
        .getattr("loads")
        .unwrap()
        .call1((value.to_string(),))
        .unwrap()
}

pub fn capture_stdout<'py>(py: Python<'py>) -> (Bound<'py, PyAny>, AttrPatch) {
    let output = PyModule::import(py, "io")
        .unwrap()
        .getattr("StringIO")
        .unwrap()
        .call0()
        .unwrap();
    let patch = AttrPatch::replace(&PyModule::import(py, "sys").unwrap(), "stdout", &output);
    (output, patch)
}

pub fn decision(
    py: Python<'_>,
    gate: &Bound<'_, PyModule>,
    action: &str,
    value: &Value,
    owner: &str,
    graph_used: bool,
) -> String {
    let payload = payload(py, value);
    if graph_used {
        let key = gate
            .getattr("_state_key")
            .unwrap()
            .call1((&payload,))
            .unwrap();
        let dir = gate.getattr("_state_dir").unwrap().call0().unwrap();
        let state = gate
            .getattr("_state_path")
            .unwrap()
            .call1((dir, key, "graph-used"))
            .unwrap();
        gate.getattr("_write_state")
            .unwrap()
            .call1((state,))
            .unwrap();
    }
    let (output, _capture) = capture_stdout(py);
    let kwargs = PyDict::new(py);
    kwargs.set_item("owner", owner).unwrap();
    let result: i32 = gate
        .getattr(action)
        .unwrap()
        .call((&payload,), Some(&kwargs))
        .unwrap()
        .extract()
        .unwrap();
    assert_eq!(result, 0);
    let emitted = text(&output.call_method0("getvalue").unwrap());
    if emitted.trim().is_empty() {
        return "allow".to_owned();
    }
    let json: Value = serde_json::from_str(emitted.trim()).expect("gate emits JSON decision");
    json["hookSpecificOutput"]["permissionDecisionReason"]
        .as_str()
        .expect("gate denial reason")
        .to_owned()
}

pub fn edit_value(file: &Path) -> Value {
    serde_json::json!({
        "session_id": "s1",
        "tool_name": "Edit",
        "tool_input": {"file_path": file.to_str().unwrap()},
    })
}

pub fn bash_value(command: &str, cwd: Option<&Path>) -> Value {
    let mut value = serde_json::json!({
        "session_id": "s1",
        "tool_name": "Bash",
        "tool_input": {"command": command},
    });
    if let Some(cwd) = cwd {
        value["cwd"] = Value::String(cwd.to_str().unwrap().to_owned());
    }
    value
}

pub fn claim_result(gate: &Bound<'_, PyModule>, owner: &str, file: &str) -> (bool, String) {
    gate.getattr("_claim_allows")
        .unwrap()
        .call1((owner, file))
        .unwrap()
        .extract()
        .unwrap()
}

pub fn read_exposure_log(fixture: &GateCase) -> String {
    fs::read_to_string(fixture.exposure_log()).expect("read claim exposure log")
}
