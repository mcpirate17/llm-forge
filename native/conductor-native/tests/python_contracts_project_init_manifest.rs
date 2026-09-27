#![cfg(feature = "python-compat-tests")]
//! Host pyproject warning contracts for `conductor init`.

#[path = "python_contracts/agent_comm_support.rs"]
#[allow(dead_code)]
mod comm_support;
#[path = "python_contracts/project_init_support.rs"]
#[allow(dead_code)]
mod init_support;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use init_support::{config, doctor, init, plan, repo};
use pyo3::prelude::*;
use pyo3::types::PySet;
use std::fs;
use support::{module, path, text, Case};

fn warnings(py: Python<'_>, project: &std::path::Path) -> Vec<String> {
    init(py)
        .getattr("_pyproject_conductor_warnings")
        .unwrap()
        .call1((path(py, project),))
        .unwrap()
        .extract()
        .unwrap()
}

#[test]
fn pyproject_warning_when_manifest_absent() {
    let case = Case::new();
    let project = repo(&case);
    Python::attach(|py| {
        let messages = warnings(py, &project);
        assert_eq!(messages.len(), 1);
        assert!(messages[0].contains("pyproject.toml does not exist"));
    });
}

#[test]
fn pyproject_warning_lists_missing_keys() {
    let case = Case::new();
    let project = repo(&case);
    fs::write(
        project.join("pyproject.toml"),
        "[tool.conductor]\ncandidate_policy = \"x.toml\"\n",
    )
    .unwrap();
    Python::attach(|py| {
        let messages = warnings(py, &project);
        assert_eq!(messages.len(), 1);
        assert!(messages[0].contains("mutation_registry"));
        assert!(messages[0].contains("package_root"));
        assert!(!messages[0]
            .split_once("missing")
            .unwrap()
            .1
            .contains("candidate_policy"));
    });
}

#[test]
fn pyproject_no_warning_when_stanza_is_complete() {
    let case = Case::new();
    let project = repo(&case);
    fs::write(project.join("pyproject.toml"), "[tool.conductor]\ncandidate_policy = \"candidate_policy.toml\"\nmutation_registry = \"campaigns/registry.json\"\npackage_root = \"src/conductor\"\n").unwrap();
    Python::attach(|py| assert!(warnings(py, &project).is_empty()));
}

#[test]
fn pyproject_warning_on_malformed_toml() {
    let case = Case::new();
    let project = repo(&case);
    fs::write(project.join("pyproject.toml"), "[tool.conductor\n").unwrap();
    Python::attach(|py| {
        let messages = warnings(py, &project);
        assert_eq!(messages.len(), 1);
        assert!(messages[0].contains("could not be parsed"));
    });
}

#[test]
fn pyproject_warning_reaches_the_plan_without_writing_pyproject() {
    let case = Case::new();
    let project = repo(&case);
    Python::attach(|py| {
        let _doctor = doctor(py, 0, true);
        let p = plan(py, &config(py, &project, None, false, false, false), false);
        let messages = p.getattr("warnings").unwrap();
        assert!(messages
            .try_iter()
            .unwrap()
            .any(|message| text(&message.unwrap()).contains("pyproject.toml does not exist")));
        assert!(!project.join("pyproject.toml").exists());
    });
}

#[test]
fn conductor_stanza_keys_round_trip_tomllib() {
    let _case = Case::new();
    Python::attach(|py| {
        let parsed = module(py, "tomllib")
            .getattr("loads")
            .unwrap()
            .call1(("[tool.conductor]\ncandidate_policy = \"a\"\nmutation_registry = \"b\"\npackage_root = \"c\"\n",))
            .unwrap();
        let keys = init(py).getattr("CONDUCTOR_STANZA_KEYS").unwrap();
        let expected = PySet::new(py, keys.try_iter().unwrap().map(|v| v.unwrap())).unwrap();
        let actual = parsed
            .get_item("tool")
            .unwrap()
            .get_item("conductor")
            .unwrap()
            .call_method0("keys")
            .unwrap();
        let difference = expected.call_method1("difference", (actual,)).unwrap();
        assert_eq!(difference.len().unwrap(), 0);
    });
}
