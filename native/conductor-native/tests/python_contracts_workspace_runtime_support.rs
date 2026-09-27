#![cfg(feature = "python-compat-tests")]
//! Boundary contracts for foreign-root preamble and Grok inspection support.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyList, PyModule};
use serde_json::json;
use std::ffi::CString;
use std::path::Path;
use support::{module, path, text, Case};

const CALLBACKS: &str = r#"
import subprocess
import sys
calls = []
def run(argv, **kwargs):
    calls.append((argv, kwargs))
    stdout = preamble if argv[:4] == [sys.executable, '-P', '-m', 'conductor.session_preamble'] else grok
    return subprocess.CompletedProcess(argv, 0, stdout, '')
def digest(_raw):
    return 'digest'
def grok_argv():
    return ['grok', 'inspect', '--json']
"#;

fn inspect_payload(root: &Path) -> String {
    let hooks = ["Read|read|read_file", "Bash|run_shell_command|shell"].map(|matcher| {
        json!({"event":"pre_tool_use", "matcher":matcher,
            "source":{"path":root.join(".grok/hooks").to_str().unwrap()}})
    });
    json!({"projectRoot":root.to_str().unwrap(),"projectTrusted":true,"hooks":hooks}).to_string()
}

fn callbacks<'py>(py: Python<'py>, preamble: &str, grok: &str) -> Bound<'py, PyDict> {
    let globals = PyDict::new(py);
    globals.set_item("preamble", preamble).unwrap();
    globals.set_item("grok", grok).unwrap();
    py.run(&CString::new(CALLBACKS).unwrap(), Some(&globals), None)
        .unwrap();
    globals
}

fn exercise<'py>(
    py: Python<'py>,
    support_module: &Bound<'py, PyModule>,
    root: &Bound<'py, PyAny>,
    globals: &Bound<'py, PyDict>,
) -> (Bound<'py, PyList>, Bound<'py, PyDict>) {
    let failures = PyList::empty(py);
    let evidence = PyDict::new(py);
    let kwargs = PyDict::new(py);
    for (key, name) in [
        ("run_command", "run"),
        ("sha256_bytes", "digest"),
        ("grok_argv", "grok_argv"),
    ] {
        kwargs
            .set_item(key, globals.get_item(name).unwrap().unwrap())
            .unwrap();
    }
    support_module
        .getattr("check_preamble_and_grok")
        .unwrap()
        .call((root, &failures, &evidence), Some(&kwargs))
        .unwrap();
    (failures, evidence)
}

#[test]
fn foreign_root_uses_package_import_path_and_accepts_empty_policy() {
    let case = Case::new();
    let _cwd = case.chdir("cwd");
    let root = std::env::current_dir().unwrap().join("foreign-project");
    Python::attach(|py| {
        let support_module = module(py, "conductor.workspace_runtime_support");
        let preamble = json!({"hookSpecificOutput":
            {"hookEventName":"SessionStart","additionalContext":""}})
        .to_string();
        let globals = callbacks(py, &preamble, &inspect_payload(&root));
        let (failures, evidence) = exercise(
            py,
            &support_module,
            &path(py, Path::new("foreign-project")),
            &globals,
        );
        assert_eq!(failures.len(), 0);
        let calls = globals.get_item("calls").unwrap().unwrap();
        let first = calls.get_item(0).unwrap();
        let argv: Vec<String> = first.get_item(0).unwrap().extract().unwrap();
        let sys = PyModule::import(py, "sys").unwrap();
        let executable: String = sys.getattr("executable").unwrap().extract().unwrap();
        assert_eq!(
            argv,
            [
                executable,
                "-P".into(),
                "-m".into(),
                "conductor.session_preamble".into(),
                "hook".into(),
                "--state".into(),
                root.join("conductor/active_state.json")
                    .to_str()
                    .unwrap()
                    .into(),
                "--repo".into(),
                root.to_str().unwrap().into(),
            ]
        );
        let kwargs = first.get_item(1).unwrap();
        assert_eq!(
            text(&kwargs.get_item("cwd").unwrap()),
            root.to_str().unwrap()
        );
        assert_eq!(
            kwargs
                .get_item("timeout")
                .unwrap()
                .extract::<i64>()
                .unwrap(),
            30
        );
        let environment = kwargs.get_item("env").unwrap();
        let import_path: String = environment
            .get_item("PYTHONPATH")
            .unwrap()
            .extract()
            .unwrap();
        let module_path = support_module.getattr("__file__").unwrap();
        let expected = std::path::Path::new(&text(&module_path))
            .canonicalize()
            .unwrap()
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        assert_eq!(import_path.split(':').next().unwrap(), expected);
        let session = evidence.get_item("session-preamble").unwrap().unwrap();
        assert_eq!(
            session
                .get_item("returncode")
                .unwrap()
                .extract::<i64>()
                .unwrap(),
            0
        );
        assert!(session
            .get_item("hook_payload_valid")
            .unwrap()
            .extract::<bool>()
            .unwrap());
        assert!(session
            .get_item("passed")
            .unwrap()
            .extract::<bool>()
            .unwrap());
        assert_eq!(
            session
                .get_item("stdout_sha256")
                .unwrap()
                .extract::<String>()
                .unwrap(),
            "digest"
        );
        assert!(evidence
            .get_item("grok-inspect")
            .unwrap()
            .unwrap()
            .get_item("passed")
            .unwrap()
            .extract::<bool>()
            .unwrap());
    });
}

#[test]
fn malformed_preamble_payloads_fail_even_when_grok_passes() {
    let case = Case::new();
    let root = case.mkdir("generic-project");
    Python::attach(|py| {
        let support_module = module(py, "conductor.workspace_runtime_support");
        for output in [
            "not json".to_owned(),
            json!({"hookSpecificOutput":{"hookEventName":"Other"}}).to_string(),
            json!({"hookSpecificOutput":{"hookEventName":"SessionStart",
                "additionalContext":[]}})
            .to_string(),
        ] {
            let globals = callbacks(py, &output, &inspect_payload(&root));
            let (failures, evidence) = exercise(py, &support_module, &path(py, &root), &globals);
            assert_eq!(
                failures.extract::<Vec<String>>().unwrap(),
                ["session-preamble"]
            );
            assert!(!evidence
                .get_item("session-preamble")
                .unwrap()
                .unwrap()
                .get_item("hook_payload_valid")
                .unwrap()
                .extract::<bool>()
                .unwrap());
            assert!(evidence
                .get_item("grok-inspect")
                .unwrap()
                .unwrap()
                .get_item("passed")
                .unwrap()
                .extract::<bool>()
                .unwrap());
        }
    });
}
