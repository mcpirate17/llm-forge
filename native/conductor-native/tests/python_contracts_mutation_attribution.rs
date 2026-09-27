#![cfg(feature = "python-compat-tests")]
//! Rust-owned ports of all 26 expanded mutation-attribution pytest contracts.

#[path = "python_contracts/mutation_attribution_support.rs"]
mod attribution_support;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use attribution_support::{
    attribute, call, capture_stderr, fixture, json_to_py, killed, lines, mutant, py_to_json,
    runner, selected, Behavior, Runner, ADD, MODULE, MUL, SOURCE, TESTS,
};
use pyo3::prelude::*;
use pyo3::types::{PyCFunction, PyList};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};
use support::{assert_error, module, path, Case};

fn run<'py>(py: Python<'py>, root: &Path, rows: &[Value], fixture_runner: &Runner) -> Value {
    py_to_json(&attribute(py, root, rows, fixture_runner, 60, json!({}), None).unwrap())
}

fn standard<'py>(py: Python<'py>, root: &Path, rows: &[Value]) -> Value {
    let fixture_runner = runner(py, root, Behavior::Ordinary);
    run(py, root, rows, &fixture_runner)
}

fn narrowed(py: Python<'_>, root: &Path, argv: &[&str]) -> Value {
    py_to_json(&call(
        py,
        "_narrowed",
        (PyList::new(py, argv).unwrap(), path(py, root)),
    ))
}

fn narrowing_case(argv: &[&str], expected: &[&str]) {
    let case = Case::new();
    fixture(case.root());
    Python::attach(|py| assert_eq!(narrowed(py, case.root(), argv), json!(expected)));
}

#[test]
fn attribution_charges_each_mutant_to_the_test_that_actually_failed() {
    let case = Case::new();
    fixture(case.root());
    Python::attach(|py| {
        let receipt = standard(
            py,
            case.root(),
            &[
                killed("add-op", "a + b", "a - b"),
                killed("mul-op", "a * b", "a / b"),
            ],
        );
        assert_eq!(receipt["attribution"]["status"], "ATTRIBUTED");
        assert_eq!(receipt["attribution"]["attributed_mutants"], 2);
        assert_eq!(receipt["attribution"]["unattributed"], json!([]));
        assert_eq!(
            receipt["test_value"]["killers_by_mutant"],
            json!({"add-op": [ADD], "mul-op": [MUL]})
        );
        let classes: BTreeMap<_, _> = receipt["test_value"]["tests"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| {
                (
                    row["nodeid"].as_str().unwrap(),
                    row["classification"].as_str().unwrap(),
                )
            })
            .collect();
        assert_eq!(classes, BTreeMap::from([(ADD, "CORE"), (MUL, "CORE")]));
    });
}

#[test]
fn a_mutant_whose_covering_set_still_passes_is_recorded_not_dropped() {
    let case = Case::new();
    fixture(case.root());
    Python::attach(|py| {
        let receipt = standard(py, case.root(), &[killed("dead-op", "def mul", "def  mul")]);
        assert!(receipt["test_value"].is_null());
        assert_eq!(receipt["attribution"]["status"], "NO_ATTRIBUTION");
        assert_eq!(
            receipt["attribution"]["unattributed"],
            json!([{"id": "dead-op", "reason": "covering set passed on re-run"}])
        );
        let mut misaligned = killed("add-op", "a + b", "a - b");
        misaligned["byte_offset"] = json!(misaligned["byte_offset"].as_u64().unwrap() + 1);
        let err = attribute(
            py,
            case.root(),
            &[misaligned],
            &runner(py, case.root(), Behavior::Ordinary),
            60,
            json!({}),
            None,
        )
        .unwrap_err();
        assert_error(
            py,
            err,
            &module(py, "conductor.mutation_scope")
                .getattr("CampaignError")
                .unwrap(),
            "does not match",
        );
    });
}

#[test]
fn only_killed_mutants_are_reapplied_and_none_says_why() {
    let case = Case::new();
    fixture(case.root());
    Python::attach(|py| {
        let receipt = standard(
            py,
            case.root(),
            &[
                killed("add-op", "a + b", "a - b"),
                mutant("lived", "a * b", "a / b", "SURVIVED", &[ADD, MUL]),
            ],
        );
        assert_eq!(receipt["attribution"]["killed_mutants"], 1);
        assert_eq!(
            receipt["test_value"]["killers_by_mutant"],
            json!({"add-op": [ADD]})
        );
        let none = standard(
            py,
            case.root(),
            &[mutant("lived", "a + b", "a - b", "SURVIVED", &[ADD, MUL])],
        );
        assert!(none["test_value"].is_null());
        assert_eq!(none["attribution"]["status"], "NOT_ATTEMPTED");
        assert_eq!(none["attribution"]["reason"], "no mutant was killed");
        assert_eq!(none["attribution"]["attributed_mutants"], 0);

        let calls = Arc::new(Mutex::new(Vec::<(i32, i32)>::new()));
        let observed = Arc::clone(&calls);
        let progress = PyCFunction::new_closure(py, None, None, move |args, _| -> PyResult<()> {
            observed
                .lock()
                .unwrap()
                .push((args.get_item(0)?.extract()?, args.get_item(1)?.extract()?));
            Ok(())
        })
        .unwrap();
        attribute(
            py,
            case.root(),
            &[
                killed("add-op-1", "a + b", "a - b"),
                killed("mul-op-1", "a * b", "a / b"),
                killed("add-op-2", "a + b", "a - b"),
            ],
            &runner(py, case.root(), Behavior::Ordinary),
            60,
            json!({}),
            Some(progress.as_any()),
        )
        .unwrap();
        assert_eq!(*calls.lock().unwrap(), [(0, 3), (1, 3), (2, 3)]);
    });
}

#[test]
fn coverage_map_is_ranked_by_what_a_junit_report_can_name() {
    let case = Case::new();
    fixture(case.root());
    Python::attach(|py| {
        assert!(call(py, "_function", ("crate::suite",)).is_none());
        assert_eq!(
            call(py, "_function", (format!("{ADD}[one]"),))
                .extract::<String>()
                .unwrap(),
            ADD
        );
        let unusable = standard(
            py,
            case.root(),
            &[mutant(
                "add-op",
                "a + b",
                "a - b",
                "KILLED",
                &["crate::suite"],
            )],
        );
        assert_eq!(unusable["attribution"]["status"], "NOT_ATTEMPTED");
        assert_eq!(
            unusable["attribution"]["unusable_nodeids"],
            json!(["crate::suite"])
        );
        assert_eq!(unusable["attribution"]["ranked_tests"], 0);
        assert_eq!(
            unusable["attribution"]["reason"],
            "the coverage map names no usable pytest nodeid"
        );
        let parameterized = standard(
            py,
            case.root(),
            &[mutant(
                "add-op",
                "a + b",
                "a - b",
                "KILLED",
                &[&format!("{ADD}[one]"), &format!("{ADD}[two]"), MUL],
            )],
        );
        assert_eq!(parameterized["attribution"]["ranked_tests"], 2);
        assert_eq!(parameterized["attribution"]["unusable_nodeids"], json!([]));
        assert_eq!(
            parameterized["test_value"]["killers_by_mutant"],
            json!({"add-op": [ADD]})
        );
    });
}

#[test]
fn baseline_precedes_mutants_and_both_select_only_covering_tests() {
    let case = Case::new();
    fixture(case.root());
    Python::attach(|py| {
        let fixture_runner = runner(py, case.root(), Behavior::Ordinary);
        run(
            py,
            case.root(),
            &[killed("add-op", "a + b", "a - b")],
            &fixture_runner,
        );
        let commands = fixture_runner.state.lock().unwrap().commands.clone();
        let baselines: Vec<_> = commands
            .iter()
            .filter(|command| command.iter().any(|arg| arg.contains("baseline-")))
            .collect();
        let expected: usize = module(py, "conductor.mutation_attribution")
            .getattr("BASELINE_REPETITIONS")
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(baselines.len(), expected);
        assert_eq!(
            &commands[..expected],
            baselines.iter().map(|c| (*c).clone()).collect::<Vec<_>>()
        );
        let mutant_command = commands.last().unwrap();
        assert!(!mutant_command.contains(&TESTS.to_owned()));
        let interpreter: String = module(py, "sys")
            .getattr("executable")
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(
            &mutant_command[..4],
            [interpreter, "-m".into(), "pytest".into(), "-q".into()]
        );
        assert_eq!(
            mutant_command
                .iter()
                .filter(|arg| *arg == ADD || *arg == MUL)
                .map(String::as_str)
                .collect::<Vec<_>>(),
            [ADD, MUL]
        );
    });
}

#[test]
fn narrowing_removes_path_selection() {
    narrowing_case(&["pytest", "-q", TESTS], &["pytest", "-q"]);
}
#[test]
fn narrowing_removes_exit_first() {
    narrowing_case(&["pytest", "-x", "-q"], &["pytest", "-q"]);
}
#[test]
fn narrowing_removes_separate_maxfail() {
    narrowing_case(&["pytest", "--maxfail", "3", "-q"], &["pytest", "-q"]);
}
#[test]
fn narrowing_removes_inline_maxfail() {
    narrowing_case(&["pytest", "--maxfail=3", "-q"], &["pytest", "-q"]);
}
#[test]
fn narrowing_removes_keyword_selection() {
    narrowing_case(&["pytest", "-k", "add", "-q"], &["pytest", "-q"]);
}
#[test]
fn narrowing_removes_nodeid_selection() {
    narrowing_case(&["pytest", "-q", ADD], &["pytest", "-q"]);
}
#[test]
fn narrowing_keeps_plugin_flag_and_value() {
    narrowing_case(
        &["pytest", "-p", "no:cacheprovider"],
        &["pytest", "-p", "no:cacheprovider"],
    );
}

#[test]
fn source_and_bytecode_are_restored_around_every_mutation() {
    let case = Case::new();
    fixture(case.root());
    Python::attach(|py| {
        standard(
            py,
            case.root(),
            &[
                killed("add-op", "a + b", "a - b"),
                killed("mul-op", "a * b", "a / b"),
            ],
        );
        assert_eq!(
            fs::read_to_string(case.root().join(MODULE)).unwrap(),
            SOURCE
        );
        let cache = case.root().join("pkg/__pycache__");
        fs::create_dir_all(&cache).unwrap();
        let stale = cache.join("calc.cpython-312.pyc");
        fs::write(&stale, b"stale").unwrap();
        let context = call(
            py,
            "_applied",
            (
                path(py, case.root()),
                json_to_py(py, &killed("add-op", "a + b", "a - b")),
            ),
        );
        context.call_method0("__enter__").unwrap();
        let evicted_while_live = !stale.exists();
        fs::write(&stale, b"mutated").unwrap();
        context
            .call_method1("__exit__", (py.None(), py.None(), py.None()))
            .unwrap();
        assert!(evicted_while_live);
        assert!(!stale.exists());
    });
}

#[test]
fn summary_names_every_field_a_reader_needs() {
    let case = Case::new();
    fixture(case.root());
    Python::attach(|py| {
        let receipt = standard(py, case.root(), &[killed("add-op", "a + b", "a - b")]);
        let mut keys: Vec<_> = receipt["attribution"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        keys.sort();
        assert_eq!(
            keys,
            [
                "attributed_mutants",
                "baseline_repetitions",
                "killed_mutants",
                "ranked_tests",
                "schema_version",
                "status",
                "unattributed",
                "unusable_nodeids"
            ]
        );
    });
}

#[test]
fn absolute_pytest_is_recognized_and_survives_narrowing() {
    let case = Case::new();
    fixture(case.root());
    let binary = case.root().join("bin/pytest");
    fs::create_dir_all(binary.parent().unwrap()).unwrap();
    fs::write(&binary, "#!/bin/sh\n").unwrap();
    Python::attach(|py| {
        let name = binary.to_str().unwrap();
        assert_eq!(
            call(py, "_pytest_index", (vec![name, "-q"],))
                .extract::<i32>()
                .unwrap(),
            0
        );
        assert_eq!(
            narrowed(py, case.root(), &[name, "-q"]),
            json!([name, "-q"])
        );
    });
}

#[test]
fn narrowing_keeps_later_flags_and_refuses_unusable_commands() {
    let case = Case::new();
    fixture(case.root());
    Python::attach(|py| {
        assert_eq!(
            narrowed(py, case.root(), &["pytest", TESTS, "-q", "--tb=no"]),
            json!(["pytest", "-q", "--tb=no"])
        );
        let attribution = module(py, "conductor.mutation_attribution");
        let err = attribution
            .getattr("_narrowed")
            .unwrap()
            .call1((vec!["cargo", "test"], path(py, case.root())))
            .unwrap_err();
        assert_error(
            py,
            err,
            &module(py, "conductor.mutation_scope")
                .getattr("CampaignError")
                .unwrap(),
            "needs a pytest test command",
        );
        let err = selected(
            py,
            &["pytest", "--junitxml=other.xml"],
            case.root(),
            &[ADD],
            Path::new("x.xml"),
        )
        .unwrap_err();
        assert_error(
            py,
            err,
            &module(py, "conductor.mutation_value")
                .getattr("ValueEvidenceError")
                .unwrap(),
            "junitxml",
        );
    });
}

#[test]
fn selection_pins_rootdir_to_the_snapshot_worktree() {
    let case = Case::new();
    fixture(case.root());
    Python::attach(|py| {
        let report = case.root().join("r.xml");
        let selected_command =
            py_to_json(&selected(py, &["pytest", "-q"], case.root(), &[ADD], &report).unwrap());
        let pinned = format!("--rootdir={}", case.root().display());
        assert!(selected_command
            .as_array()
            .unwrap()
            .contains(&json!(pinned)));
        for explicit in [vec!["--rootdir=elsewhere"], vec!["--rootdir", "elsewhere"]] {
            let mut argv = vec!["pytest", "-q"];
            argv.extend(explicit.iter().copied());
            let selected_command =
                py_to_json(&selected(py, &argv, case.root(), &[ADD], &report).unwrap());
            let selected_command = selected_command.as_array().unwrap();
            assert!(!selected_command.contains(&json!(pinned)));
            assert!(explicit
                .iter()
                .all(|arg| selected_command.contains(&json!(arg))));
        }
    });
}

#[test]
fn reruns_never_disable_the_run_private_cache() {
    let case = Case::new();
    fixture(case.root());
    Python::attach(|py| {
        let fixture_runner = runner(py, case.root(), Behavior::Ordinary);
        attribute(
            py,
            case.root(),
            &[killed("add-op", "a + b", "a - b")],
            &fixture_runner,
            60,
            json!({"EXISTING": "kept"}),
            None,
        )
        .unwrap();
        let environments = fixture_runner.state.lock().unwrap().environments.clone();
        assert!(!environments.is_empty());
        for environment in environments {
            assert!(environment.get("PYTHONDONTWRITEBYTECODE").is_none());
            assert_eq!(environment["EXISTING"], "kept");
        }
    });
}

#[test]
fn reapplying_a_mutant_evicts_beside_and_private_bytecode() {
    let case = Case::new();
    fixture(case.root());
    Python::attach(|py| {
        let target = case.root().join(MODULE);
        let scratch = module(py, "conductor.bytecode_isolation")
            .getattr("scratch_root_for")
            .unwrap()
            .call1((path(py, case.root()),))
            .unwrap();
        let prefix = scratch.call_method1("__truediv__", ("pycache",)).unwrap();
        let cached = module(py, "conductor.bytecode_isolation")
            .getattr("cache_paths_for")
            .unwrap()
            .call1((path(py, &target), prefix))
            .unwrap()
            .get_item(0)
            .unwrap()
            .str()
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        let cached = Path::new(&cached);
        let tag: String = module(py, "sys")
            .getattr("implementation")
            .unwrap()
            .getattr("cache_tag")
            .unwrap()
            .extract()
            .unwrap();
        let beside = target
            .parent()
            .unwrap()
            .join("__pycache__")
            .join(format!("calc.{tag}.pyc"));
        fs::create_dir_all(beside.parent().unwrap()).unwrap();
        fs::create_dir_all(cached.parent().unwrap()).unwrap();
        fs::write(&beside, b"stale").unwrap();
        fs::write(cached, b"stale").unwrap();
        let context = call(
            py,
            "_applied",
            (
                path(py, case.root()),
                json_to_py(py, &killed("add-op", "a + b", "a - b")),
            ),
        );
        context.call_method0("__enter__").unwrap();
        let mutated = fs::read_to_string(&target).unwrap().contains("a - b");
        let evicted_live = !beside.exists() && !cached.exists();
        fs::write(&beside, b"stale").unwrap();
        fs::write(cached, b"stale").unwrap();
        context
            .call_method1("__exit__", (py.None(), py.None(), py.None()))
            .unwrap();
        assert!(mutated && evicted_live);
        assert!(fs::read_to_string(&target).unwrap().contains("a + b"));
        assert!(!beside.exists() && !cached.exists());
    });
}

#[test]
fn reports_are_written_to_their_own_directory() {
    let case = Case::new();
    fixture(case.root());
    Python::attach(|py| {
        let fixture_runner = runner(py, case.root(), Behavior::Ordinary);
        run(
            py,
            case.root(),
            &[killed("add-op", "a + b", "a - b")],
            &fixture_runner,
        );
        let reports = fixture_runner.state.lock().unwrap().reports.clone();
        assert!(!reports.is_empty());
        assert!(reports
            .iter()
            .all(|report| report.parent() == Some(case.root().join(".attribution").as_path())));
    });
}

#[test]
fn progress_prints_every_position_when_the_run_is_small() {
    let _case = Case::new();
    Python::attach(|py| {
        let (_patch, stderr) = capture_stderr(py);
        for position in 0..5 {
            call(py, "_stderr_progress", (position, 5));
        }
        let expected: Vec<_> = (1..=5)
            .map(|i| format!("attribution: {i}/5 killed mutants re-run"))
            .collect();
        assert_eq!(lines(&stderr), expected);
    });
}

#[test]
fn progress_step_arithmetic_is_pinned_at_chosen_totals() {
    let _case = Case::new();
    Python::attach(|py| {
        let (_patch, stderr) = capture_stderr(py);
        for position in 0..30 {
            call(py, "_stderr_progress", (position, 30));
        }
        let positions = [0, 2, 5, 8, 11, 14, 17, 20, 23, 26, 29];
        let expected: Vec<_> = positions
            .iter()
            .map(|position| format!("attribution: {}/30 killed mutants re-run", position + 1))
            .collect();
        assert_eq!(lines(&stderr), expected);
        stderr.call_method1("seek", (0,)).unwrap();
        stderr.call_method1("truncate", (0,)).unwrap();
        for position in 0..31 {
            call(py, "_stderr_progress", (position, 31));
        }
        assert_eq!(
            lines(&stderr).last().unwrap(),
            "attribution: 31/31 killed mutants re-run"
        );
        assert_ne!(31 % 3, 0);
        stderr.call_method1("seek", (0,)).unwrap();
        stderr.call_method1("truncate", (0,)).unwrap();
        for position in 0..47 {
            call(py, "_stderr_progress", (position, 47));
        }
        let emitted = lines(&stderr);
        assert!(!emitted.is_empty());
        assert!(emitted.len() < 47);
        assert_eq!(
            emitted.first().unwrap(),
            "attribution: 1/47 killed mutants re-run"
        );
        assert_eq!(
            emitted.last().unwrap(),
            "attribution: 47/47 killed mutants re-run"
        );
    });
}

#[test]
fn mutant_that_aborts_the_rerun_is_unattributed_not_fatal() {
    let case = Case::new();
    fixture(case.root());
    Python::attach(|py| {
        let fixture_runner = runner(py, case.root(), Behavior::AbortOn("a // b"));
        let receipt = run(
            py,
            case.root(),
            &[
                killed("abort-op", "a + b", "a // b"),
                killed("mul-op", "a * b", "a / b"),
            ],
            &fixture_runner,
        );
        assert_eq!(
            receipt["attribution"]["unattributed"],
            json!([{"id": "abort-op", "reason": "covering set crashed without a report"}])
        );
        assert_eq!(
            receipt["test_value"]["killers_by_mutant"],
            json!({"mul-op": [MUL]})
        );
    });
}

#[test]
fn baseline_that_writes_no_report_still_refuses() {
    let case = Case::new();
    fixture(case.root());
    Python::attach(|py| {
        let err = attribute(
            py,
            case.root(),
            &[killed("add-op", "a + b", "a - b")],
            &runner(py, case.root(), Behavior::AbortOn("def add")),
            60,
            json!({}),
            None,
        )
        .unwrap_err();
        assert_error(
            py,
            err,
            &module(py, "conductor.mutation_scope")
                .getattr("CampaignError")
                .unwrap(),
            "baseline 0 exited -6 without a JUnit",
        );
    });
}

#[test]
fn hung_mutant_gets_baseline_derived_limit_and_reports_it() {
    let case = Case::new();
    fixture(case.root());
    Python::attach(|py| {
        let (_patch, stderr) = capture_stderr(py);
        let fixture_runner = runner(py, case.root(), Behavior::HangOn("a - b", 20.0));
        let receipt = py_to_json(
            &attribute(
                py,
                case.root(),
                &[
                    killed("hang-op", "a + b", "a - b"),
                    killed("mul-op", "a * b", "a / b"),
                ],
                &fixture_runner,
                1800,
                json!({}),
                None,
            )
            .unwrap(),
        );
        assert_eq!(
            fixture_runner.state.lock().unwrap().timeouts,
            [1800, 1800, 200, 200]
        );
        let error = lines(&stderr).join("\n");
        assert!(error.contains("attribution: mutant 1/2 (hang-op) re-run timed out after 200s"));
        assert!(!error.contains("mul-op) re-run timed out"));
        assert_eq!(
            receipt["attribution"]["unattributed"],
            json!([{"id": "hang-op", "reason": "covering set timed out"}])
        );
        assert_eq!(
            receipt["test_value"]["killers_by_mutant"],
            json!({"mul-op": [MUL]})
        );
    });
}

#[test]
fn rerun_limit_is_capped_and_floored() {
    let case = Case::new();
    fixture(case.root());
    Python::attach(|py| {
        for (cap, baseline, expected) in [
            (1800, 20.0, 200),
            (1800, 180.0, 1800),
            (1800, 181.0, 1800),
            (1800, 179.01, 1791),
            (1800, 0.5, 60),
            (1800, 6.0, 60),
            (1800, 6.01, 61),
            (30, 0.5, 30),
        ] {
            assert_eq!(
                call(py, "rerun_timeout", (cap, baseline))
                    .extract::<i32>()
                    .unwrap(),
                expected
            );
        }
        let fixture_runner = runner(py, case.root(), Behavior::HangOn("a - b", 900.0));
        run(
            py,
            case.root(),
            &[killed("hang-op", "a + b", "a - b")],
            &fixture_runner,
        );
        assert_eq!(fixture_runner.state.lock().unwrap().timeouts, [60, 60, 60]);
    });
}
