//! Rust-owned scratch fixtures and normalization for the PostToolUse corpus.

use crate::comm_support::{bind_signature, signature};
use crate::support::{module, Case};
use pyo3::prelude::*;
use pyo3::types::{PyCFunction, PyModule};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

pub const STAMP: &str = "2026-09-13T00:00:00.000+00:00";
pub const PID: i64 = 3_831_796;
pub const DATE_STAMP: &str = "2026-09-13";
pub const MANAGED_ENV: &[&str] = &[
    "CRG_GATE_REPO_ROOT",
    "CRG_DATA_DIR",
    "CRG_GATE_STATE_DIR",
    "READ_BUDGET_STEP_TOKENS",
    "QWEN_PROJECT_DIR",
    "CONTEXT_TELEMETRY_PATH",
    "CLAUDE_PROJECT_DIR",
    "CONDUCTOR_SNAPSHOT_PYTHON",
    "PROJECT_DIR",
    "OBSIDIAN_VAULT_ROOT",
    "CLAUDE_MEMORY_ROOT",
    "HOME",
];

pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

pub fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../forge/tests/fixtures")
        .join(name)
}

pub fn load(name: &str) -> Value {
    serde_json::from_str(&fs::read_to_string(fixture(name)).unwrap()).unwrap()
}

pub fn install_paths(py: Python<'_>) {
    let root = repo_root();
    let sys_path = module(py, "sys").getattr("path").unwrap();
    for suffix in ["src/tooling/hooks/agent", "src/tooling/hooks/claude"] {
        let directory = root.join(suffix);
        sys_path
            .call_method1("insert", (0, directory.to_str().unwrap()))
            .unwrap();
    }
}

pub fn reset_env(case: &mut Case) {
    for name in MANAGED_ENV {
        case.remove_env(name);
    }
}

pub fn apply_env(case: &mut Case, entries: &Value) {
    if let Some(entries) = entries.as_object() {
        for (key, value) in entries {
            let key: &'static str = MANAGED_ENV
                .iter()
                .copied()
                .find(|managed| managed == key)
                .expect("corpus may set only managed environment keys");
            case.set_env(key, value.as_str().unwrap());
        }
    }
}

pub fn reload<'py>(py: Python<'py>, name: &str) -> Bound<'py, PyModule> {
    let imported = module(py, name);
    module(py, "importlib")
        .getattr("reload")
        .unwrap()
        .call1((&imported,))
        .unwrap()
        .cast_into::<PyModule>()
        .unwrap()
}

pub fn make_repo(parent: &Path, label: &str) -> PathBuf {
    let repo = parent.join(format!("{label}-repo"));
    fs::create_dir_all(repo.join(".git")).unwrap();
    fs::write(repo.join(".git/HEAD"), "ref: refs/heads/lane\n").unwrap();
    repo
}

pub fn stub(bin_dir: &Path, tools: &[&str]) {
    fs::create_dir_all(bin_dir).unwrap();
    for tool in tools {
        let file = bin_dir.join(tool);
        fs::write(&file, "#!/bin/sh\nexit 0\n").unwrap();
        let mut permissions = fs::metadata(&file).unwrap().permissions();
        permissions.set_mode(permissions.mode() | 0o111);
        fs::set_permissions(file, permissions).unwrap();
    }
}

pub fn replace_strings(value: Value, mapping: &[(String, String)]) -> Value {
    match value {
        Value::String(mut text) => {
            for (old, new) in mapping {
                text = text.replace(old, new);
            }
            Value::String(text)
        }
        Value::Array(items) => Value::Array(
            items
                .into_iter()
                .map(|item| replace_strings(item, mapping))
                .collect(),
        ),
        Value::Object(items) => Value::Object(
            items
                .into_iter()
                .map(|(key, item)| (key, replace_strings(item, mapping)))
                .collect(),
        ),
        other => other,
    }
}

pub fn substitute(value: Value, mapping: &[(String, String)]) -> Value {
    replace_strings(value, mapping)
}

pub fn read_or_null(file: &Path) -> Value {
    if file.exists() {
        Value::String(fs::read_to_string(file).unwrap())
    } else {
        Value::Null
    }
}

pub fn ledger_key(session_id: &str) -> String {
    format!("{:x}", Sha256::digest(session_id.as_bytes()))
}

pub fn sleeper(py: Python<'_>) -> Bound<'_, PyCFunction> {
    let expected = signature(py, &["_body", "_root"], &[]);
    PyCFunction::new_closure(
        py,
        None,
        None,
        move |args, kwargs| -> PyResult<Vec<String>> {
            bind_signature(&expected, args, kwargs)?;
            Ok(vec!["/bin/sleep".to_owned(), "30".to_owned()])
        },
    )
    .unwrap()
}
