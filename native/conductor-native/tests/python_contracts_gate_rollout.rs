#![cfg(feature = "python-compat-tests")]
//! Rust-owned compatibility contracts for the shipped gate rollout policy and CLI.

#[path = "python_contracts/gate_support.rs"]
#[allow(dead_code)]
mod gate_support;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use gate_support::{buffer_text, capture, clear_buffer, isolated_case};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyCFunction, PyDict, PyList, PyModule};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use support::{module, path, text, AttrPatch};

fn rollout<'py>(py: Python<'py>) -> Bound<'py, PyModule> {
    module(py, "conductor.gate_rollout")
}

fn check_run<'py>(
    py: Python<'py>,
    check: &str,
    pr: i64,
    conclusion: &str,
    required: bool,
    merged_at: &str,
    unrelated: bool,
) -> Bound<'py, PyAny> {
    let kwargs = PyDict::new(py);
    kwargs.set_item("check", check).unwrap();
    kwargs.set_item("pr_number", pr).unwrap();
    kwargs.set_item("conclusion", conclusion).unwrap();
    kwargs.set_item("required", required).unwrap();
    kwargs.set_item("merged_at", merged_at).unwrap();
    kwargs.set_item("unrelated_to_diff", unrelated).unwrap();
    rollout(py)
        .getattr("CheckRun")
        .unwrap()
        .call((), Some(&kwargs))
        .unwrap()
}

fn run<'py>(
    py: Python<'py>,
    pr: i64,
    conclusion: &str,
    required: bool,
    unrelated: bool,
    day: u8,
) -> Bound<'py, PyAny> {
    check_run(
        py,
        "candidate-review",
        pr,
        conclusion,
        required,
        &format!("2026-08-{day:02}T00:00:00+00:00"),
        unrelated,
    )
}

fn greens<'py>(py: Python<'py>, count: usize, required: bool) -> Bound<'py, PyList> {
    PyList::new(
        py,
        (1..=count).map(|index| {
            run(
                py,
                index as i64,
                "success",
                required,
                false,
                index as u8 + 1,
            )
        }),
    )
    .unwrap()
}

fn decision(runs: &Bound<'_, PyList>, method: &str, check: &str) -> (bool, String) {
    let py = runs.py();
    let answer = rollout(py)
        .getattr(method)
        .unwrap()
        .call1((runs, check))
        .unwrap();
    (
        answer.get_item(0).unwrap().extract().unwrap(),
        answer.get_item(1).unwrap().extract().unwrap(),
    )
}

fn completed<'py>(
    py: Python<'py>,
    returncode: i32,
    stdout: &str,
    stderr: &str,
) -> Bound<'py, PyAny> {
    let kwargs = PyDict::new(py);
    kwargs.set_item("returncode", returncode).unwrap();
    kwargs.set_item("stdout", stdout).unwrap();
    kwargs.set_item("stderr", stderr).unwrap();
    PyModule::import(py, "types")
        .unwrap()
        .getattr("SimpleNamespace")
        .unwrap()
        .call((), Some(&kwargs))
        .unwrap()
}

fn stub_gh(
    py: Python<'_>,
    ruleset: Value,
    put_returncode: i32,
    put_stderr: &'static str,
) -> (AttrPatch, Arc<Mutex<Vec<Value>>>) {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&sent);
    let callback =
        PyCFunction::new_closure(py, None, None, move |args, kwargs| -> PyResult<Py<PyAny>> {
            assert_eq!(args.len(), 1, "subprocess.run(argv, ...) expected");
            let argv = args.get_item(0)?;
            let py = args.py();
            let output = if argv.contains("--method")? {
                let payload = kwargs
                    .expect("PUT passes keyword arguments")
                    .get_item("input")?
                    .expect("PUT passes an input body");
                let encoded: String = payload.extract()?;
                captured
                    .lock()
                    .unwrap()
                    .push(serde_json::from_str(&encoded).unwrap());
                completed(py, put_returncode, "", put_stderr)
            } else {
                completed(py, 0, &ruleset.to_string(), "")
            };
            Ok(output.unbind())
        })
        .unwrap();
    let subprocess = rollout(py).getattr("subprocess").unwrap();
    let patch = AttrPatch::replace(subprocess.as_any(), "run", callback.as_any());
    (patch, sent)
}

fn main_args(py: Python<'_>, args: &[&str]) -> i32 {
    rollout(py)
        .getattr("main")
        .unwrap()
        .call1((PyList::new(py, args).unwrap(),))
        .unwrap()
        .extract()
        .unwrap()
}

#[test]
fn promotion_is_refused_one_run_below_the_threshold() {
    let _case = isolated_case();
    Python::attach(|py| {
        let threshold: usize = rollout(py)
            .getattr("PROMOTION_GREEN_RUNS")
            .unwrap()
            .extract()
            .unwrap();
        let runs = greens(py, threshold - 1, false);
        let (allowed, reason) = decision(&runs, "may_promote", "candidate-review");
        assert!(!allowed);
        assert!(reason.contains(&threshold.to_string()));
    });
}

#[test]
fn promotion_is_allowed_exactly_at_the_threshold() {
    let _case = isolated_case();
    Python::attach(|py| {
        let threshold: usize = rollout(py)
            .getattr("PROMOTION_GREEN_RUNS")
            .unwrap()
            .extract()
            .unwrap();
        let (allowed, _) = decision(
            &greens(py, threshold, false),
            "may_promote",
            "candidate-review",
        );
        assert!(allowed);
    });
}

#[test]
fn promotion_is_allowed_above_the_threshold() {
    let _case = isolated_case();
    Python::attach(|py| {
        let threshold: usize = rollout(py)
            .getattr("PROMOTION_GREEN_RUNS")
            .unwrap()
            .extract()
            .unwrap();
        let (allowed, _) = decision(
            &greens(py, threshold + 3, false),
            "may_promote",
            "candidate-review",
        );
        assert!(allowed);
    });
}

#[test]
fn a_red_resets_the_promotion_count() {
    let _case = isolated_case();
    Python::attach(|py| {
        let threshold: usize = rollout(py)
            .getattr("PROMOTION_GREEN_RUNS")
            .unwrap()
            .extract()
            .unwrap();
        let runs = greens(py, threshold, false);
        runs.append(run(py, 99, "failure", false, false, 20))
            .unwrap();
        runs.append(run(py, 100, "success", false, false, 21))
            .unwrap();
        let count: i32 = rollout(py)
            .getattr("consecutive_green_as_advisory")
            .unwrap()
            .call1((&runs, "candidate-review"))
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(count, 1);
        assert!(!decision(&runs, "may_promote", "candidate-review").0);
    });
}

#[test]
fn required_runs_do_not_count_toward_promotion() {
    let _case = isolated_case();
    Python::attach(|py| {
        let threshold: usize = rollout(py)
            .getattr("PROMOTION_GREEN_RUNS")
            .unwrap()
            .extract()
            .unwrap();
        let count: i32 = rollout(py)
            .getattr("consecutive_green_as_advisory")
            .unwrap()
            .call1((greens(py, threshold, true), "candidate-review"))
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(count, 0);
    });
}

#[test]
fn promotion_counts_are_per_check() {
    let _case = isolated_case();
    Python::attach(|py| {
        let threshold: usize = rollout(py)
            .getattr("PROMOTION_GREEN_RUNS")
            .unwrap()
            .extract()
            .unwrap();
        assert!(
            !decision(
                &greens(py, threshold, false),
                "may_promote",
                "some-other-check"
            )
            .0
        );
    });
}

#[test]
fn one_unrelated_red_does_not_demote() {
    let _case = isolated_case();
    Python::attach(|py| {
        let runs = PyList::new(py, [run(py, 1, "failure", true, true, 1)]).unwrap();
        assert!(!decision(&runs, "must_demote", "candidate-review").0);
    });
}

#[test]
fn two_consecutive_unrelated_reds_demote() {
    let _case = isolated_case();
    Python::attach(|py| {
        let runs = PyList::new(
            py,
            [
                run(py, 1, "failure", true, true, 1),
                run(py, 2, "failure", true, true, 2),
            ],
        )
        .unwrap();
        let (needed, reason) = decision(&runs, "must_demote", "candidate-review");
        assert!(needed);
        assert!(reason.contains("#2"));
    });
}

#[test]
fn a_red_related_to_its_own_diff_does_not_count() {
    let _case = isolated_case();
    Python::attach(|py| {
        let runs = PyList::new(
            py,
            [
                run(py, 1, "failure", true, true, 1),
                run(py, 2, "failure", true, false, 2),
            ],
        )
        .unwrap();
        assert!(!decision(&runs, "must_demote", "candidate-review").0);
    });
}

#[test]
fn a_green_between_reds_breaks_the_demotion_streak() {
    let _case = isolated_case();
    Python::attach(|py| {
        let runs = PyList::new(
            py,
            [
                run(py, 1, "failure", true, true, 1),
                run(py, 2, "success", true, false, 2),
                run(py, 3, "failure", true, true, 3),
            ],
        )
        .unwrap();
        assert!(!decision(&runs, "must_demote", "candidate-review").0);
    });
}

#[test]
fn advisory_reds_never_demote() {
    let _case = isolated_case();
    Python::attach(|py| {
        let runs = PyList::new(
            py,
            [
                run(py, 1, "failure", false, true, 1),
                run(py, 2, "failure", false, true, 2),
            ],
        )
        .unwrap();
        assert!(!decision(&runs, "must_demote", "candidate-review").0);
    });
}

#[test]
fn ledger_round_trips() {
    let case = isolated_case();
    let ledger = case.root().join("ledger.json");
    Python::attach(|py| {
        let runs = greens(py, 3, false);
        rollout(py)
            .getattr("save_ledger")
            .unwrap()
            .call1((path(py, &ledger), &runs))
            .unwrap();
        let loaded = rollout(py)
            .getattr("load_ledger")
            .unwrap()
            .call1((path(py, &ledger),))
            .unwrap();
        assert!(loaded.eq(runs).unwrap());
    });
}

#[test]
fn missing_ledger_is_empty_not_an_error() {
    let case = isolated_case();
    Python::attach(|py| {
        let loaded = rollout(py)
            .getattr("load_ledger")
            .unwrap()
            .call1((path(py, &case.root().join("absent.json")),))
            .unwrap();
        assert_eq!(loaded.len().unwrap(), 0);
    });
}

#[test]
fn unknown_ledger_schema_is_refused() {
    let case = isolated_case();
    let ledger = case.write(
        "ledger.json",
        &json!({"schema_version": 999, "runs": []}).to_string(),
    );
    Python::attach(|py| {
        let error = rollout(py)
            .getattr("load_ledger")
            .unwrap()
            .call1((path(py, &ledger),))
            .unwrap_err();
        assert!(error
            .matches(py, &rollout(py).getattr("RolloutError").unwrap())
            .unwrap());
    });
}

#[test]
fn promote_refuses_without_the_owner_acknowledgement() {
    let case = isolated_case();
    let ledger = case.root().join("ledger.json");
    Python::attach(|py| {
        let threshold: usize = rollout(py)
            .getattr("PROMOTION_GREEN_RUNS")
            .unwrap()
            .extract()
            .unwrap();
        rollout(py)
            .getattr("save_ledger")
            .unwrap()
            .call1((path(py, &ledger), greens(py, threshold, false)))
            .unwrap();
        // Patch even though the current refusal path exits before any gh call.
        let (_gh, sent) = stub_gh(
            py,
            json!({"name":"fixture", "enforcement":"active", "rules":[]}),
            0,
            "",
        );
        let (stderr, _stderr) = capture(py, "stderr");
        assert_eq!(
            main_args(
                py,
                &[
                    "--ledger",
                    ledger.to_str().unwrap(),
                    "promote",
                    "--check",
                    "candidate-review",
                    "--ruleset",
                    "1"
                ]
            ),
            1
        );
        assert!(buffer_text(&stderr).contains("owner's decision"));
        assert!(
            sent.lock().unwrap().is_empty(),
            "unacknowledged promotion tried to mutate a ruleset"
        );
    });
}

#[test]
fn a_failed_gh_call_names_the_command_and_carries_its_stderr() {
    let _case = isolated_case();
    Python::attach(|py| {
        let callback =
            PyCFunction::new_closure(py, None, None, |args, _kwargs| -> PyResult<Py<PyAny>> {
                assert_eq!(args.len(), 1);
                Ok(completed(args.py(), 1, "", "  HTTP 404: Not Found  \n").unbind())
            })
            .unwrap();
        let subprocess = rollout(py).getattr("subprocess").unwrap();
        let _patch = AttrPatch::replace(subprocess.as_any(), "run", callback.as_any());
        let error = rollout(py)
            .getattr("_gh_api")
            .unwrap()
            .call1((vec!["repos/o/r/rulesets/1"],))
            .unwrap_err();
        assert!(error
            .matches(py, &rollout(py).getattr("RolloutError").unwrap())
            .unwrap());
        let message = text(error.value(py));
        assert!(message.contains("gh api repos/o/r/rulesets/1"));
        assert!(message.contains("HTTP 404: Not Found"));
        assert!(!message.ends_with(' '));
    });
}

#[test]
fn setting_required_checks_replaces_only_the_status_check_rule() {
    let _case = isolated_case();
    Python::attach(|py| {
        let existing = json!({
            "name":"default", "enforcement":"active",
            "conditions":{"ref_name":{"include":["~DEFAULT_BRANCH"]}},
            "rules":[{"type":"deletion"}, {"type":"required_status_checks", "parameters":{"required_status_checks":[{"context":"old"}]}}]
        });
        let (_patch, sent) = stub_gh(py, existing.clone(), 0, "");
        rollout(py)
            .getattr("set_ruleset_required_checks")
            .unwrap()
            .call1(("o/r", 1, vec!["candidate-review"]))
            .unwrap();
        let sent = sent.lock().unwrap();
        assert_eq!(sent.len(), 1);
        let body = &sent[0];
        assert_eq!(body["conditions"], existing["conditions"]);
        let types: std::collections::BTreeSet<_> = body["rules"]
            .as_array()
            .unwrap()
            .iter()
            .map(|rule| rule["type"].as_str().unwrap())
            .collect();
        assert_eq!(types, ["deletion", "required_status_checks"].into());
        let check = body["rules"]
            .as_array()
            .unwrap()
            .iter()
            .find(|rule| rule["type"] == "required_status_checks")
            .unwrap();
        assert_eq!(
            check["parameters"]["required_status_checks"],
            json!([{"context":"candidate-review"}])
        );
        assert_eq!(
            check["parameters"]["strict_required_status_checks_policy"],
            false
        );
    });
}

#[test]
fn an_empty_context_list_removes_the_rule_rather_than_emptying_it() {
    let _case = isolated_case();
    Python::attach(|py| {
        let existing = json!({"name":"default", "enforcement":"active", "rules":[{"type":"required_status_checks", "parameters":{"required_status_checks":[{"context":"old"}]}}]});
        let (_patch, sent) = stub_gh(py, existing, 0, "");
        rollout(py)
            .getattr("set_ruleset_required_checks")
            .unwrap()
            .call1(("o/r", 1, Vec::<String>::new()))
            .unwrap();
        let sent = sent.lock().unwrap();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0]["rules"], json!([]));
        assert!(sent[0].get("conditions").is_none());
    });
}

#[test]
fn a_rejected_ruleset_update_is_raised_not_swallowed() {
    let _case = isolated_case();
    Python::attach(|py| {
        let (_patch, _sent) = stub_gh(
            py,
            json!({"name":"default", "enforcement":"active", "rules":[]}),
            1,
            "Resource not accessible by integration",
        );
        let error = rollout(py)
            .getattr("set_ruleset_required_checks")
            .unwrap()
            .call1(("o/r", 1, vec!["candidate-review"]))
            .unwrap_err();
        assert!(error
            .matches(py, &rollout(py).getattr("RolloutError").unwrap())
            .unwrap());
        assert!(text(error.value(py)).contains("Resource not accessible"));
    });
}

#[test]
fn reading_required_checks_returns_the_contexts_in_the_rule() {
    let _case = isolated_case();
    Python::attach(|py| {
        let payload = json!({"rules":[{"type":"deletion"}, {"type":"required_status_checks", "parameters":{"required_status_checks":[{"context":"a"}, {"context":"b"}]}}]});
        let (_patch, _sent) = stub_gh(py, payload, 0, "");
        let contexts: Vec<String> = rollout(py)
            .getattr("ruleset_required_checks")
            .unwrap()
            .call1(("o/r", 1))
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(contexts, ["a", "b"]);
    });
}

#[test]
fn a_ruleset_with_no_status_check_rule_reads_as_no_required_checks() {
    let _case = isolated_case();
    Python::attach(|py| {
        let (_patch, _sent) = stub_gh(py, json!({"rules":[{"type":"deletion"}]}), 0, "");
        let contexts: Vec<String> = rollout(py)
            .getattr("ruleset_required_checks")
            .unwrap()
            .call1(("o/r", 1))
            .unwrap()
            .extract()
            .unwrap();
        assert!(contexts.is_empty());
    });
}

#[test]
fn record_appends_a_run_and_status_reads_it_back() {
    let case = isolated_case();
    let ledger = case.root().join("ledger.json");
    Python::attach(|py| {
        let (stdout, _stdout) = capture(py, "stdout");
        assert_eq!(
            main_args(
                py,
                &[
                    "--ledger",
                    ledger.to_str().unwrap(),
                    "record",
                    "--check",
                    "candidate-review",
                    "--pr",
                    "7",
                    "--conclusion",
                    "success",
                    "--merged-at",
                    "2026-08-01T00:00:00+00:00"
                ]
            ),
            0
        );
        assert!(buffer_text(&stdout).contains("recorded candidate-review on #7"));
        clear_buffer(&stdout);
        assert_eq!(
            main_args(py, &["--ledger", ledger.to_str().unwrap(), "status"]),
            0
        );
        let out = buffer_text(&stdout);
        assert!(out.contains("candidate-review:"));
        assert!(out.contains("promotion: blocked"));
        assert!(out.contains("demotion:  not needed"));
    });
}

#[test]
fn status_on_an_empty_ledger_says_so_instead_of_printing_nothing() {
    let case = isolated_case();
    let ledger = case.root().join("ledger.json");
    Python::attach(|py| {
        let (stdout, _stdout) = capture(py, "stdout");
        assert_eq!(
            main_args(py, &["--ledger", ledger.to_str().unwrap(), "status"]),
            0
        );
        assert!(buffer_text(&stdout).contains("ledger is empty"));
    });
}
