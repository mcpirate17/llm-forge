//! Rust fixtures for the gate and gate-rollout Python API contract targets.

use crate::support::{module, AttrPatch, Case};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyModule};
use serde_json::Value;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Keep ambient repository selectors and config from redirecting fixture Git.
/// Removing CONFIG_COUNT disables any ambient GIT_CONFIG_KEY_n/VALUE_n pairs;
/// the command-level environment is also cleared for fixture creation.
pub fn isolated_case() -> Case {
    let mut case = Case::new();
    for name in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_NAMESPACE",
        "GIT_CEILING_DIRECTORIES",
        "GIT_DISCOVERY_ACROSS_FILESYSTEM",
        "GIT_CONFIG_COUNT",
        "GIT_CONFIG_PARAMETERS",
        "GIT_CONFIG_KEY_0",
        "GIT_CONFIG_VALUE_0",
        "GIT_CONFIG",
        "GIT_TEMPLATE_DIR",
    ] {
        case.remove_env(name);
    }
    case.set_env("GIT_CONFIG_NOSYSTEM", "1");
    case.set_env("GIT_CONFIG_GLOBAL", "/dev/null");
    case.set_env("GIT_CONFIG_SYSTEM", "/dev/null");
    case
}

pub fn default_tool(py: Python<'_>) -> Bound<'_, PyAny> {
    tool(py, "sample", "sample", "1.2.3", &["fast", "full"])
}

pub fn tool<'py>(
    py: Python<'py>,
    tool_id: &str,
    executable: &str,
    expected_version: &str,
    required_profiles: &[&str],
) -> Bound<'py, PyAny> {
    let kwargs = PyDict::new(py);
    kwargs.set_item("tool_id", tool_id).unwrap();
    kwargs.set_item("executable", executable).unwrap();
    kwargs
        .set_item("version_command", (executable, "--version"))
        .unwrap();
    kwargs
        .set_item("expected_version", expected_version)
        .unwrap();
    kwargs
        .set_item("required_profiles", required_profiles)
        .unwrap();
    kwargs.set_item("provided_by", "test fixture").unwrap();
    kwargs.set_item("rationale", "test fixture").unwrap();
    module(py, "conductor.candidate_review.policy")
        .getattr("ToolPolicy")
        .unwrap()
        .call((), Some(&kwargs))
        .unwrap()
}

pub fn fake_executable(directory: &Path, name: &str, output: &str, exit_code: i32) -> PathBuf {
    fs::create_dir_all(directory).expect("create fake executable directory");
    let path = directory.join(name);
    fs::write(
        &path,
        format!("#!/bin/sh\necho '{output}'\nexit {exit_code}\n"),
    )
    .expect("write fake executable");
    let mut permissions = fs::metadata(&path)
        .expect("fake executable metadata")
        .permissions();
    use std::os::unix::fs::PermissionsExt;
    permissions.set_mode(0o755);
    fs::set_permissions(&path, permissions).expect("make fake executable runnable");
    path
}

pub fn repo(case: &Case) -> PathBuf {
    let root = case.mkdir("repo");
    git(&root, case.root(), &["init", "--quiet", "-b", "main"]);
    git(
        &root,
        case.root(),
        &["config", "user.email", "test@example.invalid"],
    );
    git(&root, case.root(), &["config", "user.name", "test"]);
    fs::write(root.join("tracked.py"), "VALUE = 1\n").expect("write tracked fixture");
    git(&root, case.root(), &["add", "tracked.py"]);
    git(&root, case.root(), &["commit", "--quiet", "-m", "first"]);
    root
}

fn git(repo: &Path, home: &Path, args: &[&str]) {
    let path = env::var_os("PATH").unwrap_or_default();
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .env_clear()
        .env("PATH", path)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .expect("run isolated Git fixture command");
    assert_git_ok(args, output);
}

fn assert_git_ok(args: &[&str], output: Output) {
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

pub fn py_json<'py>(py: Python<'py>, value: Value) -> Bound<'py, PyAny> {
    PyModule::import(py, "json")
        .unwrap()
        .getattr("loads")
        .unwrap()
        .call1((value.to_string(),))
        .unwrap()
}

pub fn json_value(value: &Bound<'_, PyAny>) -> Value {
    let encoded: String = PyModule::import(value.py(), "json")
        .unwrap()
        .getattr("dumps")
        .unwrap()
        .call1((value,))
        .unwrap()
        .extract()
        .unwrap();
    serde_json::from_str(&encoded).unwrap()
}

pub fn capture<'py>(py: Python<'py>, name: &str) -> (Bound<'py, PyAny>, AttrPatch) {
    let buffer = PyModule::import(py, "io")
        .unwrap()
        .getattr("StringIO")
        .unwrap()
        .call0()
        .unwrap();
    let sys = PyModule::import(py, "sys").unwrap();
    let patch = AttrPatch::replace(sys.as_any(), name, &buffer);
    (buffer, patch)
}

pub fn buffer_text(buffer: &Bound<'_, PyAny>) -> String {
    buffer.call_method0("getvalue").unwrap().extract().unwrap()
}

pub fn clear_buffer(buffer: &Bound<'_, PyAny>) {
    buffer.call_method1("truncate", (0,)).unwrap();
    buffer.call_method1("seek", (0,)).unwrap();
}
