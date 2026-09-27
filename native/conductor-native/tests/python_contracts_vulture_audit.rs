#![cfg(feature = "python-compat-tests")]
//! Rust-owned contracts for the shipped fail-closed Vulture audit.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::prelude::*;
use pyo3::types::{PyAny, PyCFunction, PyDict, PyFrozenSet, PyModule};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use support::{assert_error, module, path, text, AttrPatch, Case};

const MESSAGE: &str = "unused variable 'value' (100% confidence)";

fn isolated_case() -> Case {
    let mut case = Case::new();
    case.remove_env("CONDUCTOR_VULTURE_WHITELIST");
    case
}

fn vulture<'py>(py: Python<'py>) -> Bound<'py, PyModule> {
    module(py, "conductor.candidate_review.vulture_audit")
}

fn py_json<'py>(py: Python<'py>, value: Value) -> Bound<'py, PyAny> {
    module(py, "json")
        .getattr("loads")
        .unwrap()
        .call1((value.to_string(),))
        .unwrap()
}

fn date(py: Python<'_>, offset_days: i32) -> String {
    let datetime = module(py, "datetime");
    let today = datetime
        .getattr("date")
        .unwrap()
        .call_method0("today")
        .unwrap();
    let delta = datetime
        .getattr("timedelta")
        .unwrap()
        .call1((offset_days,))
        .unwrap();
    today
        .call_method1("__add__", (delta,))
        .unwrap()
        .call_method0("isoformat")
        .unwrap()
        .extract()
        .unwrap()
}

fn baseline(py: Python<'_>, entries: Value) -> Value {
    let count = entries.as_object().unwrap().len();
    json!({
        "schema_version": 1,
        "generated_from_tree": ["00000000","11111111","22222222","33333333","44444444"],
        "expires": date(py, 30),
        "count": count,
        "entries": entries,
    })
}

fn entry(py: Python<'_>, source: &str, message: &str) -> (String, Value) {
    let key: String = vulture(py)
        .getattr("_key")
        .unwrap()
        .call1((source, message))
        .unwrap()
        .extract()
        .unwrap();
    let item = json!({
        "path": source,
        "message": message,
        "owner": "governance-test",
        "justification": "Exact test-only debt with a bounded expiration date.",
        "expires": date(py, 30),
    });
    (key, item)
}

fn entry_map(key: &str, item: Value) -> Value {
    let mut entries = serde_json::Map::new();
    entries.insert(key.to_owned(), item);
    Value::Object(entries)
}

fn write_baseline(case: &Case, payload: Value) -> PathBuf {
    case.write("baseline.json", &payload.to_string())
}

fn error_class<'py>(py: Python<'py>) -> Bound<'py, PyAny> {
    vulture(py).getattr("VultureAuditError").unwrap()
}

fn load<'py>(py: Python<'py>, file: &Path) -> PyResult<Bound<'py, PyAny>> {
    vulture(py)
        .getattr("_load_baseline")
        .unwrap()
        .call1((path(py, file),))
}

fn assert_audit_error(py: Python<'_>, result: PyResult<Bound<'_, PyAny>>, message: &str) {
    assert_error(py, result.unwrap_err(), &error_class(py), message);
}

fn capture<'py>(py: Python<'py>, stream: &str) -> (Bound<'py, PyAny>, AttrPatch) {
    let output = module(py, "io")
        .getattr("StringIO")
        .unwrap()
        .call0()
        .unwrap();
    let patch = AttrPatch::replace(module(py, "sys").as_any(), stream, &output);
    (output, patch)
}

fn mock_return<'py>(py: Python<'py>, value: &Bound<'py, PyAny>) -> Bound<'py, PyAny> {
    let kwargs = PyDict::new(py);
    kwargs.set_item("return_value", value).unwrap();
    module(py, "unittest.mock")
        .getattr("Mock")
        .unwrap()
        .call((), Some(&kwargs))
        .unwrap()
}

fn stub_analyzer(py: Python<'_>, code: i32, stdout: &str, stderr: &str) -> AttrPatch {
    let body = vulture(py);
    let stdout = stdout.to_owned();
    let stderr = stderr.to_owned();
    let run = PyCFunction::new_closure(
        py,
        None,
        None,
        move |args, _kwargs| -> PyResult<Py<PyAny>> {
            let py = args.py();
            let command = args.get_item(0)?;
            Ok(py
                .import("subprocess")?
                .getattr("CompletedProcess")?
                .call1((command, code, stdout.as_str(), stderr.as_str()))?
                .unbind())
        },
    )
    .unwrap();
    AttrPatch::replace(&body.getattr("subprocess").unwrap(), "run", &run)
}

fn audit<'py>(
    py: Python<'py>,
    baseline: &Path,
    paths: &[&str],
    changed: Option<&[&str]>,
) -> PyResult<Bound<'py, PyAny>> {
    let body = vulture(py);
    let arguments = paths.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
    let kwargs = PyDict::new(py);
    if let Some(changed) = changed {
        kwargs
            .set_item(
                "changed_files",
                PyFrozenSet::new(py, changed.iter().copied()).unwrap(),
            )
            .unwrap();
    }
    body.getattr("run_audit")
        .unwrap()
        .call((path(py, baseline), arguments), Some(&kwargs))
}

#[test]
fn baseline_rejects_count_and_key_mismatch() {
    let case = isolated_case();
    Python::attach(|py| {
        let (key, item) = entry(py, "sample.py", MESSAGE);
        let mut payload = baseline(py, entry_map(&key, item.clone()));
        payload["count"] = json!(2);
        let file = write_baseline(&case, payload.clone());
        assert_audit_error(py, load(py, &file), "count");

        payload["count"] = json!(1);
        payload["entries"] = json!({"wrong-key":item});
        write_baseline(&case, payload);
        assert_audit_error(py, load(py, &file), "key mismatch");
    });
}

fn real_new_finding(whitelist_exists: bool) {
    let mut case = isolated_case();
    let source = case.write(
        "candidate.py",
        "def live():\n    if False:\n        return 1\n    return 2\n",
    );
    let whitelist = case.root().join("whitelist.py");
    if whitelist_exists {
        case.write("whitelist.py", "live\n");
    }
    Python::attach(|py| {
        let configured: String = vulture(py)
            .getattr("WHITELIST_ENV")
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(configured, "CONDUCTOR_VULTURE_WHITELIST");
    });
    case.set_env("CONDUCTOR_VULTURE_WHITELIST", whitelist.to_str().unwrap());
    Python::attach(|py| {
        let file = write_baseline(&case, baseline(py, json!({})));
        let result = audit(py, &file, &[source.to_str().unwrap()], None).unwrap();
        assert_eq!(result.extract::<i32>().unwrap(), 1);
    });
}

#[test]
fn audit_blocks_new_real_finding_with_whitelist() {
    real_new_finding(true);
}

#[test]
fn audit_blocks_new_real_finding_without_whitelist() {
    real_new_finding(false);
}

#[test]
fn audit_rejects_resolved_stale_entry() {
    let case = isolated_case();
    let source = case.write("candidate.py", "VALUE = 1\n");
    Python::attach(|py| {
        let (key, item) = entry(
            py,
            source.to_str().unwrap(),
            "unused variable 'removed' (100% confidence)",
        );
        let file = write_baseline(&case, baseline(py, entry_map(&key, item)));
        assert_audit_error(
            py,
            audit(py, &file, &[source.to_str().unwrap()], None),
            "resolved findings",
        );
    });
}

#[test]
fn rejects_unbound_tree_and_untrusted_analyzer() {
    let case = isolated_case();
    Python::attach(|py| {
        let mut payload = baseline(py, json!({}));
        payload["generated_from_tree"] = json!(["too-short"]);
        let file = write_baseline(&case, payload);
        assert_audit_error(py, load(py, &file), "Git OID chunks");

        write_baseline(&case, baseline(py, json!({})));
        let _stub = stub_analyzer(py, 9, "", "analyzer crashed");
        let source = case.root().join("source.py");
        assert_audit_error(
            py,
            audit(py, &file, &[source.to_str().unwrap()], None),
            "findings are untrusted",
        );
    });
}

#[test]
fn rejects_unrecognized_success_output() {
    let case = isolated_case();
    Python::attach(|py| {
        let file = write_baseline(&case, baseline(py, json!({})));
        let _stub = stub_analyzer(py, 3, "not a finding\n", "");
        let source = case.root().join("source.py");
        assert_audit_error(
            py,
            audit(py, &file, &[source.to_str().unwrap()], None),
            "unrecognized Vulture output",
        );
    });
}

#[test]
fn baseline_schema_and_entry_failures_are_blocking() {
    let case = isolated_case();
    Python::attach(|py| {
        let valid = baseline(py, json!({}));
        let mut wrong_schema = valid.clone();
        wrong_schema["schema_version"] = json!(2);
        let mut expired = valid.clone();
        expired["expires"] = json!(date(py, -1));
        for payload in [json!({}), wrong_schema, expired] {
            let file = write_baseline(&case, payload);
            assert_audit_error(py, load(py, &file), "Vulture baseline");
        }
        assert_audit_error(
            py,
            load(py, &case.root().join("missing.json")),
            "unreadable",
        );
        let body = vulture(py);
        for value in [Value::Null, json!("not-a-date")] {
            let kwargs = PyDict::new(py);
            kwargs.set_item("field", "probe").unwrap();
            let result = body
                .getattr("_iso_date")
                .unwrap()
                .call((py_json(py, value),), Some(&kwargs));
            assert_audit_error(py, result, "ISO date");
        }
        let parsed = body
            .getattr("_parse_output")
            .unwrap()
            .call1(("\n",))
            .unwrap();
        assert!(parsed.eq(PyDict::new(py)).unwrap());

        let (key, item) = entry(py, "sample.py", MESSAGE);
        let mut bad_path = item.clone();
        bad_path["path"] = json!("");
        let mut bad_message = item.clone();
        bad_message["message"] = json!("");
        let mut bad_owner = item.clone();
        bad_owner["owner"] = json!("");
        let mut bad_justification = item.clone();
        bad_justification["justification"] = json!("short");
        let mut bad_date = item.clone();
        bad_date["expires"] = json!(date(py, -1));
        for invalid in [
            json!({}),
            bad_path,
            bad_message,
            bad_owner,
            bad_justification,
            bad_date,
        ] {
            assert_audit_error(
                py,
                body.getattr("_validate_entry")
                    .unwrap()
                    .call1((key.as_str(), py_json(py, invalid))),
                "Vulture baseline entry",
            );
        }
    });
}

#[test]
fn main_returns_audit_error_for_missing_baseline() {
    let case = isolated_case();
    let missing = case.root().join("missing.json");
    Python::attach(|py| {
        let (stderr, _capture) = capture(py, "stderr");
        let result = vulture(py)
            .getattr("main")
            .unwrap()
            .call1((vec![
                "--baseline".to_owned(),
                missing.to_str().unwrap().to_owned(),
                "source.py".to_owned(),
            ],))
            .unwrap();
        assert_eq!(result.extract::<i32>().unwrap(), 2);
        assert!(
            text(&stderr.call_method0("getvalue").unwrap()).contains("Vulture audit incomplete")
        );
    });
}

#[test]
fn changed_file_caused_blocks() {
    let case = isolated_case();
    Python::attach(|py| {
        let file = write_baseline(&case, baseline(py, json!({})));
        let _stub = stub_analyzer(
            py,
            0,
            "changed.py:5: unused variable 'value' (100% confidence)\n",
            "",
        );
        assert_eq!(
            audit(py, &file, &["changed.py"], Some(&["changed.py"]))
                .unwrap()
                .extract::<i32>()
                .unwrap(),
            1
        );
    });
}

#[test]
fn inherited_finding_does_not_block() {
    let case = isolated_case();
    Python::attach(|py| {
        let file = write_baseline(&case, baseline(py, json!({})));
        let _stub = stub_analyzer(
            py,
            0,
            "untouched.py:5: unused variable 'value' (100% confidence)\n",
            "",
        );
        assert_eq!(
            audit(py, &file, &["untouched.py"], Some(&["changed.py"]))
                .unwrap()
                .extract::<i32>()
                .unwrap(),
            0
        );
    });
}

#[test]
fn no_changed_files_blocks_every_new_finding() {
    let case = isolated_case();
    Python::attach(|py| {
        let file = write_baseline(&case, baseline(py, json!({})));
        let _stub = stub_analyzer(
            py,
            0,
            "untouched.py:5: unused variable 'value' (100% confidence)\n",
            "",
        );
        assert_eq!(
            audit(py, &file, &["untouched.py"], None)
                .unwrap()
                .extract::<i32>()
                .unwrap(),
            1
        );
    });
}

#[test]
fn no_new_findings_exits_zero_either_way() {
    let case = isolated_case();
    Python::attach(|py| {
        let (key, item) = entry(py, "known.py", MESSAGE);
        let file = write_baseline(&case, baseline(py, entry_map(&key, item)));
        let _stub = stub_analyzer(py, 0, &format!("known.py:5: {MESSAGE}\n"), "");
        assert_eq!(
            audit(py, &file, &["known.py"], None)
                .unwrap()
                .extract::<i32>()
                .unwrap(),
            0
        );
        assert_eq!(
            audit(py, &file, &["known.py"], Some(&["known.py"]))
                .unwrap()
                .extract::<i32>()
                .unwrap(),
            0
        );
    });
}

#[test]
fn changed_baseline_only_file_not_reported_as_caused() {
    let case = isolated_case();
    Python::attach(|py| {
        let message = "unused variable 'known' (100% confidence)";
        let (key, item) = entry(py, "baselined.py", message);
        let file = write_baseline(&case, baseline(py, entry_map(&key, item)));
        let output = format!("baselined.py:5: {message}\nunrelated.py:9: unused variable 'other' (100% confidence)\n");
        let _stub = stub_analyzer(py, 0, &output, "");
        assert_eq!(
            audit(
                py,
                &file,
                &["baselined.py", "unrelated.py"],
                Some(&["baselined.py"])
            )
            .unwrap()
            .extract::<i32>()
            .unwrap(),
            0
        );
    });
}

#[test]
fn missing_vulture_is_audit_error() {
    let case = isolated_case();
    let source = case.write("candidate.py", "def live():\n    return 1\n");
    Python::attach(|py| {
        let body = vulture(py);
        let which = mock_return(py, &py.None().into_bound(py));
        let _which = AttrPatch::replace(&body.getattr("shutil").unwrap(), "which", &which);
        let file = write_baseline(&case, baseline(py, json!({})));
        assert_audit_error(
            py,
            audit(py, &file, &[source.to_str().unwrap()], None),
            "not installed or not on PATH",
        );
    });
}
