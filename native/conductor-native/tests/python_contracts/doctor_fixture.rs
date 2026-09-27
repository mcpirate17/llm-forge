//! Isolated settings fixtures for the conductor doctor contract.

use crate::comm_support::json_value;
use crate::support::{module, path, Case};
use pyo3::prelude::*;
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};

pub struct DoctorCase {
    case: Case,
    project: PathBuf,
    home: PathBuf,
}

impl DoctorCase {
    pub fn new() -> Self {
        let case = Case::new();
        let project = case.root().join("p");
        let home = case.root().join("u");
        Self {
            case,
            project,
            home,
        }
    }

    pub fn project(&self) -> &Path {
        &self.project
    }

    pub fn home(&self) -> &Path {
        &self.home
    }

    pub fn set_env(&mut self, name: &'static str, value: &str) {
        self.case.set_env(name, value);
    }

    pub fn write_project(&self, payload: &Value) {
        write_settings(&self.project, payload);
    }

    pub fn write_user(&self, payload: &Value) {
        write_settings(&self.home, payload);
    }

    pub fn read_project(&self) -> Value {
        read_settings(&self.project)
    }

    pub fn user_bytes(&self) -> Vec<u8> {
        fs::read(settings_path(&self.home)).expect("read user settings")
    }

    pub fn project_bytes(&self) -> Vec<u8> {
        fs::read(settings_path(&self.project)).expect("read project settings")
    }

    pub fn write_raw_project(&self, serialized: &str) {
        let file = settings_path(&self.project);
        fs::create_dir_all(file.parent().unwrap()).expect("create settings directory");
        fs::write(file, serialized).expect("write raw settings");
    }

    pub fn run(&self, py: Python<'_>, flags: &[&str]) -> i32 {
        let mut args = vec![
            "--harness".to_owned(),
            "--project-dir".to_owned(),
            self.project.display().to_string(),
            "--home".to_owned(),
            self.home.display().to_string(),
        ];
        args.extend(flags.iter().map(|flag| (*flag).to_owned()));
        call_main(py, &args)
    }

    pub fn diagnose<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, pyo3::types::PyAny>> {
        doctor(py)
            .getattr("diagnose")?
            .call1((path(py, &self.project), path(py, &self.home)))
    }
}

pub fn settings_path(root: &Path) -> PathBuf {
    root.join(".claude/settings.json")
}

pub fn write_settings(root: &Path, payload: &Value) {
    let file = settings_path(root);
    fs::create_dir_all(file.parent().unwrap()).expect("create settings directory");
    fs::write(
        file,
        format!("{}\n", serde_json::to_string_pretty(payload).unwrap()),
    )
    .expect("write settings");
}

pub fn read_settings(root: &Path) -> Value {
    serde_json::from_slice(&fs::read(settings_path(root)).expect("read settings"))
        .expect("parse settings")
}

pub fn doctor(py: Python<'_>) -> Bound<'_, pyo3::types::PyModule> {
    module(py, "conductor.doctor")
}

pub fn healthy(py: Python<'_>) -> Value {
    let registry = module(py, "tooling.hooks.dispatch.registry");
    let block = registry.getattr("settings_block").unwrap().call0().unwrap();
    let mut payload = json_value(&block);
    payload["env"] = json!({"BASH_QUIET_LIMIT_BYTES": "8000"});
    payload["subagentPromptCacheTtl"] = json!("1h");
    payload
}

pub fn canonical_hooks(py: Python<'_>) -> Value {
    let registry = module(py, "tooling.hooks.dispatch.registry");
    let block = registry.getattr("settings_block").unwrap().call0().unwrap();
    json_value(&block)["hooks"].clone()
}

pub fn call_main(py: Python<'_>, args: &[String]) -> i32 {
    doctor(py)
        .getattr("main")
        .unwrap()
        .call1((args.to_vec(),))
        .unwrap()
        .extract()
        .unwrap()
}
