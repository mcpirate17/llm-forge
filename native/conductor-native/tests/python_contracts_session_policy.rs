#![cfg(feature = "python-compat-tests")]
//! Host session policy contracts; Forge's own policy stays neutral.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::prelude::*;
use pyo3::types::{PyAny, PyModule, PyTuple};
use std::fs;
use std::path::Path;
use support::{assert_error, module, path, Case};

fn load<'py>(
    py: Python<'py>,
    policy: &Bound<'py, PyModule>,
    root: &Path,
) -> PyResult<Bound<'py, PyAny>> {
    policy.call_method1("load_session_policy", (path(py, root),))
}

#[test]
fn a_host_projects_session_policy_round_trips_exactly() {
    let case = Case::new();
    let preamble = [
        "MISSION: exercise the round trip end to end.",
        "SECOND: a second opted-in preamble line.",
    ];
    let mandates = [
        "FIRST_RULE: a standing mandate a host opted into.",
        "SECOND_RULE: a second standing mandate.",
    ];
    case.write(
        "pyproject.toml",
        &format!(
            "[tool.conductor.session]\npreamble = {}\nstanding_mandates = {}\n",
            serde_json::to_string(&preamble).unwrap(),
            serde_json::to_string(&mandates).unwrap()
        ),
    );
    Python::attach(|py| {
        let module = module(py, "conductor.session_policy");
        let policy = load(py, &module, case.root()).unwrap();
        assert!(policy
            .getattr("preamble")
            .unwrap()
            .eq(PyTuple::new(py, preamble).unwrap())
            .unwrap());
        assert!(policy
            .getattr("standing_mandates")
            .unwrap()
            .eq(PyTuple::new(py, mandates).unwrap())
            .unwrap());
        let error = policy.setattr("preamble", PyTuple::empty(py)).unwrap_err();
        let frozen = py
            .import("dataclasses")
            .unwrap()
            .getattr("FrozenInstanceError")
            .unwrap();
        assert!(error.matches(py, &frozen).unwrap());
    });
}

#[test]
fn this_packages_own_root_has_no_opinion_by_default() {
    let _case = Case::new();
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    Python::attach(|py| {
        let policy = module(py, "conductor.session_policy");
        let empty = policy.getattr("EMPTY_SESSION_POLICY").unwrap();
        // Preserve the original module-derived src root and cover the actual
        // repository root promised by the original test's description.
        for root in [repo.join("src"), repo] {
            assert!(load(py, &policy, &root).unwrap().is(&empty));
        }
    });
}

#[test]
fn missing_file_or_session_table_is_generic_empty() {
    let case = Case::new();
    Python::attach(|py| {
        let policy = module(py, "conductor.session_policy");
        let empty = policy.getattr("EMPTY_SESSION_POLICY").unwrap();
        assert!(load(py, &policy, case.root()).unwrap().is(&empty));
        case.write("pyproject.toml", "[tool.conductor]\n");
        assert!(load(py, &policy, case.root()).unwrap().is(&empty));
    });
}

#[test]
fn missing_repository_is_not_a_generic_policy() {
    let case = Case::new();
    Python::attach(|py| {
        let policy = module(py, "conductor.session_policy");
        assert_error(
            py,
            load(py, &policy, &case.root().join("missing")).unwrap_err(),
            &policy.getattr("SessionPolicyError").unwrap(),
            "existing directory",
        );
    });
}

#[test]
fn present_policy_is_complete_and_strict() {
    for body in [
        "[tool",
        "[tool]\nconductor = []\n",
        "[tool.conductor]\nsession = []\n",
        "[tool.conductor.session]\npreamble = []\n",
        "[tool.conductor.session]\npreamble = []\nstanding_mandates = []\nextra = []\n",
        "[tool.conductor.session]\npreamble = [1]\nstanding_mandates = []\n",
        "[tool.conductor.session]\npreamble = []\nstanding_mandates = [\"\"]\n",
        "[tool.conductor.session]\npreamble = [\" \"]\nstanding_mandates = []\n",
    ] {
        let case = Case::new();
        case.write("pyproject.toml", body);
        Python::attach(|py| {
            let policy = module(py, "conductor.session_policy");
            let error = load(py, &policy, case.root()).unwrap_err();
            assert!(
                error
                    .matches(py, policy.getattr("SessionPolicyError").unwrap())
                    .unwrap(),
                "config {body:?}: {error}"
            );
        });
    }
}

#[test]
fn policy_reader_refuses_an_oversized_or_nonregular_config() {
    let case = Case::new();
    let config = case.write("pyproject.toml", &"x".repeat(64 * 1024 + 1));
    Python::attach(|py| {
        let policy = module(py, "conductor.session_policy");
        let class = policy.getattr("SessionPolicyError").unwrap();
        assert_error(
            py,
            load(py, &policy, case.root()).unwrap_err(),
            &class,
            "exceeds 64 KiB",
        );
        fs::remove_file(&config).unwrap();
        fs::create_dir(&config).unwrap();
        assert_error(
            py,
            load(py, &policy, case.root()).unwrap_err(),
            &class,
            "regular file",
        );
    });
}
