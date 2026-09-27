#![cfg(feature = "python-compat-tests")]
//! Forge executable selection and hook command contracts for `conductor init`.

#[path = "python_contracts/agent_comm_support.rs"]
#[allow(dead_code)]
mod comm_support;
#[path = "python_contracts/project_init_support.rs"]
#[allow(dead_code)]
mod init_support;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use init_support::{
    config, event_command, events, executable, init, json_loads, plan, repo, settings_action,
    settings_hooks, which,
};
use pyo3::prelude::*;
use pyo3::types::{PyList, PyString};
use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;
use support::{assert_error, module, path, text, AttrPatch, Case};

fn synthetic_python(case: &Case) -> std::path::PathBuf {
    let python = case.root().join("venv/bin/python");
    fs::create_dir_all(python.parent().unwrap()).unwrap();
    python
}

fn patch_python(py: Python<'_>, python: &Path) -> AttrPatch {
    let sys = module(py, "sys");
    AttrPatch::replace(
        sys.as_any(),
        "executable",
        PyString::new(py, python.to_str().unwrap()).as_any(),
    )
}

fn resolve(py: Python<'_>, project: &Path) -> Option<String> {
    let selected = init(py)
        .getattr("resolve_forge_binary")
        .unwrap()
        .call1((path(py, project),))
        .unwrap();
    if selected.is_none() {
        None
    } else {
        Some(text(&selected))
    }
}

#[test]
fn resolve_forge_binary_prefers_project_local_tools_bin() {
    let mut case = Case::new();
    case.remove_env("FORGE_BIN");
    let project = repo(&case);
    let local = executable(&project, ".tools/bin/forge");
    let python = synthetic_python(&case);
    Python::attach(|py| {
        let _python = patch_python(py, &python);
        let _which = which(py, Some("/usr/bin/forge"));
        assert_eq!(resolve(py, &project).as_deref(), local.to_str());
    });
}

#[test]
fn resolve_forge_binary_falls_back_to_path() {
    let mut case = Case::new();
    case.remove_env("FORGE_BIN");
    let project = repo(&case);
    let python = synthetic_python(&case);
    Python::attach(|py| {
        let _python = patch_python(py, &python);
        let _which = which(py, Some("/usr/local/bin/forge"));
        assert_eq!(
            resolve(py, &project).as_deref(),
            Some("/usr/local/bin/forge")
        );
    });
}

#[test]
fn resolve_forge_binary_is_none_when_absent_everywhere() {
    let mut case = Case::new();
    case.remove_env("FORGE_BIN");
    let project = repo(&case);
    let python = synthetic_python(&case);
    Python::attach(|py| {
        let _python = patch_python(py, &python);
        let _which = which(py, None);
        assert_eq!(resolve(py, &project), None);
    });
}

#[test]
fn resolve_forge_binary_ignores_a_non_executable_local_file() {
    let mut case = Case::new();
    case.remove_env("FORGE_BIN");
    let project = repo(&case);
    let local = project.join(".tools/bin/forge");
    fs::create_dir_all(local.parent().unwrap()).unwrap();
    fs::write(local, "not executable").unwrap();
    let python = synthetic_python(&case);
    Python::attach(|py| {
        let _python = patch_python(py, &python);
        let _which = which(py, None);
        assert_eq!(resolve(py, &project), None);
    });
}

#[test]
fn render_settings_without_a_forge_binary_keeps_the_python_launcher() {
    let _case = Case::new();
    Python::attach(|py| {
        let rendered = init(py)
            .getattr("render_settings")
            .unwrap()
            .call1((py.None(), false))
            .unwrap();
        let hooks = json_loads(py, &rendered).get_item("hooks").unwrap();
        let template = settings_hooks(py);
        for event in events(py) {
            let command = event_command(&hooks, &event);
            assert!(command.eq(event_command(&template, &event)).unwrap());
            assert!(!text(&command).contains("forge hook"));
        }
    });
}

#[test]
fn render_settings_with_a_forge_binary_switches_every_event_command() {
    let _case = Case::new();
    Python::attach(|py| {
        let forge = Path::new("/opt/forge/bin/forge");
        let rendered = init(py)
            .getattr("render_settings")
            .unwrap()
            .call1((py.None(), false, path(py, forge)))
            .unwrap();
        let hooks = json_loads(py, &rendered).get_item("hooks").unwrap();
        let template = settings_hooks(py);
        for event in events(py) {
            assert_eq!(
                text(&event_command(&hooks, &event)),
                format!("{} hook {event}", forge.display())
            );
            let actual = hooks.get_item(&event).unwrap().get_item(0).unwrap();
            let expected = template.get_item(&event).unwrap().get_item(0).unwrap();
            assert!(actual
                .get_item("matcher")
                .unwrap()
                .eq(expected.get_item("matcher").unwrap())
                .unwrap());
            assert!(actual
                .get_item("hooks")
                .unwrap()
                .get_item(0)
                .unwrap()
                .get_item("timeout")
                .unwrap()
                .eq(expected
                    .get_item("hooks")
                    .unwrap()
                    .get_item(0)
                    .unwrap()
                    .get_item("timeout")
                    .unwrap())
                .unwrap());
        }
    });
}

#[test]
fn plan_wires_settings_to_a_detected_project_local_forge_binary() {
    let mut case = Case::new();
    case.remove_env("FORGE_BIN");
    let project = repo(&case);
    let local = executable(&project, ".tools/bin/forge");
    let python = synthetic_python(&case);
    Python::attach(|py| {
        let _which = which(py, None);
        let _doctor = init_support::doctor(py, 0, true);
        let config = config(py, &project, Some(&python), false, false, false);
        let action = settings_action(py, &plan(py, &config, true));
        let hooks = json_loads(py, &action.getattr("after").unwrap())
            .get_item("hooks")
            .unwrap();
        for event in events(py) {
            let argv = PyList::new(
                py,
                [
                    "env".to_owned(),
                    format!("CONDUCTOR_PYTHON={}", python.display()),
                    local.display().to_string(),
                    "hook".to_owned(),
                    event.clone(),
                ],
            )
            .unwrap();
            let expected = module(py, "shlex")
                .getattr("join")
                .unwrap()
                .call1((argv,))
                .unwrap();
            assert!(event_command(&hooks, &event).eq(expected).unwrap());
        }
    });
}

#[test]
fn plan_keeps_the_python_launcher_when_no_forge_binary_is_found() {
    let mut case = Case::new();
    case.remove_env("FORGE_BIN");
    let project = repo(&case);
    let python = synthetic_python(&case);
    Python::attach(|py| {
        let _which = which(py, None);
        let _doctor = init_support::doctor(py, 0, true);
        let config = config(py, &project, Some(&python), false, false, false);
        let action = settings_action(py, &plan(py, &config, true));
        let hooks = json_loads(py, &action.getattr("after").unwrap())
            .get_item("hooks")
            .unwrap();
        assert!(hooks.eq(settings_hooks(py)).unwrap());
    });
}

#[test]
fn installed_venv_forge_precedes_project_build_and_stale_path() {
    let mut case = Case::new();
    case.remove_env("FORGE_BIN");
    let project = repo(&case);
    let python = synthetic_python(&case);
    let installed = executable(case.root(), "venv/bin/forge");
    let _local = executable(&project, ".tools/bin/forge");
    Python::attach(|py| {
        let _python = patch_python(py, &python);
        let _which = which(py, Some("/stale/global/forge"));
        assert_eq!(resolve(py, &project).as_deref(), installed.to_str());
    });
}

#[test]
fn interpreter_symlink_keeps_venv_sibling_candidate() {
    let mut case = Case::new();
    case.remove_env("FORGE_BIN");
    let project = repo(&case);
    let python = synthetic_python(&case);
    symlink("/usr/bin/python3", &python).unwrap();
    let installed = executable(case.root(), "venv/bin/forge");
    Python::attach(|py| {
        let _python = patch_python(py, &python);
        let _which = which(py, None);
        assert_eq!(resolve(py, &project).as_deref(), installed.to_str());
    });
}

#[test]
fn explicit_override_precedes_venv_and_invalid_override_refuses() {
    let mut case = Case::new();
    let project = repo(&case);
    let python = synthetic_python(&case);
    let _installed = executable(case.root(), "venv/bin/forge");
    let override_bin = executable(case.root(), "override/forge");
    case.set_env("FORGE_BIN", override_bin.to_str().unwrap());
    Python::attach(|py| {
        let _python = patch_python(py, &python);
        let _which = which(py, Some("/stale/global/forge"));
        assert_eq!(resolve(py, &project).as_deref(), override_bin.to_str());
    });
    case.set_env("FORGE_BIN", "/absent/forge");
    Python::attach(|py| {
        let _python = patch_python(py, &python);
        let _which = which(py, None);
        let error = init(py)
            .getattr("resolve_forge_binary")
            .unwrap()
            .call1((path(py, &project),))
            .unwrap_err();
        assert_error(
            py,
            error,
            &py.get_type::<pyo3::exceptions::PyRuntimeError>().into_any(),
            "FORGE_BIN does not resolve to an executable",
        );
    });
}

#[test]
fn non_executable_venv_sibling_uses_legacy_fallback() {
    let mut case = Case::new();
    case.remove_env("FORGE_BIN");
    let project = repo(&case);
    let python = synthetic_python(&case);
    let sibling = case.root().join("venv/bin/forge");
    fs::write(&sibling, "not executable").unwrap();
    let local = executable(&project, ".tools/bin/forge");
    Python::attach(|py| {
        let _python = patch_python(py, &python);
        let _which = which(py, Some("/stale/global/forge"));
        assert_eq!(resolve(py, &project).as_deref(), local.to_str());
    });
}
