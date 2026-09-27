#![cfg(feature = "python-compat-tests")]
//! Rust assertions for Vulture baseline initialization contracts.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyList, PyModule, PyTuple};
use serde_json::{json, Value};
use std::fs;
use std::path::Path;
use support::{assert_error, module, path, text, AttrPatch, Case};

fn vulture<'py>(py: Python<'py>) -> Bound<'py, PyModule> {
    module(py, "conductor.candidate_review.vulture_baseline_init")
}

fn mock<'py>(py: Python<'py>, key: &str, value: &Bound<'py, PyAny>) -> Bound<'py, PyAny> {
    let kwargs = PyDict::new(py);
    kwargs.set_item(key, value).unwrap();
    module(py, "unittest.mock")
        .getattr("Mock")
        .unwrap()
        .call((), Some(&kwargs))
        .unwrap()
}

fn completed<'py>(py: Python<'py>, code: i64, stdout: &str, stderr: &str) -> Bound<'py, PyAny> {
    let kwargs = PyDict::new(py);
    kwargs.set_item("returncode", code).unwrap();
    kwargs.set_item("stdout", stdout).unwrap();
    kwargs.set_item("stderr", stderr).unwrap();
    module(py, "types")
        .getattr("SimpleNamespace")
        .unwrap()
        .call((), Some(&kwargs))
        .unwrap()
}

fn capture<'py>(py: Python<'py>, stream: &str) -> (Bound<'py, PyAny>, AttrPatch) {
    let sink = module(py, "io")
        .getattr("StringIO")
        .unwrap()
        .call0()
        .unwrap();
    let guard = AttrPatch::replace(&module(py, "sys").into_any(), stream, &sink);
    (sink, guard)
}

fn run_findings<'py>(py: Python<'py>, root: &Path) -> PyResult<Bound<'py, PyAny>> {
    vulture(py)
        .getattr("run_vulture_findings")
        .unwrap()
        .call1((path(py, root), vec!["src"]))
}

fn main(py: Python<'_>, baseline: &Path) -> i64 {
    vulture(py)
        .getattr("main")
        .unwrap()
        .call1((vec![
            "--baseline".to_owned(),
            baseline.to_string_lossy().into_owned(),
            "--expires".to_owned(),
            "2026-09-15".to_owned(),
            "src".to_owned(),
        ],))
        .unwrap()
        .extract()
        .unwrap()
}

fn baseline_stub<'py>(py: Python<'py>, findings: &Bound<'py, PyDict>) -> Bound<'py, PyAny> {
    let baseline = PyDict::new(py);
    baseline.set_item("schema_version", 1).unwrap();
    baseline
        .set_item("generated_from_tree", vec!["a"; 5])
        .unwrap();
    baseline.set_item("expires", "2026-09-15").unwrap();
    baseline.set_item("count", 0).unwrap();
    baseline.set_item("entries", PyDict::new(py)).unwrap();
    let pair = PyTuple::new(py, [baseline.into_any(), findings.clone().into_any()]).unwrap();
    mock(py, "return_value", &pair.into_any())
}

fn parsed_json(py: Python<'_>, value: &Bound<'_, PyAny>) -> Value {
    let serialized = module(py, "json")
        .getattr("dumps")
        .unwrap()
        .call1((value,))
        .unwrap()
        .extract::<String>()
        .unwrap();
    serde_json::from_str(&serialized).unwrap()
}

#[test]
fn git_tree_chunks_splits_real_oid_and_uses_exact_command() {
    let case = Case::new();
    Python::attach(|py| {
        let vbi = vulture(py);
        let oid = "abcdef01".repeat(5);
        assert_eq!(oid.len(), 40);
        let result = completed(py, 0, &format!("{oid}\n"), "");
        let run = mock(py, "return_value", &result);
        let _patch = AttrPatch::replace(&vbi.getattr("subprocess").unwrap(), "run", &run);
        let chunks: Vec<String> = vbi
            .getattr("git_tree_chunks")
            .unwrap()
            .call1((path(py, case.root()),))
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(chunks, vec!["abcdef01"; 5]);
        assert!(chunks.iter().all(|chunk| chunk.len() == 8));
        let call = run.getattr("call_args").unwrap();
        assert_eq!(
            call.getattr("args")
                .unwrap()
                .get_item(0)
                .unwrap()
                .extract::<Vec<String>>()
                .unwrap(),
            ["git", "rev-parse", "HEAD"]
        );
        let kwargs = call.getattr("kwargs").unwrap();
        assert_eq!(
            text(&kwargs.get_item("cwd").unwrap()),
            case.root().to_string_lossy()
        );
        for key in ["capture_output", "text"] {
            assert!(kwargs.get_item(key).unwrap().extract::<bool>().unwrap());
        }
        assert!(!kwargs.get_item("check").unwrap().extract::<bool>().unwrap());
    });
}

#[test]
fn git_tree_chunks_raises_on_git_failure() {
    let case = Case::new();
    Python::attach(|py| {
        let vbi = vulture(py);
        let run = mock(
            py,
            "return_value",
            &completed(py, 128, "", "fatal: not a git repository"),
        );
        let _patch = AttrPatch::replace(&vbi.getattr("subprocess").unwrap(), "run", &run);
        let error = vbi
            .getattr("git_tree_chunks")
            .unwrap()
            .call1((path(py, case.root()),))
            .unwrap_err();
        assert_error(
            py,
            error,
            &vbi.getattr("VultureBaselineInitError").unwrap(),
            "git rev-parse HEAD failed",
        );
    });
}

#[test]
fn git_tree_chunks_raises_on_truncated_oid() {
    let case = Case::new();
    Python::attach(|py| {
        let vbi = vulture(py);
        let run = mock(py, "return_value", &completed(py, 0, "short\n", ""));
        let _patch = AttrPatch::replace(&vbi.getattr("subprocess").unwrap(), "run", &run);
        let error = vbi
            .getattr("git_tree_chunks")
            .unwrap()
            .call1((path(py, case.root()),))
            .unwrap_err();
        assert_error(
            py,
            error,
            &vbi.getattr("VultureBaselineInitError").unwrap(),
            "git rev-parse HEAD failed",
        );
    });
}

#[test]
fn run_vulture_findings_raises_when_tool_missing() {
    let case = Case::new();
    Python::attach(|py| {
        let vbi = vulture(py);
        let which = mock(py, "return_value", &py.None().into_bound(py));
        let _patch = AttrPatch::replace(&vbi.getattr("shutil").unwrap(), "which", &which);
        let error = run_findings(py, case.root()).unwrap_err();
        assert_error(
            py,
            error,
            &vbi.getattr("VultureBaselineInitError").unwrap(),
            "not installed",
        );
        assert_eq!(
            which
                .getattr("call_args")
                .unwrap()
                .getattr("args")
                .unwrap()
                .extract::<Vec<String>>()
                .unwrap(),
            ["vulture"]
        );
    });
}

#[test]
fn run_vulture_findings_raises_on_unexpected_exit() {
    let case = Case::new();
    Python::attach(|py| {
        let vbi = vulture(py);
        let _which = AttrPatch::replace(
            &vbi.getattr("shutil").unwrap(),
            "which",
            &mock(
                py,
                "return_value",
                &"/usr/bin/vulture".into_pyobject(py).unwrap().into_any(),
            ),
        );
        let _run = AttrPatch::replace(
            &vbi.getattr("subprocess").unwrap(),
            "run",
            &mock(py, "return_value", &completed(py, 1, "", "boom")),
        );
        let error = run_findings(py, case.root()).unwrap_err();
        assert_error(
            py,
            error,
            &vbi.getattr("VultureBaselineInitError").unwrap(),
            "vulture exited 1",
        );
    });
}

#[test]
fn run_vulture_findings_parses_real_output() {
    let case = Case::new();
    Python::attach(|py| {
        let vbi = vulture(py);
        let _which = AttrPatch::replace(
            &vbi.getattr("shutil").unwrap(),
            "which",
            &mock(
                py,
                "return_value",
                &"/usr/bin/vulture".into_pyobject(py).unwrap().into_any(),
            ),
        );
        let output = "pkg/mod.py:12: unused variable 'x' (100% confidence)\n";
        let _run = AttrPatch::replace(
            &vbi.getattr("subprocess").unwrap(),
            "run",
            &mock(py, "return_value", &completed(py, 3, output, "")),
        );
        let findings = run_findings(py, case.root()).unwrap();
        assert_eq!(findings.len().unwrap(), 1);
        let values = findings.call_method0("values").unwrap();
        let finding = values.try_iter().unwrap().next().unwrap().unwrap();
        assert_eq!(text(&finding.get_item("path").unwrap()), "pkg/mod.py");
        assert_eq!(
            finding.get_item("line").unwrap().extract::<i64>().unwrap(),
            12
        );
    });
}

#[test]
fn run_vulture_findings_builds_exact_command() {
    let case = Case::new();
    Python::attach(|py| {
        let vbi = vulture(py);
        let which = mock(
            py,
            "return_value",
            &"/usr/bin/vulture".into_pyobject(py).unwrap().into_any(),
        );
        let _which = AttrPatch::replace(&vbi.getattr("shutil").unwrap(), "which", &which);
        let whitelist = mock(
            py,
            "return_value",
            &PyList::new(py, ["allow.py"]).unwrap().into_any(),
        );
        let _whitelist = AttrPatch::replace(&vbi, "whitelist_args", &whitelist);
        let run = mock(py, "return_value", &completed(py, 0, "", ""));
        let _run = AttrPatch::replace(&vbi.getattr("subprocess").unwrap(), "run", &run);
        assert_eq!(run_findings(py, case.root()).unwrap().len().unwrap(), 0);
        assert_eq!(which.getattr("call_args_list").unwrap().len().unwrap(), 1);
        assert_eq!(
            which
                .getattr("call_args")
                .unwrap()
                .getattr("args")
                .unwrap()
                .extract::<Vec<String>>()
                .unwrap(),
            ["vulture"]
        );
        assert_eq!(
            whitelist
                .getattr("call_args")
                .unwrap()
                .getattr("args")
                .unwrap()
                .get_item(0)
                .unwrap()
                .str()
                .unwrap()
                .to_str()
                .unwrap(),
            case.root().to_str().unwrap()
        );
        let call = run.getattr("call_args").unwrap();
        let command: Vec<String> = call
            .getattr("args")
            .unwrap()
            .get_item(0)
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(
            command,
            [
                "/usr/bin/vulture",
                "src",
                "allow.py",
                "--min-confidence",
                "80",
                "--exclude",
                "*/.venv/*,*/node_modules/*,*/__pycache__/*,*/.run/*,*/tests/*,*/migrations/*"
            ]
        );
        let kwargs = call.getattr("kwargs").unwrap();
        assert_eq!(
            text(&kwargs.get_item("cwd").unwrap()),
            case.root().to_string_lossy()
        );
        for key in ["capture_output", "text"] {
            assert!(kwargs.get_item(key).unwrap().extract::<bool>().unwrap());
        }
        assert_eq!(text(&kwargs.get_item("errors").unwrap()), "replace");
        assert!(!kwargs.get_item("check").unwrap().extract::<bool>().unwrap());
    });
}

#[test]
fn build_baseline_is_empty_allowlist_with_real_tree() {
    let case = Case::new();
    Python::attach(|py| {
        let vbi = vulture(py);
        let chunks = PyList::new(py, ["00000000"; 5]).unwrap();
        let _tree = AttrPatch::replace(
            &vbi,
            "git_tree_chunks",
            &mock(py, "return_value", &chunks.into_any()),
        );
        let _findings = AttrPatch::replace(
            &vbi,
            "run_vulture_findings",
            &mock(py, "return_value", &PyDict::new(py).into_any()),
        );
        let result = vbi
            .getattr("build_baseline")
            .unwrap()
            .call1((path(py, case.root()), vec!["src"], "2026-09-15"))
            .unwrap();
        assert_eq!(
            parsed_json(py, &result.get_item(0).unwrap()),
            json!({"schema_version":1,"generated_from_tree":["00000000","00000000","00000000","00000000","00000000"],"expires":"2026-09-15","count":0,"entries":{}})
        );
        assert_eq!(parsed_json(py, &result.get_item(1).unwrap()), json!({}));
    });
}

#[test]
fn main_writes_schema_valid_baseline_file() {
    let case = Case::new();
    Python::attach(|py| {
        let vbi = vulture(py);
        let _build =
            AttrPatch::replace(&vbi, "build_baseline", &baseline_stub(py, &PyDict::new(py)));
        let (stderr, _err) = capture(py, "stderr");
        let output = case.root().join("vulture_baseline.json");
        assert_eq!(main(py, &output), 0);
        let raw = fs::read_to_string(&output).unwrap();
        assert!(raw.ends_with('\n'));
        let payload: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(payload["count"], 0);
        assert_eq!(payload["entries"], json!({}));
        assert_eq!(payload["expires"], "2026-09-15");
        assert!(!text(&stderr.call_method0("getvalue").unwrap()).contains("NOTE"));
    });
}

#[test]
fn main_reports_current_findings_as_debt_on_stderr() {
    let case = Case::new();
    Python::attach(|py| {
        let vbi = vulture(py);
        let row = PyDict::new(py);
        row.set_item("path", "pkg/mod.py").unwrap();
        row.set_item("line", 12).unwrap();
        row.set_item("message", "unused variable 'x'").unwrap();
        let findings = PyDict::new(py);
        findings.set_item("pkg/mod.py:12", row).unwrap();
        let _build = AttrPatch::replace(&vbi, "build_baseline", &baseline_stub(py, &findings));
        let (stderr, _err) = capture(py, "stderr");
        let output = case.root().join("vulture_baseline.json");
        assert_eq!(main(py, &output), 0);
        let printed = text(&stderr.call_method0("getvalue").unwrap());
        assert!(printed.contains("NOTE: 1 current Vulture finding"));
        assert!(printed.contains("pkg/mod.py:12: unused variable 'x'"));
    });
}

#[test]
fn main_reports_analyzer_error_without_writing_file() {
    let case = Case::new();
    Python::attach(|py| {
        let vbi = vulture(py);
        let error = vbi
            .getattr("VultureBaselineInitError")
            .unwrap()
            .call1(("vulture is not installed",))
            .unwrap();
        let _build = AttrPatch::replace(&vbi, "build_baseline", &mock(py, "side_effect", &error));
        let (stderr, _err) = capture(py, "stderr");
        let output = case.root().join("vulture_baseline.json");
        assert_eq!(main(py, &output), 2);
        assert!(!output.exists());
        assert!(text(&stderr.call_method0("getvalue").unwrap()).contains("not installed"));
    });
}
