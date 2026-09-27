#![cfg(feature = "python-compat-tests")]
//! Rust-owned contracts for mutation coverage inventory, CLI, and evidence readers.

#[path = "python_contracts/agent_comm_support.rs"]
#[allow(dead_code)]
mod comm_support;
#[path = "python_contracts/mutation_coverage_scope_support.rs"]
#[allow(dead_code)]
mod fixture;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use comm_support::{buffer_text, capture, clear_buffer, py_json};
use fixture::{commit, constant_callback, equal, json_object, registry, repo, strict_callback};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyList, PyTuple};
use serde_json::{json, Value};
use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};
use support::{assert_error, module, path, AttrPatch, Case};

fn coverage<'py>(py: Python<'py>) -> Bound<'py, PyAny> {
    module(py, "conductor.mutation_coverage").into_any()
}

fn changed_result(missing: Value, counts: Value) -> Value {
    let paths: Vec<Value> = missing
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["path"].clone())
        .collect();
    let has_missing = !paths.is_empty();
    json!({
        "schema_version": "llm.mutation-testing.changed-evidence.v2",
        "status": if has_missing { "FAIL" } else { "PASS" },
        "enforcement": "changed_tests",
        "checked_test_paths": if has_missing { paths } else { vec![json!("covered.py")] },
        "evidence": if has_missing { json!([]) } else { json!([{"path":"covered.py"}]) },
        "missing_evidence": missing,
        "rejection_counts": counts,
        "malformed_receipts": [],
    })
}

fn call_main(py: Python<'_>, command: &[&str]) -> i64 {
    coverage(py)
        .getattr("main")
        .unwrap()
        .call1((PyList::new(py, command).unwrap(),))
        .unwrap()
        .extract()
        .unwrap()
}

fn patch_constant<'py>(
    py: Python<'py>,
    subject: &Bound<'py, PyAny>,
    name: &str,
    value: Value,
) -> AttrPatch {
    AttrPatch::replace(
        subject,
        name,
        constant_callback(py, &py_json(py, value)).as_any(),
    )
}

fn missing_campaign(path: &str) -> Value {
    json!({"path":path,"reason":"no registered campaign ranks this test file","reason_kind":"no_campaign","campaigns":[],"receipt_rejections":[]})
}

#[test]
fn test_is_test_path_matches_python_and_javascript_specs() {
    let _case = Case::new();
    Python::attach(|py| {
        let subject = coverage(py);
        let native = subject
            .getattr("is_mutation_test_path_native")
            .unwrap()
            .unbind();
        let calls = Arc::new(Mutex::new(Vec::<(String, Vec<String>)>::new()));
        let record = Arc::clone(&calls);
        let tracker = strict_callback(py, &["path", "patterns"], &[], move |py, args| {
            let name: String = args.get_item("path")?.extract()?;
            let patterns: Vec<String> = args.get_item("patterns")?.extract()?;
            record
                .lock()
                .unwrap()
                .push((name.clone(), patterns.clone()));
            Ok(native
                .bind(py)
                .call1((args.get_item("path")?, args.get_item("patterns")?))?
                .unbind())
        });
        let _patch = AttrPatch::replace(&subject, "is_mutation_test_path_native", tracker.as_any());
        let patterns = PyTuple::new(py, ["**/test_*.py", "**/*.spec.js"]).unwrap();
        for (name, result) in [
            ("research/tests/test_foo.py", true),
            ("aria_designer/e2e/designer.spec.js", true),
            ("research/tools/foo.py", false),
        ] {
            assert_eq!(
                subject
                    .getattr("is_test_path")
                    .unwrap()
                    .call1((name, &patterns))
                    .unwrap()
                    .extract::<bool>()
                    .unwrap(),
                result
            );
        }
        assert_eq!(calls.lock().unwrap().len(), 3);
        let extended = PyTuple::new(py, ["**/test_[fb]oo.py"]).unwrap();
        assert!(subject
            .getattr("is_test_path")
            .unwrap()
            .call1(("research/tests/test_foo.py", &extended))
            .unwrap()
            .extract::<bool>()
            .unwrap());
        assert!(!subject
            .getattr("is_test_path")
            .unwrap()
            .call1(("research/tests/test_zoo.py", &extended))
            .unwrap()
            .extract::<bool>()
            .unwrap());
        assert_eq!(calls.lock().unwrap().len(), 3);
        assert_eq!(
            calls.lock().unwrap().as_slice(),
            &[
                (
                    "research/tests/test_foo.py".into(),
                    vec!["**/test_*.py".into(), "**/*.spec.js".into()]
                ),
                (
                    "aria_designer/e2e/designer.spec.js".into(),
                    vec!["**/test_*.py".into(), "**/*.spec.js".into()]
                ),
                (
                    "research/tools/foo.py".into(),
                    vec!["**/test_*.py".into(), "**/*.spec.js".into()]
                )
            ]
        );
    });
}

#[test]
fn test_discover_and_changed_paths_include_untracked_tests() {
    let case = Case::new();
    Python::attach(|py| {
        let root = repo(&case);
        let reg = registry(py, &root);
        commit(
            &root,
            "research/tests/test_tracked.py",
            "def test_ok():\n    assert True\n",
            "tracked test",
        );
        fs::write(
            root.join("research/tests/test_new.py"),
            "def test_new():\n    assert True\n",
        )
        .unwrap();
        fs::create_dir_all(root.join("research/tools")).unwrap();
        fs::write(root.join("research/tools/not_module.py"), "x = 1\n").unwrap();
        let kw = PyDict::new(py);
        kw.set_item("repo_root", path(py, &root)).unwrap();
        let subject = coverage(py);
        equal(
            &subject
                .getattr("discover_test_paths")
                .unwrap()
                .call((path(py, &reg),), Some(&kw))
                .unwrap(),
            &PyTuple::new(
                py,
                [
                    "research/tests/test_new.py",
                    "research/tests/test_tracked.py",
                ],
            )
            .unwrap(),
        );
        equal(
            &subject
                .getattr("git_changed_test_paths")
                .unwrap()
                .call((path(py, &reg),), Some(&kw))
                .unwrap(),
            &PyTuple::new(py, ["research/tests/test_new.py"]).unwrap(),
        );
    });
}

#[test]
fn test_coverage_report_uses_verify_evidence() {
    let case = Case::new();
    Python::attach(|py| {
        let root = repo(&case);
        let reg = registry(py, &root);
        commit(
            &root,
            "research/tests/test_tracked.py",
            "def test_ok():\n    assert True\n",
            "tracked test",
        );
        let subject = coverage(py);
        let expected = json!({"status":"FAIL","evidence":[],"missing_evidence":[missing_campaign("research/tests/test_tracked.py")],"rejection_counts":{"no_campaign":1},"malformed_receipts":[]});
        let callback = strict_callback(
            py,
            &["registry_path", "paths"],
            &["repo_root"],
            move |py, args| {
                let paths = args.get_item("paths")?;
                let actual = py.import("builtins")?.getattr("list")?.call1((paths,))?;
                let expected_paths = PyList::new(py, ["research/tests/test_tracked.py"])?;
                if !actual.eq(expected_paths)? {
                    return Err(pyo3::exceptions::PyAssertionError::new_err("verify paths"));
                }
                Ok(py_json(py, expected.clone()).unbind())
            },
        );
        let _patch = AttrPatch::replace(&subject, "verify_evidence", callback.as_any());
        let kw = PyDict::new(py);
        kw.set_item("repo_root", path(py, &root)).unwrap();
        let report = subject
            .getattr("coverage_report")
            .unwrap()
            .call((path(py, &reg),), Some(&kw))
            .unwrap();
        for (key, value) in [
            ("status", json!("FAIL")),
            ("total_test_files", json!(1)),
            ("missing_test_files", json!(1)),
            ("enforcement", json!("repository_inventory")),
            ("schema_version", json!("llm.mutation-testing.coverage.v2")),
            ("rejection_counts", json!({"no_campaign":1})),
        ] {
            equal(&report.get_item(key).unwrap(), &json_object(py, value));
        }
    });
}

#[test]
fn test_changed_exit_codes_split_debt_from_defects() {
    let _case = Case::new();
    Python::attach(|py| {
        let subject = coverage(py);
        let debt = changed_result(
            json!([
            missing_campaign("src/conductor/test_new.py"),
            {"path":"src/conductor/test_old.py","reason":"no current complete PASS receipt","reason_kind":"not_pass","campaigns":["c1"],"receipt_rejections":[
                {"receipt":"r1.json","kind":"superseded","detail":"receipt superseded by r2.json"},
                {"receipt":"r0.json","kind":"runner_map_mismatch","detail":"runner component hash map mismatch"}]}]),
            json!({"no_campaign":1,"not_pass":1,"superseded":1,"runner_map_mismatch":1}),
        );
        let defect = changed_result(
            json!([{"path":"src/conductor/test_new.py","reason":"no current complete PASS receipt","reason_kind":"not_pass","campaigns":["c1"],"receipt_rejections":[{"receipt":"r1.json","kind":"decode_error","detail":"detail blob is not valid base64: oops"}]}]),
            json!({"not_pass":1,"decode_error":1}),
        );
        let covered = changed_result(json!([]), json!({}));
        let (_stdout, _patch_out) = capture(py, "stdout");
        for (result, code) in [(debt, 6), (defect, 5), (covered, 0)] {
            let _patch = patch_constant(py, &subject, "verify_changed", result);
            assert_eq!(call_main(py, &["changed", "--registry", "r.json"]), code);
        }
    });
}

#[test]
fn test_changed_github_annotations_and_summary() {
    let mut case = Case::new();
    let summary = case.root().join("summary.md");
    case.set_env("GITHUB_STEP_SUMMARY", summary.to_str().unwrap());
    Python::attach(|py| {
        let subject = coverage(py);
        let result = changed_result(
            json!([{"path":"src/conductor/test_new.py","reason":"no current complete PASS receipt","reason_kind":"not_pass","campaigns":["c1","c2"],"receipt_rejections":[
            {"receipt":"r1.json","kind":"decode_error","detail":"detail blob is not valid base64: oops"},
            {"receipt":"r0.json","kind":"superseded","detail":"receipt superseded by r1.json"}]}]),
            json!({"not_pass":1,"decode_error":1,"superseded":1}),
        );
        let _patch = patch_constant(py, &subject, "verify_changed", result);
        let (stdout, _capture) = capture(py, "stdout");
        assert_eq!(
            call_main(py, &["changed", "--registry", "r.json", "--github"]),
            5
        );
        let output = buffer_text(&stdout);
        assert!(output.contains("::warning file=src/conductor/test_new.py::"));
        assert!(output.contains("::error::src/conductor/test_new.py: r1.json:"));
        let table = fs::read_to_string(summary).unwrap();
        for part in [
            "| path | campaign | status | kind |",
            "| `src/conductor/test_new.py` | c1, c2 |",
            "not_pass (decode_error, superseded)",
        ] {
            assert!(table.contains(part), "missing {part:?}");
        }
    });
}

#[test]
fn test_coverage_subcommand_exits_on_report_status() {
    let _case = Case::new();
    Python::attach(|py| {
        let subject = coverage(py);
        let (_stdout, _capture) = capture(py, "stdout");
        for (status, code) in [("PASS", 0), ("FAIL", 5)] {
            let _patch = patch_constant(py, &subject, "coverage_report", json!({"status":status}));
            assert_eq!(call_main(py, &["coverage", "--registry", "r.json"]), code);
        }
    });
}

fn canary_clean() -> Value {
    json!({"schema_version":"llm.mutation-testing.coverage.v2","status":"FAIL","enforcement":"repository_inventory","total_test_files":9,"covered_test_files":0,"missing_test_files":9,"evidence":[],"missing_evidence":[missing_campaign("src/conductor/test_debt.py")],"rejection_counts":{"no_campaign":9,"not_pass":3,"superseded":5},"malformed_receipts":[]})
}

fn canary_unreadable(mut clean: Value) -> Value {
    clean["rejection_counts"] = json!({"no_campaign":9,"decode_error":1});
    clean["missing_evidence"] = json!([{"path":"src/conductor/test_broken.py","reason":"no current complete PASS receipt","reason_kind":"not_pass","campaigns":["c1"],"receipt_rejections":[
        {"receipt":"r1.json","kind":"decode_error","detail":"detail blob does not decompress: frame error"},
        {"receipt":"r0.json","kind":"superseded","detail":"receipt superseded by r1.json"}]}]);
    clean["malformed_receipts"] = json!(["receipts/gone.json: Expecting value"]);
    clean
}

#[test]
fn test_canary_exit_paths() {
    let _case = Case::new();
    Python::attach(|py| {
        let subject = coverage(py);
        let (stdout, _capture) = capture(py, "stdout");
        let clean = canary_clean();
        let clean_patch = patch_constant(py, &subject, "coverage_report", clean.clone());
        assert_eq!(call_main(py, &["canary", "--registry", "r.json"]), 0);
        let output = buffer_text(&stdout);
        assert!(output.contains("\"canary\""));
        assert!(output.contains("\"offending_kinds\": []"));
        drop(clean_patch);
        clear_buffer(&stdout);
        let _patch = patch_constant(py, &subject, "coverage_report", canary_unreadable(clean));
        assert_eq!(call_main(py, &["canary", "--registry", "r.json"]), 5);
        let output = buffer_text(&stdout);
        for part in [
            "\"offending_kinds\": [",
            "\"decode_error\"",
            "receipts/gone.json",
        ] {
            assert!(output.contains(part));
        }
        let report = subject
            .getattr("canary_report")
            .unwrap()
            .call1((path(py, Path::new("r.json")),))
            .unwrap();
        assert_eq!(
            report
                .get_item("canary")
                .unwrap()
                .get_item("status")
                .unwrap()
                .extract::<String>()
                .unwrap(),
            "FAIL"
        );
        equal(
            &report
                .get_item("canary")
                .unwrap()
                .get_item("offending_kinds")
                .unwrap(),
            &PyList::new(py, ["decode_error"]).unwrap(),
        );
        equal(
            &report
                .get_item("canary")
                .unwrap()
                .get_item("offending_receipts")
                .unwrap(),
            &py_json(
                py,
                json!([
            {"path":"src/conductor/test_broken.py","receipt":"r1.json","kind":"decode_error","detail":"detail blob does not decompress: frame error"},
            {"receipt":"receipts/gone.json: Expecting value","kind":"decode_error","detail":"receipts/gone.json: Expecting value"}]),
            ),
        );
    });
}

fn fixture_path(root: &Path, name: &str) -> std::path::PathBuf {
    root.join("src/conductor/testdata/coverage").join(name)
}

#[test]
fn test_mutation_testing_cli_inspect_verify_and_refuse() {
    let case = Case::new();
    Python::attach(|py| {
        let testing = module(py, "conductor.mutation_testing");
        let root: String = testing
            .getattr("REPO_ROOT")
            .unwrap()
            .str()
            .unwrap()
            .extract()
            .unwrap();
        let root = Path::new(&root);
        let campaign = fixture_path(root, "claude_bash_quiet.json");
        let registry = fixture_path(root, "claude_bash_quiet_registry.json");
        let (stdout, _capture) = capture(py, "stdout");
        let main = testing.getattr("main").unwrap();
        assert_eq!(
            main.call1((PyList::new(py, ["inspect", campaign.to_str().unwrap()]).unwrap(),))
                .unwrap()
                .extract::<i64>()
                .unwrap(),
            0
        );
        assert_eq!(
            main.call1((PyList::new(
                py,
                [
                    "verify-evidence",
                    "--registry",
                    registry.to_str().unwrap(),
                    "example/tests/test_unregistered.py"
                ]
            )
            .unwrap(),))
                .unwrap()
                .extract::<i64>()
                .unwrap(),
            5
        );
        let missing = case.root().join("missing.json");
        assert_eq!(
            main.call1((PyList::new(py, ["inspect", missing.to_str().unwrap()]).unwrap(),))
                .unwrap()
                .extract::<i64>()
                .unwrap(),
            4
        );
        assert!(buffer_text(&stdout).contains("REFUSED"));
    });
}

#[test]
fn test_mutation_testing_load_rejects_malformed_campaigns() {
    let case = Case::new();
    Python::attach(|py| {
        let testing = module(py, "conductor.mutation_testing");
        let location = case.write("campaign.json", "[]\n");
        let load = testing.getattr("load_campaign").unwrap();
        let kwargs = PyDict::new(py);
        kwargs.set_item("repo_root", path(py, case.root())).unwrap();
        assert_error(
            py,
            load.call((path(py, &location),), Some(&kwargs))
                .unwrap_err(),
            &testing.getattr("CampaignError").unwrap(),
            "JSON object",
        );
        let payload = json!({"schema_version":1,"campaign_id":"x","title":"x","language":"python","mutation_engine":"reviewed_unified_diff","expected_mutations":1,"expected_ranked_tests":1,"source_sha256":{},"ranked_tests":[],"planned_mutations":[],"mutations":[],"baseline":{"argv":["true"],"timeout_seconds":1}});
        fs::write(&location, payload.to_string()).unwrap();
        assert_error(
            py,
            load.call((path(py, &location),), Some(&kwargs))
                .unwrap_err(),
            &testing.getattr("CampaignError").unwrap(),
            "ranked_tests",
        );
    });
}

#[test]
fn test_inspect_cli_returns_not_ready() {
    let case = Case::new();
    Python::attach(|py| {
        let testing = module(py, "conductor.mutation_testing");
        let root: String = testing
            .getattr("REPO_ROOT")
            .unwrap()
            .str()
            .unwrap()
            .extract()
            .unwrap();
        let data =
            fs::read_to_string(fixture_path(Path::new(&root), "claude_bash_quiet.json")).unwrap();
        let mut payload: Value = serde_json::from_str(&data).unwrap();
        payload["mutations"] = json!([]);
        let location = case.write("campaign.json", &payload.to_string());
        let (_stdout, _capture) = capture(py, "stdout");
        let rc: i64 = testing
            .getattr("main")
            .unwrap()
            .call1((PyList::new(py, ["inspect", location.to_str().unwrap()]).unwrap(),))
            .unwrap()
            .extract()
            .unwrap();
        assert!([3, 4].contains(&rc));
    });
}

#[test]
fn test_run_command_timeout_and_ps_scan() {
    let _case = Case::new();
    Python::attach(|py| {
        let testing = module(py, "conductor.mutation_testing");
        let kwargs = PyDict::new(py);
        kwargs
            .set_item("cwd", testing.getattr("REPO_ROOT").unwrap())
            .unwrap();
        kwargs.set_item("timeout_seconds", 0.01).unwrap();
        kwargs.set_item("environment", PyDict::new(py)).unwrap();
        let result = testing
            .getattr("_run_command")
            .unwrap()
            .call((PyList::new(py, ["sleep", "1"]).unwrap(),), Some(&kwargs))
            .unwrap();
        assert!(result
            .getattr("timed_out")
            .unwrap()
            .extract::<bool>()
            .unwrap());
        assert!(result.getattr("returncode").unwrap().is_none());
        equal(
            &testing
                .getattr("blocking_processes")
                .unwrap()
                .call1((PyList::new(py, ["this-string-will-not-match-xyz"]).unwrap(),))
                .unwrap(),
            &PyList::empty(py),
        );
        let kind = testing.getattr("CampaignError").unwrap();
        assert_error(
            py,
            testing
                .getattr("_require_string")
                .unwrap()
                .call1(("", "label"))
                .unwrap_err(),
            &kind,
            "non-empty string",
        );
        assert_error(
            py,
            testing
                .getattr("_require_string_list")
                .unwrap()
                .call1((PyList::new(py, ["", "x"]).unwrap(), "label"))
                .unwrap_err(),
            &kind,
            "list of non-empty",
        );
    });
}
