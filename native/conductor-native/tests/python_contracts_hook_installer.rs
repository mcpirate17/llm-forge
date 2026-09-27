#![cfg(feature = "python-compat-tests")]
//! Rust-owned contract cases for provider hook installation and rollback.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyModule};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use support::{assert_error, module, path, text, AttrPatch, Case};

fn py_json<'py>(py: Python<'py>, value: &Value) -> Bound<'py, PyAny> {
    py.import("json")
        .unwrap()
        .getattr("loads")
        .unwrap()
        .call1((value.to_string(),))
        .unwrap()
}

fn json_value(value: &Bound<'_, PyAny>) -> Value {
    let py = value.py();
    let encoded: String = py
        .import("json")
        .unwrap()
        .getattr("dumps")
        .unwrap()
        .call1((value,))
        .unwrap()
        .extract()
        .unwrap();
    serde_json::from_str(&encoded).unwrap()
}

fn provider<'py>(installer: &Bound<'py, PyModule>, name: &str) -> Bound<'py, PyAny> {
    installer
        .getattr("PROVIDERS")
        .unwrap()
        .get_item(name)
        .unwrap()
}

fn config_path(installer: &Bound<'_, PyModule>, root: &Path, name: &str) -> PathBuf {
    root.join(text(
        &provider(installer, name).getattr("relative_path").unwrap(),
    ))
}

fn write_config(installer: &Bound<'_, PyModule>, root: &Path, name: &str, value: Value) -> PathBuf {
    let dest = config_path(installer, root, name);
    fs::create_dir_all(dest.parent().unwrap()).unwrap();
    fs::write(
        &dest,
        format!("{}\n", serde_json::to_string_pretty(&value).unwrap()),
    )
    .unwrap();
    dest
}

fn managed_commands(py: Python<'_>, installer: &Bound<'_, PyModule>, value: &Value) -> Vec<String> {
    let native = module(py, "conductor._native");
    let managed = text(&installer.getattr("MANAGED_MODULE").unwrap());
    let mut commands = Vec::new();
    if let Some(hooks) = value.get("hooks").and_then(Value::as_object) {
        for groups in hooks.values() {
            for group in groups.as_array().unwrap() {
                if let Some(entries) = group.get("hooks").and_then(Value::as_array) {
                    for entry in entries {
                        if let Some(command) = entry.get("command").and_then(Value::as_str) {
                            let ours: bool = native
                                .getattr("hook_installer_is_managed_native")
                                .unwrap()
                                .call1((command, managed.as_str()))
                                .unwrap()
                                .extract()
                                .unwrap();
                            if ours {
                                commands.push(command.to_owned());
                            }
                        }
                    }
                }
            }
        }
    }
    commands
}

fn command(
    py: Python<'_>,
    installer: &Bound<'_, PyModule>,
    name: &str,
    interpreter: Option<&str>,
    identity: Option<&str>,
) -> PyResult<String> {
    let kwargs = PyDict::new(py);
    if let Some(interpreter) = interpreter {
        kwargs.set_item("interpreter", interpreter)?;
    }
    if let Some(identity) = identity {
        kwargs.set_item("identity", identity)?;
    }
    let value = installer
        .getattr("startup_command")?
        .call((provider(installer, name),), Some(&kwargs))?;
    Ok(text(&value))
}

fn merge(
    py: Python<'_>,
    installer: &Bound<'_, PyModule>,
    name: &str,
    value: Value,
    cmd: &str,
) -> Value {
    json_value(
        &installer
            .getattr("merge_install")
            .unwrap()
            .call1((py_json(py, &value), provider(installer, name), cmd))
            .unwrap(),
    )
}

fn main_call(_py: Python<'_>, installer: &Bound<'_, PyModule>, args: &[&str]) -> i32 {
    installer
        .getattr("main")
        .unwrap()
        .call1((args.to_vec(),))
        .unwrap()
        .extract()
        .unwrap()
}

fn backup_path(py: Python<'_>, installer: &Bound<'_, PyModule>, dest: &Path) -> PathBuf {
    PathBuf::from(text(
        &installer
            .getattr("backup_path")
            .unwrap()
            .call1((path(py, dest),))
            .unwrap(),
    ))
}

fn capture_stdout<'py>(py: Python<'py>) -> (Bound<'py, PyAny>, AttrPatch) {
    let output = py
        .import("io")
        .unwrap()
        .getattr("StringIO")
        .unwrap()
        .call0()
        .unwrap();
    let patch = AttrPatch::replace(&py.import("sys").unwrap(), "stdout", &output);
    (output, patch)
}

#[test]
fn install_merge_is_idempotent_for_every_provider_and_keeps_foreign_settings() {
    let _case = Case::new();
    Python::attach(|py| {
        let installer = module(py, "conductor.hook_installer");
        let names: Vec<String> = installer
            .getattr("PROVIDERS")
            .unwrap()
            .call_method0("keys")
            .unwrap()
            .try_iter()
            .unwrap()
            .map(|entry| entry.unwrap().extract::<String>().unwrap())
            .collect();
        assert_eq!(names.len(), 4);
        for name in names {
            let original = json!({"permissions":{"allow":["Read"]},"hooks":{"PreToolUse":[{
                "matcher":"Read","hooks":[{"type":"command","command":"keep-me","timeout":7}]
            }]}});
            let cmd = command(
                py,
                &installer,
                &name,
                Some("/runtime/python"),
                Some("codex-efficiency"),
            )
            .unwrap();
            let once = merge(py, &installer, &name, original.clone(), &cmd);
            let twice = merge(py, &installer, &name, once.clone(), &cmd);
            assert_eq!(once, twice, "{name}");
            assert_eq!(once["permissions"], original["permissions"], "{name}");
            assert_eq!(
                once["hooks"]["PreToolUse"], original["hooks"]["PreToolUse"],
                "{name}"
            );
            assert_eq!(managed_commands(py, &installer, &once), vec![cmd], "{name}");
        }
    });
}

#[test]
fn startup_command_has_fixed_context_bounds_and_grok_uses_first_turn() {
    let _case = Case::new();
    Python::attach(|py| {
        let installer = module(py, "conductor.hook_installer");
        let cmd = command(py, &installer, "codex", Some("/runtime/python"), None).unwrap();
        let parts: Vec<String> = py
            .import("shlex")
            .unwrap()
            .getattr("split")
            .unwrap()
            .call1((cmd.as_str(),))
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(
            &parts[..3],
            ["/runtime/python", "-m", "conductor.a2a_session_start"]
        );
        for (flag, expected) in [
            ("--max-messages", "8"),
            ("--preview-chars", "140"),
            ("--max-chars", "1200"),
        ] {
            let index = parts.iter().position(|part| part == flag).unwrap();
            assert_eq!(parts[index + 1], expected);
        }
        assert!(!cmd.contains("/home/"));
        let grok = command(py, &installer, "grok", Some("python"), None).unwrap();
        let original = json!({"hooks":{"SessionStart":[{"hooks":[{"command":"keep"}]}]}});
        let updated = merge(py, &installer, "grok", original.clone(), &grok);
        assert_eq!(
            updated["hooks"]["SessionStart"],
            original["hooks"]["SessionStart"]
        );
        assert_eq!(
            updated["hooks"]["UserPromptSubmit"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert!(grok.contains("--once-per-session"));
    });
}

#[test]
fn default_install_is_dry_run_and_invalid_json_is_not_written() {
    let case = Case::new();
    Python::attach(|py| {
        let installer = module(py, "conductor.hook_installer");
        let dest = write_config(
            &installer,
            case.root(),
            "codex",
            json!({"unrelated":{"answer":42}}),
        );
        let original = fs::read(&dest).unwrap();
        let (output, _patch) = capture_stdout(py);
        let root = case.root().to_str().unwrap();
        assert_eq!(
            main_call(
                py,
                &installer,
                &[
                    "install",
                    "--provider",
                    "codex",
                    "--root",
                    root,
                    "--interpreter",
                    "/runtime/python"
                ]
            ),
            0
        );
        let summary: Value =
            serde_json::from_str(&text(&output.call_method0("getvalue").unwrap())).unwrap();
        assert_eq!(summary["applied"], false);
        assert_eq!(summary["providers"][0]["changed"], true);
        assert_eq!(fs::read(&dest).unwrap(), original);
        assert!(!backup_path(py, &installer, &dest).exists());
        fs::write(&dest, "{not json\n").unwrap();
        assert_eq!(
            main_call(
                py,
                &installer,
                &["install", "--provider", "codex", "--root", root, "--apply"]
            ),
            2
        );
        assert_eq!(fs::read_to_string(&dest).unwrap(), "{not json\n");
        assert!(!backup_path(py, &installer, &dest).exists());
    });
}

#[test]
fn apply_is_idempotent_and_uninstall_keeps_foreign_hooks() {
    let case = Case::new();
    Python::attach(|py| {
        let installer = module(py, "conductor.hook_installer");
        let dest = write_config(
            &installer,
            case.root(),
            "claude",
            json!({"theme":"dark","hooks":{
                "SessionStart":[{"matcher":"","hooks":[{"type":"command","command":"keep-me","timeout":3}]}]
            }}),
        );
        let root = case.root().to_str().unwrap();
        let args = [
            "install",
            "--provider",
            "claude",
            "--root",
            root,
            "--interpreter",
            "/runtime/python",
            "--identity",
            "codex-efficiency",
            "--apply",
        ];
        let (_output, _patch) = capture_stdout(py);
        assert_eq!(main_call(py, &installer, &args), 0);
        let installed = fs::read_to_string(&dest).unwrap();
        assert_eq!(main_call(py, &installer, &args), 0);
        assert_eq!(fs::read_to_string(&dest).unwrap(), installed);
        let payload: Value = serde_json::from_str(&installed).unwrap();
        assert_eq!(payload["theme"], "dark");
        assert!(!managed_commands(py, &installer, &payload).is_empty());
        assert!(installed.contains("keep-me"));
        assert_eq!(
            main_call(
                py,
                &installer,
                &[
                    "uninstall",
                    "--provider",
                    "claude",
                    "--root",
                    root,
                    "--apply"
                ]
            ),
            0
        );
        let uninstalled = fs::read_to_string(&dest).unwrap();
        let payload: Value = serde_json::from_str(&uninstalled).unwrap();
        assert_eq!(payload["theme"], "dark");
        assert!(managed_commands(py, &installer, &payload).is_empty());
        assert!(uninstalled.contains("keep-me"));
    });
}

#[test]
fn rollback_restores_exact_bytes_and_second_rollback_redoes_install() {
    let case = Case::new();
    Python::attach(|py| {
        let installer = module(py, "conductor.hook_installer");
        let dest = write_config(
            &installer,
            case.root(),
            "qwen",
            json!({"mcpServers":{"existing":{"command":"server"}},"hooks":{}}),
        );
        let original = fs::read(&dest).unwrap();
        let root = case.root().to_str().unwrap();
        let (_output, _patch) = capture_stdout(py);
        assert_eq!(
            main_call(
                py,
                &installer,
                &["install", "--provider", "qwen", "--root", root, "--apply"]
            ),
            0
        );
        assert_ne!(fs::read(&dest).unwrap(), original);
        assert!(backup_path(py, &installer, &dest).is_file());
        assert_eq!(
            main_call(
                py,
                &installer,
                &["rollback", "--provider", "qwen", "--root", root, "--apply"]
            ),
            0
        );
        assert_eq!(fs::read(&dest).unwrap(), original);
        assert_eq!(
            main_call(
                py,
                &installer,
                &["rollback", "--provider", "qwen", "--root", root, "--apply"]
            ),
            0
        );
        let restored: Value = serde_json::from_slice(&fs::read(&dest).unwrap()).unwrap();
        assert!(!managed_commands(py, &installer, &restored).is_empty());
    });
}

#[test]
fn explicit_backup_restores_missing_file_state() {
    let case = Case::new();
    Python::attach(|py| {
        let installer = module(py, "conductor.hook_installer");
        let dest = config_path(&installer, case.root(), "grok");
        let root = case.root().to_str().unwrap();
        let (_output, _patch) = capture_stdout(py);
        assert_eq!(
            main_call(
                py,
                &installer,
                &["backup", "--provider", "grok", "--root", root, "--apply"]
            ),
            0
        );
        assert!(backup_path(py, &installer, &dest).is_file());
        fs::create_dir_all(dest.parent().unwrap()).unwrap();
        fs::write(&dest, "{\"temporary\": true}\n").unwrap();
        assert_eq!(
            main_call(
                py,
                &installer,
                &["rollback", "--provider", "grok", "--root", root, "--apply"]
            ),
            0
        );
        assert!(!dest.exists());
    });
}

#[test]
fn adversarial_identity_round_trips_and_blank_identity_is_rejected() {
    let _case = Case::new();
    Python::attach(|py| {
        let installer = module(py, "conductor.hook_installer");
        let shlex = py.import("shlex").unwrap();
        for identity in ["it's", "$HOME a\"b", "x y", "a\\b"] {
            let cmd = command(py, &installer, "codex", None, Some(identity)).unwrap();
            let parts: Vec<String> = shlex
                .getattr("split")
                .unwrap()
                .call1((cmd,))
                .unwrap()
                .extract()
                .unwrap();
            assert_eq!(parts.last().unwrap(), identity);
        }
        let error = command(py, &installer, "codex", None, Some("  ")).unwrap_err();
        assert_error(
            py,
            error,
            &installer.getattr("HookInstallerError").unwrap(),
            "blank",
        );
    });
}

#[test]
fn merge_rejects_non_object_root_and_uninstall_keeps_foreign_groups() {
    let _case = Case::new();
    Python::attach(|py| {
        let installer = module(py, "conductor.hook_installer");
        let error = installer
            .getattr("merge_install")
            .unwrap()
            .call1((
                py_json(py, &json!(["not", "a", "dict"])),
                provider(&installer, "codex"),
                "x",
            ))
            .unwrap_err();
        assert_error(
            py,
            error,
            &installer.getattr("HookInstallerError").unwrap(),
            "JSON object",
        );
        let python = text(&py.import("sys").unwrap().getattr("executable").unwrap());
        let config = json!({"hooks":{
            "SessionStart":[{"hooks":[{"type":"command",
                "command":format!("{python} -m conductor.a2a_session_start --provider codex") }]}],
            "UserPromptSubmit":[{"matcher":".*","hooks":[{"type":"command","command":"echo foreign"}]}]
        },"model":"x"});
        let merged = json_value(
            &installer
                .getattr("merge_uninstall")
                .unwrap()
                .call1((py_json(py, &config),))
                .unwrap(),
        );
        assert_eq!(merged["model"], "x");
        assert_eq!(
            merged["hooks"]["UserPromptSubmit"],
            config["hooks"]["UserPromptSubmit"]
        );
        assert!(merged["hooks"].get("SessionStart").is_none());
    });
}
