#![cfg(feature = "python-compat-tests")]
//! Rust-owned contracts for `conductor.equivalence_gate`: the host table it reads, the
//! modules it hands the native gate, and its `gate` and `backlog` commands.

#[path = "python_contracts/git_fixture_support.rs"]
#[allow(dead_code)]
mod git_fixture_support;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use git_fixture_support::{git, init_protected_repo};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyString};
use serde_json::{json, Value};
use std::fs;
use std::path::Path;
use support::{module, path, text, AttrPatch, Case};

fn gate(py: Python<'_>) -> Bound<'_, PyModule> {
    module(py, "conductor.equivalence_gate")
}

fn host<'py>(py: Python<'py>, root: &Path) -> PyResult<Bound<'py, PyAny>> {
    gate(py).getattr("host")?.call1((path(py, root),))
}

fn table(case: &Case, body: &str) {
    case.write(
        "pyproject.toml",
        &format!("[tool.forge.equivalence_probe]\n{body}"),
    );
}

fn main(py: Python<'_>, argv: &[&str]) -> PyResult<i64> {
    gate(py).getattr("main")?.call1((argv.to_vec(),))?.extract()
}

fn blocking(module_name: &str) -> Value {
    json!({"module": module_name, "qualname": "Lane.forward", "rule": "drop_where",
        "lineno": 2, "verdict": "REACHABLE_BUT_UNTESTED", "description": "torch.where(...) collapsed",
        "amplifier": "params_x1e3", "max_diff_amplified": 0.97, "drivers_source": "index"})
}

fn summary(blocking: Vec<Value>) -> String {
    json!({"modules_probed": 1, "measured": [], "modules_without_drivers": [],
        "modules_index_driven": ["lane.py"], "blocking": blocking, "advisory": [],
        "untested": [], "unreached": [], "incomplete": [], "unmeasured": [], "live": 0})
    .to_string()
}

/// `conductor.equivalence_gate.probe` answering `summary`, for as long as it is held.
fn fake_probe<'py>(py: Python<'py>, summary: &str) -> (Bound<'py, PyAny>, AttrPatch) {
    let kwargs = PyDict::new(py);
    kwargs
        .set_item("return_value", PyString::new(py, summary))
        .unwrap();
    let fake = module(py, "unittest.mock")
        .getattr("Mock")
        .unwrap()
        .call((), Some(&kwargs))
        .unwrap();
    let patch = AttrPatch::replace(gate(py).as_any(), "probe", &fake);
    (fake, patch)
}

#[test]
fn a_host_without_the_table_gets_no_waivers_artifacts_or_store() {
    let case = Case::new();
    Python::attach(|py| {
        let found = host(py, case.root()).unwrap();
        let root = case.root().canonicalize().unwrap();
        assert_eq!(
            text(&found.getattr("root").unwrap()),
            root.to_str().unwrap()
        );
        assert!(found.getattr("store").unwrap().is_none());
        assert!(found.getattr("jobs").unwrap().is_none());
        assert_eq!(
            found.getattr("timeout").unwrap().extract::<f64>().unwrap(),
            180.0
        );
        for key in ["waivers", "run_artifacts", "ledger"] {
            assert!(
                found.call_method1("path", (key,)).unwrap().is_none(),
                "{key}"
            );
        }
    });
}

#[test]
fn an_unknown_or_mistyped_key_is_refused_by_name() {
    for body in [
        "waiver = \"w.json\"\n",
        "jobs = true\n",
        "jobs = 2.5\n",
        "per_module_timeout = \"30\"\n",
        "run_artifacts = 3\n",
    ] {
        let case = Case::new();
        table(&case, body);
        Python::attach(|py| {
            let error = host(py, case.root()).unwrap_err();
            assert!(
                error.is_instance_of::<pyo3::exceptions::PyValueError>(py),
                "{body}"
            );
            let message = error.to_string();
            let key = body.split(' ').next().unwrap();
            assert!(
                message.contains(key) && message.contains("is not a known key"),
                "{message}"
            );
        });
    }
}

#[test]
fn table_paths_resolve_against_the_host_root_or_the_given_base() {
    let case = Case::new();
    table(
        &case,
        "waivers = \"conf/waivers.json\"\nper_module_timeout = 30\njobs = 3\n",
    );
    Python::attach(|py| {
        let found = host(py, case.root()).unwrap();
        let root = case.root().canonicalize().unwrap();
        assert_eq!(
            text(&found.call_method1("path", ("waivers",)).unwrap()),
            root.join("conf/waivers.json").to_str().unwrap()
        );
        let copy = case.root().join("copy");
        assert_eq!(
            text(
                &found
                    .call_method1("path", ("waivers", path(py, &copy)))
                    .unwrap()
            ),
            copy.join("conf/waivers.json").to_str().unwrap()
        );
        assert_eq!(
            found.getattr("timeout").unwrap().extract::<f64>().unwrap(),
            30.0
        );
        assert_eq!(found.getattr("jobs").unwrap().extract::<i64>().unwrap(), 3);
    });
}

#[test]
fn the_gate_probes_named_changed_or_all_source_modules_never_tests() {
    let case = Case::new();
    let repo = case.root().join("repo");
    init_protected_repo(&repo);
    for name in [
        "pkg/a.py",
        "pkg/b.py",
        "pkg/test_a.py",
        "pkg/conftest.py",
        "tests/helper.py",
        "notes.md",
    ] {
        git_fixture_support::write(&repo, name, "VALUE = 1\n");
    }
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "base"]);
    for name in ["pkg/b.py", "pkg/test_a.py", "tests/helper.py"] {
        git_fixture_support::write(&repo, name, "VALUE = 2\n");
    }
    Python::attach(|py| {
        let seen = |argv: &[&str]| -> Vec<(String, Option<Vec<usize>>)> {
            let (fake, _patch) = fake_probe(py, &summary(vec![]));
            let mut full = vec!["gate", "--root", repo.to_str().unwrap()];
            full.extend_from_slice(argv);
            assert_eq!(main(py, &full).unwrap(), 0);
            fake.getattr("call_args")
                .unwrap()
                .getattr("args")
                .unwrap()
                .get_item(2)
                .unwrap()
                .extract()
                .unwrap()
        };
        let whole = |names: &[&str]| -> Vec<(String, Option<Vec<usize>>)> {
            names.iter().map(|name| (name.to_string(), None)).collect()
        };
        assert_eq!(seen(&["--all"]), whole(&["pkg/a.py", "pkg/b.py"]));
        assert_eq!(seen(&["--base", "HEAD"]), whole(&["pkg/b.py"]));
        assert_eq!(
            seen(&["--module", "pkg/test_a.py"]),
            whole(&["pkg/test_a.py"])
        );
    });
}

#[test]
fn the_gate_writes_its_summary_and_fails_only_when_enforced_on_a_blocker() {
    let case = Case::new();
    let out = case.root().join("out/summary.json");
    let root = case.root().to_str().unwrap();
    Python::attach(|py| {
        let text = summary(vec![blocking("lane.py")]);
        let (_fake, _patch) = fake_probe(py, &text);
        let argv = [
            "gate",
            "--root",
            root,
            "--module",
            "lane.py",
            "--json",
            out.to_str().unwrap(),
        ];
        assert_eq!(main(py, &argv).unwrap(), 0);
        let written: Value = serde_json::from_str(&fs::read_to_string(&out).unwrap()).unwrap();
        assert_eq!(written["blocking"], json!([blocking("lane.py")]));
        let mut enforced = argv.to_vec();
        enforced.push("--enforce");
        assert_eq!(main(py, &enforced).unwrap(), 1);
        let (_clean, _patch) = fake_probe(py, &summary(vec![]));
        assert_eq!(main(py, &enforced).unwrap(), 0);
    });
}

#[test]
fn backlog_folds_recorded_runs_and_removes_them_once_folded() {
    let case = Case::new();
    table(
        &case,
        "ledger = \"state/ledger.json\"\nreport = \"state/backlog.md\"\nrun_artifacts = \"runs\"\n",
    );
    let run = case.write(
        "runs/gate-20261024T000000000000.json",
        &summary(vec![blocking("lane.py")]),
    );
    let root = case.root().to_str().unwrap();
    Python::attach(|py| {
        assert_eq!(
            main(py, &["backlog", "--root", root, "--dry-run"]).unwrap(),
            0
        );
        assert!(run.is_file(), "a dry run consumed the run artifact");
        assert!(!case.root().join("state/ledger.json").exists());
        assert_eq!(main(py, &["backlog", "--root", root]).unwrap(), 0);
        assert!(
            !run.exists(),
            "a folded run artifact was left to be folded again"
        );
        let ledger = fs::read_to_string(case.root().join("state/ledger.json")).unwrap();
        assert!(ledger.contains("lane.py"), "{ledger}");
        let report = fs::read_to_string(case.root().join("state/backlog.md")).unwrap();
        assert!(report.contains("Lane.forward"), "{report}");
    });
}

#[test]
fn backlog_without_a_ledger_says_where_to_configure_one() {
    let case = Case::new();
    let root = case.root().to_str().unwrap();
    Python::attach(|py| {
        let error = main(py, &["backlog", "--root", root]).unwrap_err();
        assert!(error.is_instance_of::<pyo3::exceptions::PySystemExit>(py));
        assert!(error
            .value(py)
            .to_string()
            .contains("[tool.forge.equivalence_probe]"));
    });
}
