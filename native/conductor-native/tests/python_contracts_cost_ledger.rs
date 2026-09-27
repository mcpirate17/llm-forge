#![cfg(feature = "python-compat-tests")]
//! Rust-owned contracts for the Python cost-ledger command shim.

#[path = "python_contracts/agent_comm_support.rs"]
#[allow(dead_code)]
mod comm_support;
#[path = "python_contracts/cost_contract_support.rs"]
#[allow(dead_code)]
mod fixture;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use comm_support::{buffer_text, capture};
use fixture::{
    argv, config, equal, forge_script, patch_constant, patch_kwargs_result, write_script,
};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyCFunction, PyDict, PyList};
use serde_json::json;
use std::path::Path;
use support::{module, path, AttrPatch, Case};

fn ledger<'py>(py: Python<'py>) -> Bound<'py, PyAny> {
    module(py, "conductor.cost_ledger").into_any()
}

fn call_main(py: Python<'_>, subject: &Bound<'_, PyAny>, args: &[&str]) -> i32 {
    subject
        .getattr("main")
        .unwrap()
        .call1((PyList::new(py, args).unwrap(),))
        .unwrap()
        .extract()
        .unwrap()
}

fn patch_binary<'py>(py: Python<'py>, subject: &Bound<'py, PyAny>, binary: &Path) -> AttrPatch {
    patch_constant(
        py,
        subject,
        "resolve_forge_binary",
        &path(py, binary),
        &["root"],
    )
}

#[test]
fn test_forwarding_subcommands_pass_argv_verbatim_and_propagate_exit() {
    let mut case = Case::new();
    let (binary, _, argv_file) = forge_script(&mut case);
    case.set_env("FORGE_EXIT", "7");
    Python::attach(|py| {
        let subject = ledger(py);
        let _binary = patch_binary(py, &subject, &binary);
        assert_eq!(
            call_main(
                py,
                &subject,
                &["read", "/a.jsonl", "--json", "--", "--weird"]
            ),
            7
        );
        assert_eq!(
            argv(&argv_file),
            ["ledger", "read", "/a.jsonl", "--json", "--", "--weird"]
        );
    });
}

#[test]
fn test_record_forwards_to_audit_record() {
    let mut case = Case::new();
    let (binary, _, argv_file) = forge_script(&mut case);
    case.set_env("FORGE_EXIT", "0");
    Python::attach(|py| {
        let subject = ledger(py);
        let _binary = patch_binary(py, &subject, &binary);
        assert_eq!(
            call_main(py, &subject, &["record", "--window-days", "3"]),
            0
        );
        assert_eq!(
            argv(&argv_file),
            ["ledger", "audit", "--record", "--window-days", "3"]
        );
    });
}

#[test]
fn test_rollup_without_paths_uses_the_project_defaults() {
    let mut case = Case::new();
    let (binary, _, argv_file) = forge_script(&mut case);
    Python::attach(|py| {
        let subject = ledger(py);
        let _binary = patch_binary(py, &subject, &binary);
        let cfg = config(py, case.root());
        let _config = patch_kwargs_result(py, &subject, "resolve_config", &cfg);
        assert_eq!(call_main(py, &subject, &["rollup"]), 0);
        let expected = [
            "ledger".to_owned(),
            "rollup".to_owned(),
            case.root().join("-home-x-project").display().to_string(),
            "--out".to_owned(),
            case.root().join("ledger").display().to_string(),
            "--repo".to_owned(),
            case.root().join("repo").display().to_string(),
            "--project".to_owned(),
            "-home-x-project".to_owned(),
        ];
        assert_eq!(argv(&argv_file), expected);
    });
}

#[test]
fn test_rollup_with_paths_forwards_verbatim() {
    let mut case = Case::new();
    let (binary, _, argv_file) = forge_script(&mut case);
    Python::attach(|py| {
        let subject = ledger(py);
        let _binary = patch_binary(py, &subject, &binary);
        let never = PyCFunction::new_closure(py, None, None, |args, _kwargs| {
            if !args.is_empty() {
                return Err(pyo3::exceptions::PyTypeError::new_err(
                    "unexpected positional args",
                ));
            }
            Err::<Py<PyAny>, _>(pyo3::exceptions::PyAssertionError::new_err(
                "resolve_config must not run for explicit paths",
            ))
        })
        .unwrap();
        let _config = AttrPatch::replace(&subject, "resolve_config", never.as_any());
        assert_eq!(
            call_main(py, &subject, &["rollup", "/a.jsonl", "--dry-run"]),
            0
        );
        assert_eq!(
            argv(&argv_file),
            ["ledger", "rollup", "/a.jsonl", "--dry-run"]
        );
    });
}

#[test]
fn test_rollup_defaults_refuse_a_missing_transcript_directory() {
    let mut case = Case::new();
    let (binary, _, argv_file) = forge_script(&mut case);
    Python::attach(|py| {
        let subject = ledger(py);
        let _binary = patch_binary(py, &subject, &binary);
        let fields = PyDict::new(py);
        fields
            .set_item("ledger_root", path(py, &case.root().join("ledger")))
            .unwrap();
        fields
            .set_item("repo_path", path(py, &case.root().join("repo")))
            .unwrap();
        fields.set_item("project", "-nowhere").unwrap();
        fields
            .set_item("transcripts_dir", path(py, &case.root().join("nowhere")))
            .unwrap();
        fields
            .set_item(
                "baseline",
                path(
                    py,
                    &case.root().join("repo/ledger/cost_budget_baseline.json"),
                ),
            )
            .unwrap();
        let cfg = subject
            .getattr("LedgerConfig")
            .unwrap()
            .call((), Some(&fields))
            .unwrap();
        let _config = patch_kwargs_result(py, &subject, "resolve_config", &cfg);
        assert_eq!(call_main(py, &subject, &["rollup"]), 2);
        assert!(!argv_file.exists());
    });
}

#[test]
fn test_missing_forge_binary_is_a_loud_exit_2() {
    let _case = Case::new();
    Python::attach(|py| {
        let subject = ledger(py);
        let _binary = patch_constant(
            py,
            &subject,
            "resolve_forge_binary",
            py.None().bind(py),
            &["root"],
        );
        assert_eq!(call_main(py, &subject, &["read", "/a.jsonl"]), 2);
    });
}

fn report_fixture() -> serde_json::Value {
    json!({
        "window":{"from":"2026-09-06","to":"2026-09-13","days":7},
        "metrics":{
            "median_hook_ms":{"value":null,"n":0,"baseline":null,"delta_pct":null,"status":"NO_DATA"},
            "resend_bytes_per_session":{"value":8815583.0,"n":20,"baseline":8815583.0,"delta_pct":0.0,"status":"RATCHET_HELD"}
        },
        "status":"NO_DATA"
    })
}

#[test]
fn test_report_prints_one_line_per_metric_with_status() {
    let mut case = Case::new();
    let (binary, script, _) = forge_script(&mut case);
    let payload = report_fixture();
    write_script(
        &script,
        &format!(
            "#!/bin/sh\n# require --baseline\ncase \" $* \" in *\" --baseline \"*) ;; *) echo \"--baseline required\" >&2; exit 2 ;; esac\nprintf '{}\\n'\n",
            payload
        ),
    );
    Python::attach(|py| {
        let subject = ledger(py);
        let _binary = patch_binary(py, &subject, &binary);
        let cfg = config(py, case.root());
        let _config = patch_kwargs_result(py, &subject, "resolve_config", &cfg);
        let (stdout, _capture) = capture(py, "stdout");
        assert_eq!(call_main(py, &subject, &["report"]), 1);
        let lines = buffer_text(&stdout);
        assert!(lines
            .lines()
            .any(|line| line.contains("median_hook_ms") && line.contains("NO_DATA")));
        assert!(
            lines
                .lines()
                .any(|line| line.contains("resend_bytes_per_session")
                    && line.contains("RATCHET_HELD"))
        );
    });
}

#[test]
fn test_report_with_unparseable_audit_output_is_a_clean_exit_2() {
    let mut case = Case::new();
    let (binary, script, _) = forge_script(&mut case);
    write_script(&script, "#!/bin/sh\necho \"not json\" >&2\nexit 2\n");
    Python::attach(|py| {
        let subject = ledger(py);
        let _binary = patch_binary(py, &subject, &binary);
        let cfg = config(py, case.root());
        let _config = patch_kwargs_result(py, &subject, "resolve_config", &cfg);
        let (stderr, _capture) = capture(py, "stderr");
        assert_eq!(call_main(py, &subject, &["report"]), 2);
        assert!(buffer_text(&stderr).contains("cost_ledger:"));
    });
}

#[test]
fn test_report_propagates_the_hard_empty_window() {
    let mut case = Case::new();
    let (binary, script, _) = forge_script(&mut case);
    write_script(&script, "#!/bin/sh\necho \"empty window\" >&2\nexit 3\n");
    Python::attach(|py| {
        let subject = ledger(py);
        let _binary = patch_binary(py, &subject, &binary);
        let cfg = config(py, case.root());
        let _config = patch_kwargs_result(py, &subject, "resolve_config", &cfg);
        assert_eq!(call_main(py, &subject, &["report"]), 3);
    });
}

#[test]
fn test_munged_project_name_matches_the_harness_layout() {
    let _case = Case::new();
    Python::attach(|py| {
        let subject = ledger(py);
        let repo = path(py, Path::new("/home/tim/Projects/llm-forge"));
        let name = subject
            .getattr("munged_project_name")
            .unwrap()
            .call1((&repo,))
            .unwrap();
        assert!(name.eq("-home-tim-Projects-llm-forge").unwrap());
        let transcripts = subject
            .getattr("transcripts_dir_for")
            .unwrap()
            .call1((&repo,))
            .unwrap();
        let expected = subject
            .getattr("PROJECTS_DIR")
            .unwrap()
            .call_method1("__truediv__", ("-home-tim-Projects-llm-forge",))
            .unwrap();
        equal(&transcripts, &expected);
    });
}

#[test]
fn test_resolve_config_prefers_flags_over_env_over_default() {
    let mut case = Case::new();
    let repo = case.mkdir("repo");
    case.set_env(
        "LEDGER_ROOT",
        case.root().join("env-ledger").to_str().unwrap(),
    );
    Python::attach(|py| {
        let subject = ledger(py);
        let fields = PyDict::new(py);
        fields.set_item("repo_root", path(py, &repo)).unwrap();
        let default = subject
            .getattr("resolve_config")
            .unwrap()
            .call((), Some(&fields))
            .unwrap();
        equal(
            &default.getattr("ledger_root").unwrap(),
            &path(py, &case.root().join("env-ledger")),
        );
        let munged = subject
            .getattr("munged_project_name")
            .unwrap()
            .call1((path(py, &repo),))
            .unwrap();
        equal(&default.getattr("project").unwrap(), &munged);
        fields
            .set_item("ledger_root", path(py, &case.root().join("flag-ledger")))
            .unwrap();
        fields
            .set_item("transcripts_dir", path(py, &case.root().join("elsewhere")))
            .unwrap();
        let flagged = subject
            .getattr("resolve_config")
            .unwrap()
            .call((), Some(&fields))
            .unwrap();
        equal(
            &flagged.getattr("ledger_root").unwrap(),
            &path(py, &case.root().join("flag-ledger")),
        );
        assert!(flagged.getattr("project").unwrap().eq("elsewhere").unwrap());
    });
}

#[test]
fn test_has_positional_path_treats_flags_and_separators_correctly() {
    let _case = Case::new();
    Python::attach(|py| {
        let subject = ledger(py);
        let has = subject.getattr("_has_positional_path").unwrap();
        let cases: &[(&[&str], bool)] = &[
            (&["/a.jsonl"], true),
            (&["--out", "/x", "/a.jsonl"], true),
            (&["--out=/x", "/a.jsonl"], true),
            (&["--dry-run", "--", "/a.jsonl"], true),
            (&["--dry-run"], false),
            (&["--out", "/x", "--dry-run"], false),
            (&["--branch", "master"], false),
            (&["--branch=master"], false),
            (&["--branch", "master", "--out", "/x", "--dry-run"], false),
            (&["--branch", "master", "/a.jsonl"], true),
            (&[], false),
        ];
        for (args, expected) in cases {
            let actual: bool = has
                .call1((PyList::new(py, *args).unwrap(),))
                .unwrap()
                .extract()
                .unwrap();
            assert_eq!(actual, *expected, "{args:?}");
        }
    });
}
