#![cfg(feature = "python-compat-tests")]
//! End-to-end Python refresh CLI contracts; no mutation engine is run.

#[path = "python_contracts/mutation_campaign_generate_support.rs"]
#[allow(dead_code)]
mod generate_support;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use generate_support::{
    first, generator, load_json, manifest_file, plan_json, tree, write, write_json,
};
use pyo3::prelude::*;
use pyo3::types::PyDict;
use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};
use support::{assert_error, module, path, AttrPatch, Case};

fn recorded_python_campaign(py: Python<'_>, root: &Path) -> PathBuf {
    tree(
        root,
        &[
            ("conductor/subject.py", "x = 1\n"),
            (
                "conductor/test_subject.py",
                "def test_subject(): assert 1 == 1\n",
            ),
            (
                "conductor/test_feedback.py",
                "def test_feedback(): assert 1 == 1\n",
            ),
        ],
    );
    let item = first(&plan_json(
        py,
        "python",
        root,
        &json!({
            "day": "20261004",
            "only_sources": ["conductor/subject.py"],
            "extra_tests": {"conductor/subject.py": ["conductor/test_feedback.py"]}
        }),
    ));
    let file = manifest_file(root, &write(py, root, &[item], false).unwrap());
    let mut recorded = load_json(&file);
    recorded["survivor_baseline"] = json!(["engine-recorded"]);
    recorded["survivor_baseline_recorded"] = json!(true);
    recorded["survivor_baseline_note"] = json!("engine note");
    recorded["survivor_baseline_recorded_at"] = json!("2026-10-03T00:00:00Z");
    recorded["generator"]["run_timeout_seconds"] = json!(91);
    write_json(&file, &recorded);
    file
}

fn cli_refresh(py: Python<'_>, root: &Path, file: &Path, extra: &[&str]) -> PyResult<String> {
    let generate = generator(py);
    let _root = AttrPatch::replace(&generate, "REPO_ROOT", path(py, root).as_any());
    let mut arguments = vec!["refresh", file.to_str().unwrap()];
    arguments.extend_from_slice(extra);
    let args = generate
        .getattr("_cli_parser")?
        .call0()?
        .call_method1("parse_args", (arguments,))?;
    generate
        .getattr("_refresh_command")?
        .call1((args,))?
        .extract()
}

#[test]
fn cli_refresh_preserves_ratchet_unions_tests_and_updates_only_requested_timeout() {
    let case = Case::new();
    Python::attach(|py| {
        let file = recorded_python_campaign(py, case.root());
        let recorded = load_json(&file);
        tree(
            case.root(),
            &[
                ("conductor/subject.py", "x = 2\n"),
                (
                    "conductor/test_selection.py",
                    "def test_selection(): assert 2 == 2\n",
                ),
                (".claude/hooks/dispatch.py", "def dispatch(): pass\n"),
                ("first/test_dispatch.py", "def test_dispatch(): pass\n"),
                ("second/test_dispatch.py", "def test_dispatch(): pass\n"),
            ],
        );
        let result = cli_refresh(
            py,
            case.root(),
            &file,
            &[
                "--extra-test",
                "conductor/subject.py=conductor/test_selection.py",
                "--extra-test",
                "conductor/subject.py=conductor/test_feedback.py",
                "--run-timeout",
                "300",
            ],
        )
        .unwrap();
        assert_eq!(
            result,
            file.strip_prefix(case.root()).unwrap().to_str().unwrap()
        );
        let refreshed = load_json(&file);
        assert_eq!(
            refreshed["generator"]["source"],
            json!(["conductor/subject.py"])
        );
        assert_eq!(refreshed["generator"]["run_timeout_seconds"], 300);
        assert_eq!(
            refreshed["test_sha256"]
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect::<Vec<_>>(),
            [
                "conductor/test_feedback.py",
                "conductor/test_selection.py",
                "conductor/test_subject.py"
            ]
        );
        for field in [
            "survivor_baseline",
            "survivor_baseline_recorded",
            "survivor_baseline_note",
            "survivor_baseline_recorded_at",
        ] {
            assert_eq!(refreshed[field], recorded[field]);
        }
        cli_refresh(py, case.root(), &file, &[]).unwrap();
        assert_eq!(load_json(&file), refreshed);
    });
}

#[test]
fn cli_refresh_rejects_invalid_bindings_without_writing_the_recorded_manifest() {
    let case = Case::new();
    Python::attach(|py| {
        let file = recorded_python_campaign(py, case.root());
        tree(case.root(), &[("conductor/other.py", "x = 1\n")]);
        let original = fs::read(&file).unwrap();
        let error_type = module(py, "conductor.mutation_scope")
            .getattr("CampaignError")
            .unwrap();
        for arguments in [
            vec![
                "--extra-test",
                "conductor/other.py=conductor/test_feedback.py",
            ],
            vec![
                "--extra-test",
                "conductor/subject.py=conductor/test_missing.py",
            ],
            vec!["--run-timeout", "0"],
        ] {
            let error = cli_refresh(py, case.root(), &file, &arguments).unwrap_err();
            assert!(error.is_instance(py, &error_type), "{error}");
            assert_eq!(fs::read(&file).unwrap(), original);
        }
        fs::remove_file(case.root().join("conductor/test_feedback.py")).unwrap();
        assert_error(
            py,
            cli_refresh(py, case.root(), &file, &[]).unwrap_err(),
            &error_type,
            "existing repository-relative Python test file",
        );
        assert_eq!(fs::read(&file).unwrap(), original);
    });
}

#[test]
fn direct_python_refresh_rejects_nonpositive_phase_timeout_without_erasing_ratchet() {
    let case = Case::new();
    Python::attach(|py| {
        let file = recorded_python_campaign(py, case.root());
        let original = fs::read(&file).unwrap();
        let kwargs = PyDict::new(py);
        kwargs.set_item("repo_root", path(py, case.root())).unwrap();
        kwargs.set_item("run_timeout_seconds", -1).unwrap();
        let error = generator(py)
            .getattr("refresh_python_campaign")
            .unwrap()
            .call((file.to_str().unwrap(),), Some(&kwargs))
            .unwrap_err();
        assert_error(
            py,
            error,
            &module(py, "conductor.mutation_scope")
                .getattr("CampaignError")
                .unwrap(),
            "timeout must be positive",
        );
        assert_eq!(fs::read(&file).unwrap(), original);
    });
}

fn scoped_cli_plan(
    py: Python<'_>,
    root: &Path,
    language: &str,
    width: Option<&str>,
) -> PyResult<serde_json::Value> {
    let generate = generator(py);
    let _root = AttrPatch::replace(&generate, "REPO_ROOT", path(py, root).as_any());
    let mut defaults = Vec::new();
    for name in ["plan", "_explicit_scope", "_extra_tests"] {
        let function = generate.getattr(name)?;
        let copied = function.getattr("__kwdefaults__")?.call_method0("copy")?;
        copied
            .cast::<PyDict>()?
            .set_item("repo_root", path(py, root))?;
        defaults.push(AttrPatch::replace(&function, "__kwdefaults__", &copied));
    }
    let mut arguments = vec![
        "plan",
        language,
        "--owner",
        "fixture",
        "--only",
        "conductor/subject.py",
    ];
    if let Some(width) = width {
        arguments.extend(["--python-jobs", width]);
    }
    let args = generate
        .getattr("_cli_parser")?
        .call0()?
        .call_method1("parse_args", (arguments,))?;
    let result = generate.getattr("_run_plan_or_write")?.call1((args,))?;
    Ok(generate_support::py_to_json(&result))
}

#[test]
fn python_worker_width_flows_through_cli_plan_write_loader_and_fest_config() {
    for (setting, expected) in [(None, 1), (Some("1"), 1), (Some("2"), 2)] {
        let case = Case::new();
        Python::attach(|py| {
            tree(
                case.root(),
                &[
                    ("conductor/subject.py", "x = 1\n"),
                    ("conductor/test_subject.py", "def test_subject(): pass\n"),
                ],
            );
            let planned = scoped_cli_plan(py, case.root(), "python", setting).unwrap();
            let item = first(&planned);
            assert_eq!(
                item["generator"]
                    .get("jobs")
                    .and_then(serde_json::Value::as_i64),
                setting.map(|value| value.parse::<i64>().unwrap())
            );
            let file = manifest_file(
                case.root(),
                &write(py, case.root(), &[item], false).unwrap(),
            );
            let core = module(py, "conductor.mutation_engine_generated");
            let campaign = core
                .getattr("GeneratedCampaign")
                .unwrap()
                .call1((
                    path(py, &file),
                    generate_support::json_to_py(py, &load_json(&file)),
                ))
                .unwrap();
            assert_eq!(
                campaign.getattr("jobs").unwrap().extract::<i64>().unwrap(),
                expected
            );
            core.getattr("resolve_mutant_timeout")
                .unwrap()
                .call1((&campaign, 0.01))
                .unwrap();
            let config: String = module(py, "conductor.mutation_engine_fest")
                .getattr("_config")
                .unwrap()
                .call1((&campaign, "python"))
                .unwrap()
                .extract()
                .unwrap();
            assert!(
                config
                    .lines()
                    .any(|line| line == format!("workers = {expected}")),
                "{config}"
            );
        });
    }
}

#[test]
fn python_worker_refresh_cli_preserves_width_tests_and_baseline_unless_overridden() {
    let case = Case::new();
    Python::attach(|py| {
        let file = recorded_python_campaign(py, case.root());
        let original = load_json(&file);
        cli_refresh(py, case.root(), &file, &["--python-jobs", "2"]).unwrap();
        let two = load_json(&file);
        assert_eq!(two["generator"]["jobs"], 2);
        cli_refresh(py, case.root(), &file, &[]).unwrap();
        assert_eq!(load_json(&file), two);
        cli_refresh(py, case.root(), &file, &["--python-jobs", "3"]).unwrap();
        let three = load_json(&file);
        assert_eq!(three["generator"]["jobs"], 3);
        assert_eq!(three["test_sha256"], original["test_sha256"]);
        for field in [
            "survivor_baseline",
            "survivor_baseline_recorded",
            "survivor_baseline_note",
            "survivor_baseline_recorded_at",
        ] {
            assert_eq!(three[field], original[field]);
        }
    });
}

#[test]
fn python_worker_cli_rejects_nonpositive_widths_and_rust_requests_without_writing() {
    let case = Case::new();
    Python::attach(|py| {
        let file = recorded_python_campaign(py, case.root());
        let original = fs::read(&file).unwrap();
        let error_type = module(py, "conductor.mutation_scope")
            .getattr("CampaignError")
            .unwrap();
        for width in ["0", "-1"] {
            assert_error(
                py,
                scoped_cli_plan(py, case.root(), "python", Some(width)).unwrap_err(),
                &error_type,
                "worker count must be positive",
            );
            assert_error(
                py,
                cli_refresh(py, case.root(), &file, &["--python-jobs", width]).unwrap_err(),
                &error_type,
                "worker count must be positive",
            );
            assert_eq!(fs::read(&file).unwrap(), original);
        }
        assert_error(
            py,
            scoped_cli_plan(py, case.root(), "rust", Some("2")).unwrap_err(),
            &error_type,
            "Python-only",
        );
    });
}
