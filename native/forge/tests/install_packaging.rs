//! The shipped executable must run after relocation with no compiler, Python,
//! Node, source checkout or virtual environment on its runtime PATH.
#[allow(dead_code)]
mod support;
use serde_json::Value;
use std::fs;
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn command(binary: &Path, root: &Path) -> Command {
    let mut command = Command::new(binary);
    command
        .current_dir(root)
        .env_clear()
        .env("PATH", "")
        .env("HOME", root)
        .env("XDG_STATE_HOME", root.join("state"))
        .env("CARGO", "/nonexistent/cargo")
        .env("RUSTC", "/nonexistent/rustc")
        .env("PYO3_PYTHON", "/nonexistent/python")
        .env("FORGE_LEDGER_DISABLE", "1");
    command
}

fn run_json(binary: &Path, root: &Path, args: &[&str], input: Option<&[u8]>) -> Value {
    let mut child = command(binary, root)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    if let Some(input) = input {
        child.stdin.take().unwrap().write_all(input).unwrap();
    } else {
        drop(child.stdin.take());
    }
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn runtime_smoke(binary: &Path) {
    let host = support::Host::new();
    fs::set_permissions(&host.0, fs::Permissions::from_mode(0o700)).unwrap();
    let version = command(binary, &host.0).arg("--version").output().unwrap();
    assert!(version.status.success());
    assert!(String::from_utf8(version.stdout)
        .unwrap()
        .starts_with(&format!("forge {} ", env!("CARGO_PKG_VERSION"))));
    let deny = run_json(
        binary,
        &host.0,
        &["hook", "PreToolUse", "--host", host.0.to_str().unwrap()],
        Some(br#"{"tool_name":"Bash","tool_input":{"command":"git reset --hard"}}"#),
    );
    assert_eq!(deny["hookSpecificOutput"]["permissionDecision"], "deny");
    fs::write(
        host.0.join("source.rs"),
        "pub fn native_function() -> u32 { 42 }\n",
    )
    .unwrap();
    let syntax = run_json(binary, &host.0, &["code", "inspect", "source.rs"], None);
    assert_eq!(syntax["authority"], "native-lexical-syntax");
    assert_eq!(syntax["symbols"][0]["name"], "native_function");
    fs::write(
        host.0.join("source.py"),
        "def clean_function(value):\n    return value\n",
    )
    .unwrap();
    let analysis = run_json(
        binary,
        &host.0,
        &["analyze", "scan", "source.py", "--checks", "fallback"],
        None,
    );
    assert_eq!(analysis["finding_count"], 0);
    let installed = run_json(
        binary,
        &host.0,
        &["providers", "--provider", "codex", "install", "--apply"],
        None,
    );
    assert_eq!(installed["changed"], true);
    let checked = run_json(
        binary,
        &host.0,
        &["providers", "--provider", "codex", "check"],
        None,
    );
    assert_eq!(checked["configured"], true);
    assert_eq!(checked["services_started"], false);
    assert!(!host.0.join(".agents").exists());
}

#[test]
fn relocated_executable_runs_without_interpreters_compilers_or_source_checkout() {
    let prefix = support::Host::new();
    let directory = prefix.0.join("bin");
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&directory)
        .unwrap();
    let binary = directory.join("forge");
    fs::copy(support::forge_binary::path(), &binary).unwrap();
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();
    runtime_smoke(&binary);
}

#[test]
#[ignore = "requires a native make install prefix in FORGE_INSTALL_ROOT"]
fn installed_executable_runs_without_interpreters_compilers_or_source_checkout() {
    let prefix = PathBuf::from(std::env::var_os("FORGE_INSTALL_ROOT").expect("FORGE_INSTALL_ROOT"))
        .canonicalize()
        .unwrap();
    runtime_smoke(&prefix.join("bin/forge"));
}
