#![cfg(feature = "python-compat-tests")]
//! Rust-owned contracts for the equivalence probe's per-function sweep budget.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::prelude::*;
use pyo3::types::{PyAny, PyCFunction, PyDict};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use support::{module, path, text, AttrPatch, Case, CwdRestore};

const MODULE: &str = r#"
def clipped(value, ceiling=8):
    """`min` here is load-bearing; the guard below it never binds in the tests."""
    value = min(value, ceiling)
    if value < -1000:
        value = -1000
    return value * 2
"#;

const TESTS: &str = r#"
from fixture_mod import clipped


def test_clipped():
    assert clipped(3) == 6
    assert clipped(50) == 16
"#;

fn workspace(case: &Case) -> CwdRestore {
    case.write("pytest.ini", "[pytest]\n");
    case.write("fixture_mod.py", MODULE);
    case.write("test_fixture_mod.py", TESTS);
    let cwd = case.chdir("");
    Python::attach(|py| {
        let sys = module(py, "sys");
        sys.getattr("path")
            .unwrap()
            .call_method1("insert", (0, case.root().to_str().unwrap()))
            .unwrap();
        module(py, "importlib")
            .call_method0("invalidate_caches")
            .unwrap();
        for name in ["fixture_mod", "test_fixture_mod"] {
            sys.getattr("modules")
                .unwrap()
                .call_method1("pop", (name, py.None()))
                .unwrap();
        }
    });
    cwd
}

fn probe<'py>(py: Python<'py>) -> Bound<'py, PyAny> {
    module(py, "conductor.equivalence_probe").into_any()
}

fn run_probe<'py>(py: Python<'py>, case: &Case, budget: Option<f64>) -> Vec<Py<PyAny>> {
    let kwargs = PyDict::new(py);
    kwargs.set_item("module_name", "fixture_mod").unwrap();
    kwargs.set_item("budget_seconds", budget).unwrap();
    probe(py)
        .getattr("probe_function")
        .unwrap()
        .call(
            (
                path(py, &case.root().join("fixture_mod.py")),
                "clipped",
                ["test_fixture_mod.py"],
            ),
            Some(&kwargs),
        )
        .unwrap()
        .extract()
        .unwrap()
}

fn clock<'py>(py: Python<'py>, reads: Arc<AtomicUsize>) -> Bound<'py, PyCFunction> {
    PyCFunction::new_closure(py, None, None, move |args, kwargs| -> PyResult<f64> {
        assert!(args.is_empty());
        assert!(kwargs.is_none_or(|k| k.is_empty()));
        Ok(reads.fetch_add(1, Ordering::SeqCst) as f64)
    })
    .unwrap()
}

fn pin_clock(py: Python<'_>, reads: Arc<AtomicUsize>) -> AttrPatch {
    let subject = probe(py);
    let original = subject.getattr("_Budget").unwrap().unbind();
    let pinned = clock(py, reads).unbind();
    let constructor =
        PyCFunction::new_closure(py, None, None, move |args, kwargs| -> PyResult<Py<PyAny>> {
            assert_eq!(args.len(), 1);
            assert!(kwargs.is_none_or(|k| k.is_empty()));
            let py = args.py();
            let settings = PyDict::new(py);
            settings.set_item("clock", pinned.bind(py))?;
            Ok(original.bind(py).call(args, Some(&settings))?.unbind())
        })
        .unwrap();
    AttrPatch::replace(&subject, "_Budget", constructor.as_any())
}

fn field<'py>(result: &'py Py<PyAny>, py: Python<'py>, name: &str) -> Bound<'py, PyAny> {
    result.bind(py).getattr(name).unwrap()
}

fn verdict(result: &Py<PyAny>, py: Python<'_>) -> String {
    text(&field(result, py, "verdict"))
}
fn rule(result: &Py<PyAny>, py: Python<'_>) -> String {
    text(&field(result, py, "rule"))
}
fn usable(result: &Py<PyAny>, py: Python<'_>) -> usize {
    field(result, py, "usable_calls").extract().unwrap()
}

#[test]
fn a_sweep_cut_at_any_point_is_never_reported_as_clean() {
    let case = Case::new();
    let _cwd = workspace(&case);
    Python::attach(|py| {
        let unbounded: BTreeMap<_, _> = run_probe(py, &case, None)
            .iter()
            .map(|r| (rule(r, py), verdict(r, py)))
            .collect();
        assert!(
            unbounded.values().any(|v| v == "NO_DIFFERENCE_OBSERVED"),
            "the fixture must reach a clean verdict"
        );
        let mut usable_when_cut = BTreeSet::new();
        for budget in [1.0, 2.0, 3.0, 5.0, 7.0, 12.0] {
            let _clock = pin_clock(py, Arc::new(AtomicUsize::new(0)));
            let results = run_probe(py, &case, Some(budget));
            assert!(!results.is_empty());
            let cut: Vec<_> = results
                .iter()
                .filter(|r| verdict(r, py) == "OVER_BUDGET")
                .collect();
            assert!(!cut.is_empty(), "budget={budget} was expected to run out");
            for result in &results {
                let actual = verdict(result, py);
                assert!(
                    actual == "OVER_BUDGET" || actual == unbounded[&rule(result, py)],
                    "budget={budget} turned {} into {actual}",
                    rule(result, py)
                );
            }
            usable_when_cut.extend(cut.iter().map(|r| usable(r, py)));
        }
        for boundary in [0, 1, 2] {
            assert!(
                usable_when_cut.contains(&boundary),
                "no construct was cut with {boundary} usable calls"
            );
        }
    });
}

#[test]
fn a_budget_spent_inside_the_sweep_is_not_reported_as_unusable() {
    let case = Case::new();
    let _cwd = workspace(&case);
    Python::attach(|py| {
        let _clock = pin_clock(py, Arc::new(AtomicUsize::new(0)));
        let results = run_probe(py, &case, Some(1.5));
        assert!(!results.is_empty());
        assert_eq!(verdict(&results[0], py), "OVER_BUDGET");
        assert_eq!(
            usable(&results[0], py),
            0,
            "the sweep must really have compared nothing"
        );
    });
}

#[test]
fn an_unswept_construct_carries_no_difference_numbers() {
    let case = Case::new();
    let _cwd = workspace(&case);
    Python::attach(|py| {
        let mut swept_before_the_cut = 0;
        for budget in [None, Some(2.0), Some(3.0), Some(7.0), Some(12.0)] {
            let results = if let Some(seconds) = budget {
                let _clock = pin_clock(py, Arc::new(AtomicUsize::new(0)));
                run_probe(py, &case, Some(seconds))
            } else {
                run_probe(py, &case, Some(1e-9))
            };
            let cut: Vec<_> = results
                .iter()
                .filter(|r| verdict(r, py) == "OVER_BUDGET")
                .collect();
            assert!(!cut.is_empty(), "budget={budget:?} was expected to run out");
            for result in cut {
                for name in ["max_diff_recorded", "max_diff_amplified", "amplifier"] {
                    assert!(
                        field(result, py, name).is_none(),
                        "{name} must be unmeasured"
                    );
                }
                assert!(text(&field(result, py, "detail")).contains("budget"));
                swept_before_the_cut = swept_before_the_cut.max(usable(result, py));
            }
        }
        assert!(
            swept_before_the_cut > 0,
            "every cut landed before its first recorded call"
        );
    });
}

#[test]
fn the_direct_api_and_the_cli_still_sweep_everything_by_default() {
    let case = Case::new();
    let _cwd = workspace(&case);
    Python::attach(|py| {
        let subject = probe(py);
        let defaults = subject
            .getattr("probe_function")
            .unwrap()
            .getattr("__defaults__")
            .unwrap();
        assert!(defaults
            .get_item(defaults.len().unwrap() - 1)
            .unwrap()
            .is_none());
        let defaults = subject
            .getattr("probe_module")
            .unwrap()
            .getattr("__defaults__")
            .unwrap();
        assert!(defaults
            .get_item(defaults.len().unwrap() - 1)
            .unwrap()
            .eq(subject.getattr("FUNCTION_BUDGET_SECONDS").unwrap())
            .unwrap());
        let constructor = subject.getattr("_Budget").unwrap();
        assert!(constructor
            .call1((0,))
            .unwrap()
            .getattr("deadline")
            .unwrap()
            .is_none());
        assert!(constructor
            .call1((py.None(),))
            .unwrap()
            .getattr("deadline")
            .unwrap()
            .is_none());
        let verdicts: BTreeSet<_> = run_probe(py, &case, Some(0.0))
            .iter()
            .map(|r| verdict(r, py))
            .collect();
        assert!(!verdicts.contains("OVER_BUDGET"));
    });
}

#[test]
fn the_budget_latches_so_one_function_reports_one_answer() {
    let _case = Case::new();
    Python::attach(|py| {
        let reads = Arc::new(AtomicUsize::new(0));
        let pinned = clock(py, Arc::clone(&reads));
        let kwargs = PyDict::new(py);
        kwargs.set_item("clock", pinned).unwrap();
        let budget = probe(py)
            .getattr("_Budget")
            .unwrap()
            .call((1.5,), Some(&kwargs))
            .unwrap();
        assert!(!budget
            .call_method0("expired")
            .unwrap()
            .extract::<bool>()
            .unwrap());
        assert!(budget
            .call_method0("expired")
            .unwrap()
            .extract::<bool>()
            .unwrap());
        let at_expiry = reads.load(Ordering::SeqCst);
        assert!(budget
            .call_method0("expired")
            .unwrap()
            .extract::<bool>()
            .unwrap());
        assert_eq!(reads.load(Ordering::SeqCst), at_expiry);
    });
}

#[test]
fn the_remediation_names_a_flag_the_probe_accepts() {
    let _case = Case::new();
    Python::attach(|py| {
        let subject = probe(py);
        let live = subject.getattr("Verdict").unwrap().getattr("LIVE").unwrap();
        let base = subject
            .getattr("AblationResult")
            .unwrap()
            .call1(("f", "r", "d", 1, live, 1))
            .unwrap();
        subject
            .getattr("_over_budget")
            .unwrap()
            .call1((&base, 30.0))
            .unwrap();
        assert!(text(&base.getattr("detail").unwrap()).contains("--budget-seconds 0"));
        let executable: String = module(py, "sys")
            .getattr("executable")
            .unwrap()
            .extract()
            .unwrap();
        let output = Command::new(executable)
            .args(["-m", "conductor.equivalence_probe", "--help"])
            .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
            .env("PYTHONDONTWRITEBYTECODE", "1")
            .env("CUDA_VISIBLE_DEVICES", "")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("--budget-seconds"));
    });
}

/// Every verdict `conductor.equivalence_probe.Verdict` defines.
fn emitted_verdicts(py: Python<'_>) -> BTreeSet<String> {
    let builtins = module(py, "builtins");
    let values = builtins
        .getattr("vars")
        .unwrap()
        .call1((probe(py).getattr("Verdict").unwrap(),))
        .unwrap();
    let str_type = builtins.getattr("str").unwrap();
    let mut emitted = BTreeSet::new();
    for pair in values.call_method0("items").unwrap().try_iter().unwrap() {
        let pair = pair.unwrap();
        let value = pair.get_item(1).unwrap();
        if !text(&pair.get_item(0).unwrap()).starts_with('_')
            && value.is_instance(&str_type).unwrap()
        {
            emitted.insert(text(&value));
        }
    }
    emitted
}

fn finding(verdict: &str, qualname: &str) -> Value {
    json!({"qualname": qualname, "rule": "drop_where", "lineno": 2, "verdict": verdict,
        "description": "torch.where(...) collapsed", "amplifier": "x1e3", "max_diff_amplified": 1.0})
}

/// The native gate's summary for `<rel>/lane.py` when its child reports `findings`.
fn gate_with(py: Python<'_>, case: &Case, rel: &str, findings: &[Value]) -> PyResult<Value> {
    let root = case.mkdir(rel);
    case.write(&format!("{rel}/lane.py"), "class Lane:\n    pass\n");
    let report = root.join("report.json");
    fs::write(&report, serde_json::to_string(findings).unwrap()).unwrap();
    let python = root.join("python");
    fs::write(
        &python,
        format!(
            "#!/bin/sh\nwhile [ $# -gt 0 ]; do [ \"$1\" = --json ] && cp {report:?} \"$2\"; shift; done\n"
        ),
    )
    .unwrap();
    fs::set_permissions(&python, fs::Permissions::from_mode(0o755)).unwrap();
    let units: Vec<(String, usize, Vec<String>)> = findings
        .iter()
        .map(|f| {
            (
                f["qualname"].as_str().unwrap().to_owned(),
                1,
                vec!["test_lane.py".to_owned()],
            )
        })
        .collect();
    let summary: String = module(py, "slop_core")
        .getattr("equivalence_gate")?
        .call1((
            root.to_str().unwrap(),
            python.to_str().unwrap(),
            vec![(
                "lane.py",
                true,
                units,
                Vec::<(String, usize, String)>::new(),
            )],
        ))?
        .extract()?;
    Ok(serde_json::from_str(&summary).unwrap())
}

#[test]
fn every_verdict_the_probe_can_emit_reaches_a_summary_list_or_a_count() {
    let case = Case::new();
    Python::attach(|py| {
        let verdicts = emitted_verdicts(py);
        assert!(verdicts.contains("REACHABLE_BUT_UNTESTED") && verdicts.contains("LIVE"));
        let findings: Vec<Value> = verdicts
            .iter()
            .enumerate()
            .map(|(i, v)| finding(v, &format!("Lane.f{i}")))
            .collect();
        let summary = gate_with(py, &case, "repo", &findings).unwrap();
        let accounted = summary["live"].as_u64().unwrap() as usize
            + [
                "blocking",
                "advisory",
                "untested",
                "incomplete",
                "unmeasured",
                "unreached",
            ]
            .iter()
            .map(|key| summary[key].as_array().unwrap().len())
            .sum::<usize>();
        assert_eq!(accounted, verdicts.len(), "{summary}");
        assert_eq!(summary["incomplete"], json!([]), "{summary}");
    });
}

#[test]
fn an_unclassified_verdict_stops_the_run_rather_than_vanishing() {
    let case = Case::new();
    Python::attach(|py| {
        let error = gate_with(
            py,
            &case,
            "repo",
            &[finding("A_VERDICT_FROM_THE_FUTURE", "Lane.forward")],
        )
        .unwrap_err();
        assert!(error.to_string().contains("unknown verdict"), "{error}");
    });
}

#[test]
fn an_unmeasured_construct_still_counts_its_module_as_probed() {
    let case = Case::new();
    Python::attach(|py| {
        let emitted = emitted_verdicts(py);
        for (i, verdict) in ["OVER_BUDGET", "BASELINE_UNUSABLE", "UNCOMPILABLE"]
            .iter()
            .enumerate()
        {
            assert!(emitted.contains(*verdict), "{verdict}");
            let findings = [finding(verdict, "Lane.slow"), finding("LIVE", "Lane.fast")];
            let summary = gate_with(py, &case, &format!("repo{i}"), &findings).unwrap();
            assert_eq!(summary["modules_probed"], 1, "{summary}");
            assert_eq!(summary["unmeasured"].as_array().unwrap().len(), 1);
            assert_eq!(summary["live"], 1);
        }
    });
}
