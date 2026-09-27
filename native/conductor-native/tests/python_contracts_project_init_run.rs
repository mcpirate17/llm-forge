#![cfg(feature = "python-compat-tests")]
//! Scaffolding lifecycle and executable hook contracts for `conductor init`.

#[path = "python_contracts/agent_comm_support.rs"]
#[allow(dead_code)]
mod comm_support;
#[path = "python_contracts/project_init_support.rs"]
#[allow(dead_code)]
mod init_support;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use comm_support::{buffer_text, capture, py_json};
use init_support::{config, doctor, init, json_loads, plan, python_executable, repo};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList, PySet};
use serde_json::json;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use support::{assert_error, module, path, text, Case};

fn run(py: Python<'_>, config: &Bound<'_, pyo3::types::PyAny>) -> i64 {
    init(py)
        .getattr("run")
        .unwrap()
        .call1((config,))
        .unwrap()
        .extract()
        .unwrap()
}

fn project_file(root: &Path, relative: &str) -> PathBuf {
    root.join(relative)
}

#[test]
fn apply_then_second_run_is_idempotent() {
    let mut case = Case::new();
    case.remove_env("FORGE_BIN");
    let project = repo(&case);
    fs::write(project.join(".gitignore"), "node_modules/\n").unwrap();
    Python::attach(|py| {
        let _doctor = doctor(py, 0, true);
        let pi = init(py);
        let cfg = config(py, &project, None, false, false, false);
        assert_eq!(run(py, &cfg), 0);
        let settings = json_loads(
            py,
            &pyo3::types::PyString::new(
                py,
                &fs::read_to_string(project.join(".claude/settings.json")).unwrap(),
            )
            .into_any(),
        );
        let chosen = pi
            .getattr("resolve_forge_binary")
            .unwrap()
            .call1((path(py, &project),))
            .unwrap();
        let expected = pi
            .getattr("_hooks_block")
            .unwrap()
            .call1((chosen, path(py, &python_executable(py))))
            .unwrap();
        assert!(settings.get_item("hooks").unwrap().eq(expected).unwrap());
        let launcher = project.join(text(&pi.getattr("LAUNCHER").unwrap()));
        assert_ne!(
            fs::metadata(&launcher).unwrap().permissions().mode() & 0o100,
            0
        );
        assert!(fs::read_to_string(&launcher)
            .unwrap()
            .starts_with(&format!("#!{}\n", python_executable(py).display())));
        assert!(project_file(&project, &text(&pi.getattr("REGISTRY").unwrap())).is_file());
        for keep in pi.getattr("GITKEEPS").unwrap().try_iter().unwrap() {
            assert!(project_file(&project, &text(&keep.unwrap())).is_file());
        }
        let second = plan(py, &cfg, false);
        assert_eq!(second.getattr("changed").unwrap().len().unwrap(), 0);
        let statuses = second
            .getattr("actions")
            .unwrap()
            .try_iter()
            .unwrap()
            .map(|entry| text(&entry.unwrap().getattr("status").unwrap()))
            .collect::<Vec<_>>();
        assert!(PySet::new(py, statuses)
            .unwrap()
            .eq(PySet::new(py, ["unchanged"]).unwrap())
            .unwrap());
        assert_eq!(run(py, &config(py, &project, None, false, false, true)), 0);
        let marker = text(&pi.getattr("MARK_BEGIN").unwrap());
        assert_eq!(
            fs::read_to_string(project.join(".gitignore"))
                .unwrap()
                .matches(&marker)
                .count(),
            1
        );
    });
}

#[test]
fn project_owned_files_are_never_rewritten() {
    let case = Case::new();
    let project = repo(&case);
    Python::attach(|py| {
        let _doctor = doctor(py, 0, true);
        let pi = init(py);
        assert_eq!(run(py, &config(py, &project, None, false, false, false)), 0);
        let owned = ["POLICY", "PREAUTH", "REGISTRY"]
            .map(|name| project.join(text(&pi.getattr(name).unwrap())));
        for file in &owned {
            fs::write(file, "owner edit\n").unwrap();
        }
        assert_eq!(run(py, &config(py, &project, None, true, false, false)), 0);
        for file in &owned {
            assert_eq!(fs::read_to_string(file).unwrap(), "owner edit\n");
        }
    });
}

#[test]
fn dry_run_writes_nothing_and_prints_diff() {
    let case = Case::new();
    let project = repo(&case);
    Python::attach(|py| {
        let _doctor = doctor(py, 0, true);
        let (output, _capture) = capture(py, "stdout");
        assert_eq!(run(py, &config(py, &project, None, false, true, false)), 0);
        assert!(!project.join(".claude").exists());
        assert!(!project.join("conductor").exists());
        let out = buffer_text(&output);
        assert!(out.contains("+++ b/.claude/settings.json"));
        assert!(out.contains("create    .claude/hooks/dispatch.py"));
    });
}

#[test]
fn check_reports_drift() {
    let case = Case::new();
    let project = repo(&case);
    Python::attach(|py| {
        let _doctor = doctor(py, 0, true);
        assert_eq!(run(py, &config(py, &project, None, false, false, true)), 1);
        assert!(!project.join(".claude/settings.json").exists());
        assert_eq!(run(py, &config(py, &project, None, false, false, false)), 0);
        assert_eq!(run(py, &config(py, &project, None, false, false, true)), 0);
        fs::write(project.join(".claude/settings.json"), "{\"hooks\": {}}").unwrap();
        assert_eq!(run(py, &config(py, &project, None, false, false, true)), 1);
    });
}

#[test]
fn conflict_refuses_before_any_write() {
    let case = Case::new();
    let project = repo(&case);
    let settings = project.join(".claude/settings.json");
    fs::create_dir_all(settings.parent().unwrap()).unwrap();
    fs::write(
        &settings,
        r#"{"hooks":{"SessionStart":[{"hooks":[{"command":"z"}]}]}}"#,
    )
    .unwrap();
    Python::attach(|py| {
        let _doctor = doctor(py, 0, true);
        let pi = init(py);
        let error = pi
            .getattr("run")
            .unwrap()
            .call1((config(py, &project, None, false, false, false),))
            .unwrap_err();
        assert_error(py, error, &pi.getattr("InitError").unwrap(), "SessionStart");
        assert!(!project
            .join(text(&pi.getattr("LAUNCHER").unwrap()))
            .exists());
        assert_eq!(run(py, &config(py, &project, None, true, false, false)), 0);
    });
}

#[test]
fn dead_hook_fails_loud() {
    let case = Case::new();
    let project = repo(&case);
    Python::attach(|py| {
        let _doctor = doctor(py, 1, true);
        let pi = init(py);
        let error = pi
            .getattr("run")
            .unwrap()
            .call1((config(py, &project, None, false, false, false),))
            .unwrap_err();
        assert_error(py, error, &pi.getattr("InitError").unwrap(), "dead hook");
        assert!(project
            .join(text(&pi.getattr("SETTINGS").unwrap()))
            .is_file());
        assert_eq!(run(py, &config(py, &project, None, false, false, true)), 1);
    });
}

#[test]
fn not_a_git_repo_refused() {
    let case = Case::new();
    Python::attach(|py| {
        let pi = init(py);
        let error = pi
            .getattr("run")
            .unwrap()
            .call1((config(py, case.root(), None, false, false, false),))
            .unwrap_err();
        assert_error(
            py,
            error,
            &pi.getattr("InitError").unwrap(),
            "git repository",
        );
    });
}

#[test]
fn missing_crg_is_a_warning_not_a_refusal() {
    let case = Case::new();
    let project = repo(&case);
    Python::attach(|py| {
        let _doctor = doctor(py, 0, false);
        let (stderr, _capture) = capture(py, "stderr");
        assert_eq!(run(py, &config(py, &project, None, false, false, false)), 0);
        assert!(buffer_text(&stderr).contains("code_review_graph is not importable"));
    });
}

#[test]
fn cli_parses_and_refuses_exclusive_flags() {
    let case = Case::new();
    Python::attach(|py| {
        let pi = init(py);
        let root = case.root().to_str().unwrap();
        let parsed = pi
            .getattr("parse_args")
            .unwrap()
            .call1((PyList::new(py, [root, "--force", "--dry-run"]).unwrap(),))
            .unwrap();
        let expected = path(py, case.root()).call_method0("resolve").unwrap();
        assert!(parsed.getattr("project_dir").unwrap().eq(expected).unwrap());
        assert!(parsed.getattr("force").unwrap().extract::<bool>().unwrap());
        assert!(parsed
            .getattr("dry_run")
            .unwrap()
            .extract::<bool>()
            .unwrap());
        assert!(!parsed.getattr("check").unwrap().extract::<bool>().unwrap());
        let venv = case.root().join(".venv/bin/python");
        fs::create_dir_all(venv.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(python_executable(py), &venv).unwrap();
        let args = PyList::new(py, [root, "--python", venv.to_str().unwrap()]).unwrap();
        let parsed = pi.getattr("parse_args").unwrap().call1((args,)).unwrap();
        assert!(parsed
            .getattr("python")
            .unwrap()
            .eq(path(py, &venv))
            .unwrap());
        let args = PyList::new(py, [root, "--dry-run", "--check"]).unwrap();
        let error = pi
            .getattr("parse_args")
            .unwrap()
            .call1((args,))
            .unwrap_err();
        assert!(error
            .matches(py, py.get_type::<pyo3::exceptions::PySystemExit>())
            .unwrap());
    });
}

#[test]
fn main_reports_refusal_as_exit_2() {
    let case = Case::new();
    Python::attach(|py| {
        let main = module(py, "conductor.__main__").getattr("main").unwrap();
        let (stderr, _capture) = capture(py, "stderr");
        let args = PyList::new(py, ["init", case.root().to_str().unwrap(), "--dry-run"]).unwrap();
        assert!(main.call1((args,)).unwrap().eq(2).unwrap());
        assert!(buffer_text(&stderr).contains("REFUSED"));
        assert!(main
            .call1((PyList::new(py, ["bogus"]).unwrap(),))
            .unwrap()
            .eq(2)
            .unwrap());
    });
}

#[test]
fn init_runs_doctor_and_dispatcher_denies_force_push() {
    let case = Case::new();
    let project = repo(&case);
    Python::attach(|py| {
        let subprocess = module(py, "subprocess");
        let kwargs = PyDict::new(py);
        kwargs.set_item("check", true).unwrap();
        kwargs.set_item("timeout", 30).unwrap();
        subprocess
            .getattr("run")
            .unwrap()
            .call(
                (PyList::new(py, ["git", "init", "-q", project.to_str().unwrap()]).unwrap(),),
                Some(&kwargs),
            )
            .unwrap();
        let pi = init(py);
        assert_eq!(run(py, &config(py, &project, None, false, false, false)), 0);
        let env = module(py, "os")
            .getattr("environ")
            .unwrap()
            .call_method0("copy")
            .unwrap();
        env.call_method1("pop", ("PYTHONPATH", py.None())).unwrap();
        env.set_item("PYTHONPATH", pi.getattr("TOOLING_ROOT").unwrap())
            .unwrap();
        env.set_item("CLAUDE_PROJECT_DIR", path(py, &project))
            .unwrap();
        env.set_item("CRG_SKIP_EMBED", "1").unwrap();
        let payload = py_json(
            py,
            json!({"session_id":"init-test","cwd":project.to_str().unwrap(),
            "hook_event_name":"PreToolUse","tool_name":"Bash",
            "tool_input":{"command":"git push --force origin master"}}),
        );
        let args = PyList::new(
            py,
            [
                project
                    .join(text(&pi.getattr("LAUNCHER").unwrap()))
                    .to_str()
                    .unwrap(),
                "PreToolUse",
            ],
        )
        .unwrap();
        let kwargs = PyDict::new(py);
        kwargs
            .set_item("input", init_support::json_dumps(&payload))
            .unwrap();
        kwargs.set_item("capture_output", true).unwrap();
        kwargs.set_item("text", true).unwrap();
        kwargs.set_item("cwd", path(py, &project)).unwrap();
        kwargs.set_item("env", env).unwrap();
        kwargs.set_item("timeout", 60).unwrap();
        kwargs.set_item("check", false).unwrap();
        let completed = subprocess
            .getattr("run")
            .unwrap()
            .call((args,), Some(&kwargs))
            .unwrap();
        let status: i64 = completed.getattr("returncode").unwrap().extract().unwrap();
        assert_eq!(status, 0, "{}", text(&completed.getattr("stderr").unwrap()));
        let output = json_loads(py, &completed.getattr("stdout").unwrap());
        assert!(output
            .get_item("hookSpecificOutput")
            .unwrap()
            .get_item("permissionDecision")
            .unwrap()
            .eq("deny")
            .unwrap());
    });
}

#[test]
fn workflow_and_env_stub_are_written_once() {
    let case = Case::new();
    let project = repo(&case);
    Python::attach(|py| {
        let _doctor = doctor(py, 0, true);
        let pi = init(py);
        assert_eq!(run(py, &config(py, &project, None, false, false, false)), 0);
        let workflow = project.join(text(&pi.getattr("WORKFLOW").unwrap()));
        let env_stub = project.join(text(&pi.getattr("ENV_STUB").unwrap()));
        assert_eq!(
            fs::read_to_string(&workflow).unwrap(),
            text(&pi.getattr("WORKFLOW_TEXT").unwrap())
        );
        assert_eq!(
            fs::read_to_string(&env_stub).unwrap(),
            text(&pi.getattr("ENV_STUB_TEXT").unwrap())
        );
        assert!(fs::read_to_string(&workflow)
            .unwrap()
            .contains("conductor.guardrail_audit"));
        assert!(fs::read_to_string(&workflow)
            .unwrap()
            .contains("conductor.radon_complexity"));
        assert!(fs::read_to_string(&env_stub)
            .unwrap()
            .contains("BASH_QUIET_SAVE_DIR"));
        assert_eq!(
            plan(py, &config(py, &project, None, false, false, false), false)
                .getattr("changed")
                .unwrap()
                .len()
                .unwrap(),
            0
        );
        for file in [&workflow, &env_stub] {
            fs::write(file, "owner edit\n").unwrap();
        }
        assert_eq!(run(py, &config(py, &project, None, true, false, false)), 0);
        for file in [&workflow, &env_stub] {
            assert_eq!(fs::read_to_string(file).unwrap(), "owner edit\n");
        }
    });
}
