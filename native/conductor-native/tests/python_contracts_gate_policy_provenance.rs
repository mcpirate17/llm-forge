#![cfg(feature = "python-compat-tests")]
//! Rust-owned contracts for policy provenance in the exported gate candidate.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::prelude::*;
use pyo3::types::{PyCFunction, PyDict, PyTuple};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use support::{assert_error, module, path, text, AttrPatch, Case};

fn isolated_case() -> Case {
    let mut case = Case::new();
    for name in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_NAMESPACE",
        "GIT_CEILING_DIRECTORIES",
        "GIT_CONFIG_COUNT",
        "GIT_CONFIG_PARAMETERS",
        "GIT_CONFIG_KEY_0",
        "GIT_CONFIG_VALUE_0",
        "GIT_CONFIG",
        "GIT_TEMPLATE_DIR",
    ] {
        case.remove_env(name);
    }
    case.set_env("GIT_CONFIG_NOSYSTEM", "1");
    case.set_env("GIT_CONFIG_GLOBAL", "/dev/null");
    case.set_env("GIT_CONFIG_SYSTEM", "/dev/null");
    case
}

fn git(repo: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
        .env_remove("GIT_CONFIG")
        .env_remove("GIT_TEMPLATE_DIR")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn fixture(case: &Case) -> PathBuf {
    let repo = case.mkdir("repo");
    git(&repo, &["init", "--quiet", "-b", "main"]);
    git(&repo, &["config", "user.email", "test@example.invalid"]);
    git(&repo, &["config", "user.name", "test"]);
    git(&repo, &["config", "commit.gpgsign", "false"]);
    git(&repo, &["config", "core.hooksPath", "/dev/null"]);
    fs::write(repo.join("tracked.py"), "VALUE = 1\n").unwrap();
    git(&repo, &["add", "tracked.py"]);
    git(&repo, &["commit", "--quiet", "-m", "first"]);
    repo
}

fn phase<'py>(py: Python<'py>, name: &str) -> PyResult<Bound<'py, PyAny>> {
    let gate = module(py, "conductor.gate");
    let kwargs = PyDict::new(py);
    kwargs.set_item("name", name)?;
    kwargs.set_item("ok", true)?;
    kwargs.set_item("detail", "stubbed for this module")?;
    gate.getattr("PhaseResult")?.call((), Some(&kwargs))
}

fn stub_policy<'py>(py: Python<'py>, tools: Option<&Bound<'py, PyAny>>) -> Bound<'py, PyAny> {
    let kwargs = PyDict::new(py);
    match tools {
        Some(tools) => kwargs.set_item("tools", tools).unwrap(),
        None => kwargs.set_item("tools", ()).unwrap(),
    }
    kwargs.set_item("mutation_waivers", ()).unwrap();
    module(py, "types")
        .getattr("SimpleNamespace")
        .unwrap()
        .call((), Some(&kwargs))
        .unwrap()
}

fn inert_gate<'py>(py: Python<'py>) -> (AttrPatch, AttrPatch) {
    let gate = module(py, "conductor.gate");
    let corpus = PyCFunction::new_closure(py, None, None, |args, kwargs| -> PyResult<Py<PyAny>> {
        assert_eq!(args.len(), 1);
        assert!(kwargs.is_none_or(|k| k.len() <= 1));
        Ok(phase(args.py(), "mutation-corpus")?.unbind())
    })
    .unwrap();
    let cost = PyCFunction::new_closure(py, None, None, |args, kwargs| -> PyResult<Py<PyAny>> {
        assert_eq!(args.len(), 1);
        assert!(kwargs.is_none_or(|k| k.is_empty()));
        Ok(phase(args.py(), "cost-budget-audit")?.unbind())
    })
    .unwrap();
    (
        AttrPatch::replace(gate.as_any(), "mutation_corpus_audit", corpus.as_any()),
        AttrPatch::replace(gate.as_any(), "cost_budget_audit", cost.as_any()),
    )
}

fn run_gate<'py>(
    py: Python<'py>,
    repo: &Path,
    output: &Path,
    skip_review: bool,
) -> PyResult<Bound<'py, PyAny>> {
    let kwargs = PyDict::new(py);
    kwargs.set_item("target_ref", "HEAD")?;
    kwargs.set_item("base_ref", "HEAD")?;
    kwargs.set_item("profile", "full")?;
    kwargs.set_item("python", "python3")?;
    kwargs.set_item(
        "policy_path",
        path(py, Path::new("conductor/candidate_policy.toml")),
    )?;
    kwargs.set_item("json_out", path(py, output))?;
    kwargs.set_item("skip_review", skip_review)?;
    module(py, "conductor.gate")
        .getattr("run_gate")?
        .call((path(py, repo),), Some(&kwargs))
}

fn policy_loader<'py>(py: Python<'py>, seen: Arc<Mutex<Vec<String>>>) -> Bound<'py, PyCFunction> {
    PyCFunction::new_closure(py, None, None, move |args, kwargs| -> PyResult<Py<PyAny>> {
        assert_eq!(args.len(), 1);
        assert!(kwargs.is_none_or(|k| k.is_empty()));
        let loaded = text(&args.get_item(0)?);
        seen.lock().unwrap().push(loaded);
        Ok(stub_policy(args.py(), None).unbind())
    })
    .unwrap()
}

#[test]
fn run_gate_loads_the_policy_from_the_export_not_the_working_tree() {
    let case = isolated_case();
    let repo = fixture(&case);
    Python::attach(|py| {
        let _inert = inert_gate(py);
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let loader = policy_loader(py, Arc::clone(&seen));
        let gate = module(py, "conductor.gate");
        let _load = AttrPatch::replace(gate.as_any(), "load_policy", loader.as_any());
        run_gate(py, &repo, &case.root().join("out.json"), true).unwrap();
        let seen = seen.lock().unwrap();
        assert!(!seen.is_empty(), "run_gate never loaded a policy");
        let loaded = Path::new(&seen[0]);
        assert!(
            !loaded.starts_with(&repo),
            "policy was read from the working tree: {}",
            loaded.display()
        );
        assert_eq!(loaded.file_name().unwrap(), "candidate_policy.toml");
        assert!(
            seen[0].contains("gate-export-"),
            "not read from export: {}",
            seen[0]
        );
    });
}

#[test]
fn run_gate_exports_before_it_loads_the_policy() {
    let case = isolated_case();
    let repo = fixture(&case);
    Python::attach(|py| {
        let _inert = inert_gate(py);
        let gate = module(py, "conductor.gate");
        let real_export = gate.getattr("export_tree").unwrap().unbind();
        let order = Arc::new(Mutex::new(Vec::<String>::new()));
        let export_order = Arc::clone(&order);
        let export =
            PyCFunction::new_closure(py, None, None, move |args, kwargs| -> PyResult<Py<PyAny>> {
                assert_eq!(args.len(), 3);
                assert!(kwargs.is_none_or(|k| k.is_empty()));
                export_order.lock().unwrap().push("export".to_owned());
                Ok(real_export.bind(args.py()).call(args, None)?.unbind())
            })
            .unwrap();
        let load_order = Arc::clone(&order);
        let load =
            PyCFunction::new_closure(py, None, None, move |args, kwargs| -> PyResult<Py<PyAny>> {
                assert_eq!(args.len(), 1);
                assert!(kwargs.is_none_or(|k| k.is_empty()));
                load_order.lock().unwrap().push("load_policy".to_owned());
                Ok(stub_policy(args.py(), None).unbind())
            })
            .unwrap();
        let _export = AttrPatch::replace(gate.as_any(), "export_tree", export.as_any());
        let _load = AttrPatch::replace(gate.as_any(), "load_policy", load.as_any());
        run_gate(py, &repo, &case.root().join("out.json"), true).unwrap();
        assert_eq!(&order.lock().unwrap()[..2], ["export", "load_policy"]);
    });
}

#[test]
fn run_gate_still_refuses_when_the_candidate_policy_is_bad() {
    let case = isolated_case();
    let repo = fixture(&case);
    Python::attach(|py| {
        let _inert = inert_gate(py);
        let policy_error = module(py, "conductor.candidate_review.policy")
            .getattr("PolicyError")
            .unwrap()
            .unbind();
        let boom = PyCFunction::new_closure(py, None, None, move |args, kwargs| -> PyResult<()> {
            assert_eq!(args.len(), 1);
            assert!(kwargs.is_none_or(|k| k.is_empty()));
            let error = policy_error
                .bind(args.py())
                .call1(("waiver pins an unexpected integration base",))?;
            Err(PyErr::from_value(error))
        })
        .unwrap();
        let gate = module(py, "conductor.gate");
        let _load = AttrPatch::replace(gate.as_any(), "load_policy", boom.as_any());
        let error = run_gate(py, &repo, &case.root().join("out.json"), true).unwrap_err();
        assert_error(
            py,
            error,
            &gate.getattr("GateRefusal").unwrap(),
            "policy did not load",
        );
    });
}

#[test]
fn run_gate_refuses_rather_than_fails_when_a_declared_tool_is_missing() {
    let case = isolated_case();
    let repo = fixture(&case);
    Python::attach(|py| {
        let _inert = inert_gate(py);
        let gate = module(py, "conductor.gate");
        let kwargs = PyDict::new(py);
        kwargs.set_item("tool_id", "definitely-absent").unwrap();
        kwargs
            .set_item("executable", "definitely-absent-binary")
            .unwrap();
        kwargs
            .set_item("version_command", ("definitely-absent-binary", "--version"))
            .unwrap();
        kwargs.set_item("expected_version", "1.0.0").unwrap();
        kwargs
            .set_item("required_profiles", ("fast", "full"))
            .unwrap();
        kwargs.set_item("provided_by", "test fixture").unwrap();
        kwargs.set_item("rationale", "test fixture").unwrap();
        let missing = module(py, "conductor.candidate_review.policy")
            .getattr("ToolPolicy")
            .unwrap()
            .call((), Some(&kwargs))
            .unwrap()
            .unbind();
        let load =
            PyCFunction::new_closure(py, None, None, move |args, kwargs| -> PyResult<Py<PyAny>> {
                assert_eq!(args.len(), 1);
                assert!(kwargs.is_none_or(|k| k.is_empty()));
                let tools = PyTuple::new(args.py(), [missing.bind(args.py())])?;
                Ok(stub_policy(args.py(), Some(tools.as_any())).unbind())
            })
            .unwrap();
        let _load = AttrPatch::replace(gate.as_any(), "load_policy", load.as_any());
        let result = run_gate(py, &repo, &case.root().join("out.json"), true).unwrap();
        let (code, phases, statuses): (i32, Vec<Py<PyAny>>, Vec<Py<PyAny>>) =
            result.extract().unwrap();
        assert_eq!(
            code,
            gate.getattr("EXIT_REFUSED")
                .unwrap()
                .extract::<i32>()
                .unwrap()
        );
        assert_ne!(
            code,
            gate.getattr("EXIT_FAIL").unwrap().extract::<i32>().unwrap()
        );
        let names: Vec<_> = phases
            .iter()
            .map(|p| text(&p.bind(py).getattr("name").unwrap()))
            .collect();
        assert_eq!(names, ["export", "tool-preflight"]);
        let ids: Vec<_> = statuses
            .iter()
            .map(|s| text(&s.bind(py).getattr("tool_id").unwrap()))
            .collect();
        assert_eq!(ids, ["definitely-absent"]);
    });
}

#[test]
fn run_gate_runs_the_review_unless_it_is_skipped() {
    let case = isolated_case();
    let repo = fixture(&case);
    Python::attach(|py| {
        let _inert = inert_gate(py);
        let gate = module(py, "conductor.gate");
        let load = policy_loader(py, Arc::new(Mutex::new(Vec::new())));
        let _load = AttrPatch::replace(gate.as_any(), "load_policy", load.as_any());
        let calls = Arc::new(Mutex::new(Vec::<String>::new()));
        let recorded = Arc::clone(&calls);
        let review =
            PyCFunction::new_closure(py, None, None, move |args, kwargs| -> PyResult<Py<PyAny>> {
                assert_eq!(args.len(), 1);
                let kwargs = kwargs.expect("review kwargs");
                recorded
                    .lock()
                    .unwrap()
                    .push(text(&kwargs.get_item("target_ref")?.unwrap()));
                let phase = phase(args.py(), "review")?;
                Ok((phase, PyDict::new(args.py()))
                    .into_pyobject(args.py())?
                    .into_any()
                    .unbind())
            })
            .unwrap();
        let _review = AttrPatch::replace(gate.as_any(), "run_review", review.as_any());
        let result = run_gate(py, &repo, &case.root().join("out.json"), false).unwrap();
        let phases = result.get_item(1).unwrap();
        assert_eq!(&*calls.lock().unwrap(), &["HEAD"]);
        let names: Vec<String> = phases
            .try_iter()
            .unwrap()
            .map(|p| text(&p.unwrap().getattr("name").unwrap()))
            .collect();
        assert!(names.iter().any(|n| n == "review"));
    });
}
