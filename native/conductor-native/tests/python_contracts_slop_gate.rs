#![cfg(feature = "python-compat-tests")]
//! Rust-owned contracts for the shipped slop gate and its probe orchestration.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::exceptions::PyAssertionError;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyCFunction, PyDict, PyModule};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};
use support::{assert_error, module, path, AttrPatch, Case};

fn gate<'py>(py: Python<'py>) -> Bound<'py, PyModule> {
    module(py, "conductor.slop_gate")
}

fn py_json<'py>(py: Python<'py>, value: Value) -> Bound<'py, PyAny> {
    module(py, "json")
        .getattr("loads")
        .unwrap()
        .call1((value.to_string(),))
        .unwrap()
}

fn json_value(py: Python<'_>, value: &Bound<'_, PyAny>) -> Value {
    let serialized: String = module(py, "json")
        .getattr("dumps")
        .unwrap()
        .call1((value,))
        .unwrap()
        .extract()
        .unwrap();
    serde_json::from_str(&serialized).unwrap()
}

fn py_result_json(py: Python<'_>, value: &Value) -> PyResult<Py<PyAny>> {
    Ok(py
        .import("json")?
        .getattr("loads")?
        .call1((value.to_string(),))?
        .unbind())
}

fn finding(verdict: &str, rule: &str, name: &str) -> Value {
    json!({
        "qualname": name, "rule": rule, "lineno": 12, "verdict": verdict,
        "description": "torch.where(...) collapsed", "amplifier": "params_x1e3",
        "max_diff_amplified": 0.9,
    })
}

fn basic(verdict: &str) -> Value {
    finding(verdict, "drop_where", "Lane.forward")
}

fn git(root: &Path, args: &[&str]) {
    let mut command = Command::new("git");
    for name in GIT_SELECTORS {
        command.env_remove(name);
    }
    let result = command
        .args(args)
        .current_dir(root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&result.stderr)
    );
}

const GIT_SELECTORS: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_NAMESPACE",
    "GIT_CEILING_DIRECTORIES",
    "GIT_DISCOVERY_ACROSS_FILESYSTEM",
    "GIT_CONFIG_COUNT",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG",
    "GIT_TEMPLATE_DIR",
];

fn repo() -> Case {
    let mut case = Case::new();
    case.set_env("GIT_CONFIG_NOSYSTEM", "1");
    case.set_env("GIT_CONFIG_GLOBAL", "/dev/null");
    case.set_env("GIT_CONFIG_SYSTEM", "/dev/null");
    for name in GIT_SELECTORS {
        case.remove_env(name);
    }
    git(case.root(), &["init", "-q"]);
    git(
        case.root(),
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "root",
        ],
    );
    case.write(
        "conductor/slop_waivers.json",
        &json!({"waivers":[]}).to_string(),
    );
    case
}

fn call_run(py: Python<'_>, root: &Path, mods: &[&str], jobs: Option<i32>) -> (i32, Value) {
    let kwargs = PyDict::new(py);
    kwargs.set_item("only", mods).unwrap();
    if let Some(jobs) = jobs {
        kwargs.set_item("jobs", jobs).unwrap();
    }
    let result = gate(py)
        .getattr("run")
        .unwrap()
        .call(("HEAD", path(py, root)), Some(&kwargs))
        .unwrap();
    let code: i32 = result.get_item(0).unwrap().extract().unwrap();
    (code, json_value(py, &result.get_item(1).unwrap()))
}

fn check_args(
    args: &Bound<'_, pyo3::types::PyTuple>,
    kwargs: Option<&Bound<'_, PyDict>>,
    count: usize,
) -> PyResult<()> {
    if args.len() != count || kwargs.is_some_and(|kw| !kw.is_empty()) {
        return Err(PyAssertionError::new_err(format!(
            "expected {count} positional arguments"
        )));
    }
    Ok(())
}

fn fixed_drivers(py: Python<'_>, drivers: &[&str]) -> AttrPatch {
    let expected = drivers.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
    let callback = PyCFunction::new_closure(
        py,
        None,
        None,
        move |args, kwargs| -> PyResult<Vec<String>> {
            check_args(args, kwargs, 3)?;
            Ok(expected.clone())
        },
    )
    .unwrap();
    AttrPatch::replace(gate(py).as_any(), "drivers_for", callback.as_any())
}

fn no_index(py: Python<'_>) -> AttrPatch {
    let callback =
        PyCFunction::new_closure(py, None, None, move |args, kwargs| -> PyResult<Py<PyAny>> {
            check_args(args, kwargs, 1)?;
            Ok(args.py().None())
        })
        .unwrap();
    AttrPatch::replace(gate(py).as_any(), "build_index", callback.as_any())
}

fn fixed_probe(py: Python<'_>, findings: Value) -> AttrPatch {
    let callback =
        PyCFunction::new_closure(py, None, None, move |args, kwargs| -> PyResult<Py<PyAny>> {
            check_args(args, kwargs, 5)?;
            py_result_json(args.py(), &findings)
        })
        .unwrap();
    AttrPatch::replace(gate(py).as_any(), "probe", callback.as_any())
}

fn probe(py: Python<'_>, root: &Path, module_name: &str) -> Value {
    json_value(
        py,
        &gate(py)
            .getattr("probe")
            .unwrap()
            .call1((module_name, vec!["test_lane.py"], path(py, root), py.None()))
            .unwrap(),
    )
}

fn probe_stub(py: Python<'_>, code: i32, stderr: &str, report: Option<&str>) -> AttrPatch {
    let stderr = stderr.to_owned();
    let report = report.map(str::to_owned);
    let callback = PyCFunction::new_closure(
        py,
        None,
        None,
        move |args, _kwargs| -> PyResult<Py<PyAny>> {
            if args.len() != 1 {
                return Err(PyAssertionError::new_err("subprocess.run expects argv"));
            }
            let py = args.py();
            let argv: Vec<String> = args.get_item(0)?.extract()?;
            if let Some(report) = &report {
                let at = argv
                    .iter()
                    .position(|arg| arg == "--json")
                    .ok_or_else(|| PyAssertionError::new_err("missing --json"))?;
                fs::write(&argv[at + 1], report)
                    .map_err(|error| PyAssertionError::new_err(error.to_string()))?;
            }
            Ok(py
                .import("subprocess")?
                .getattr("CompletedProcess")?
                .call1((args.get_item(0)?, code, "", stderr.as_str()))?
                .unbind())
        },
    )
    .unwrap();
    AttrPatch::replace(
        &gate(py).getattr("subprocess").unwrap(),
        "run",
        callback.as_any(),
    )
}

fn verdicts<'a>(summary: &'a Value, bucket: &str) -> Vec<&'a str> {
    summary[bucket]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["verdict"].as_str().unwrap())
        .collect()
}

#[test]
fn changed_set_excludes_test_files() {
    let case = repo();
    case.write("lane.py", "x = 1\n");
    case.write("test_lane.py", "x = 1\n");
    git(case.root(), &["add", "--", "lane.py", "test_lane.py"]);
    Python::attach(|py| {
        let changed = gate(py)
            .getattr("changed_modules")
            .unwrap()
            .call1(("HEAD", path(py, case.root())))
            .unwrap();
        assert_eq!(json_value(py, &changed), json!(["lane.py"]));
    });
}

#[test]
fn drivers_are_tests_that_import_module() {
    let case = repo();
    case.write("lane.py", "VALUE = 1\n");
    case.write("test_lane.py", "from lane import VALUE\n");
    case.write("test_other.py", "import json\n");
    Python::attach(|py| {
        let drivers = gate(py)
            .getattr("drivers_for")
            .unwrap()
            .call1(("lane.py", path(py, case.root())))
            .unwrap();
        assert_eq!(json_value(py, &drivers), json!(["test_lane.py"]));
    });
}

#[test]
fn run_builds_one_index_and_shares_it() {
    let case = repo();
    case.write("lane.py", "VALUE = 1\n");
    case.write("other.py", "VALUE = 2\n");
    case.write("third.py", "VALUE = 3\n");
    case.write("test_lane.py", "from lane import VALUE\n");
    Python::attach(|py| {
        let body = gate(py);
        let real = body.getattr("build_index").unwrap().unbind();
        let calls = Arc::new(Mutex::new(0usize));
        let observed = Arc::clone(&calls);
        let callback =
            PyCFunction::new_closure(py, None, None, move |args, kwargs| -> PyResult<Py<PyAny>> {
                check_args(args, kwargs, 1)?;
                *observed.lock().unwrap() += 1;
                Ok(real.bind(args.py()).call1((args.get_item(0)?,))?.unbind())
            })
            .unwrap();
        let _index = AttrPatch::replace(body.as_any(), "build_index", callback.as_any());
        let _probe = fixed_probe(py, json!([]));
        call_run(py, case.root(), &["lane.py", "other.py", "third.py"], None);
        assert_eq!(*calls.lock().unwrap(), 1);
    });
}

#[test]
fn reachable_untested_branch_blocks() {
    let case = repo();
    Python::attach(|py| {
        let _drivers = fixed_drivers(py, &["test_lane.py"]);
        let _index = no_index(py);
        let _probe = fixed_probe(py, json!([basic("REACHABLE_BUT_UNTESTED")]));
        let (code, summary) = call_run(py, case.root(), &["lane.py"], None);
        assert_eq!(code, 1);
        assert_eq!(summary["blocking"].as_array().unwrap().len(), 1);
    });
}

#[test]
fn advisory_verdicts_never_block() {
    let case = repo();
    Python::attach(|py| {
        let _drivers = fixed_drivers(py, &["test_lane.py"]);
        let _index = no_index(py);
        let _probe = fixed_probe(
            py,
            json!([
                basic("NO_DIFFERENCE_OBSERVED"),
                basic("WITHIN_NUMERIC_NOISE")
            ]),
        );
        let (code, summary) = call_run(py, case.root(), &["lane.py"], None);
        assert_eq!(code, 0);
        assert_eq!(summary["advisory"].as_array().unwrap().len(), 2);
    });
}

#[test]
fn waiver_downgrades_only_rule_it_names() {
    let case = repo();
    case.write("conductor/slop_waivers.json", &json!({"waivers":[{"module":"lane.py","rule":"drop_where","qualname":"Lane.forward","reason":"covered by the integration probe, not the unit tests"}]}).to_string());
    Python::attach(|py| {
        let _drivers = fixed_drivers(py, &["test_lane.py"]);
        let _index = no_index(py);
        let _probe = fixed_probe(
            py,
            json!([
                finding("REACHABLE_BUT_UNTESTED", "drop_where", "Lane.forward"),
                finding("REACHABLE_BUT_UNTESTED", "drop_clamp_min", "Lane.forward"),
            ]),
        );
        let (code, summary) = call_run(py, case.root(), &["lane.py"], None);
        assert_eq!(code, 1);
        assert_eq!(
            summary["blocking"]
                .as_array()
                .unwrap()
                .iter()
                .map(|row| row["rule"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["drop_clamp_min"]
        );
        assert_eq!(
            summary["advisory"]
                .as_array()
                .unwrap()
                .iter()
                .map(|row| row["rule"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["drop_where"]
        );
    });
}

#[test]
fn unreached_is_separated_from_genuinely_untested() {
    let case = repo();
    case.write("test_thing.py", "from lane import mentioned\n");
    git(
        case.root(),
        &["add", "--", "test_thing.py", "conductor/slop_waivers.json"],
    );
    let findings = json!([
        {"qualname":"mentioned","verdict":"NOT_EXERCISED","rule":"r","lineno":1},
        {"qualname":"nowhere_at_all","verdict":"NOT_EXERCISED","rule":"r","lineno":2},
        {"qualname":"other","verdict":"LIVE","rule":"r","lineno":3},
    ]);
    Python::attach(|py| {
        let result = gate(py)
            .getattr("refine_unexercised")
            .unwrap()
            .call1((py_json(py, findings), path(py, case.root())))
            .unwrap();
        let refined = json_value(py, &result);
        let by_name = refined
            .as_array()
            .unwrap()
            .iter()
            .map(|row| {
                (
                    row["qualname"].as_str().unwrap(),
                    row["verdict"].as_str().unwrap(),
                )
            })
            .collect::<std::collections::HashMap<_, _>>();
        assert_eq!(by_name["mentioned"], "NOT_REACHED_BY_DRIVERS");
        assert_eq!(by_name["nowhere_at_all"], "NOT_EXERCISED");
        assert_eq!(by_name["other"], "LIVE");
    });
}

#[test]
fn module_with_no_driver_is_reported_not_silently_passed() {
    let case = repo();
    Python::attach(|py| {
        let _drivers = fixed_drivers(py, &[]);
        let (code, summary) = call_run(py, case.root(), &["lane.py"], None);
        assert_eq!(code, 0);
        assert_eq!(summary["modules_without_drivers"], json!(["lane.py"]));
        assert_eq!(summary["modules_probed"], 0);
    });
}

#[test]
fn parallel_and_serial_report_same_findings() {
    let case = repo();
    let mods = (0..12).map(|i| format!("m{i}.py")).collect::<Vec<_>>();
    for module_name in &mods {
        case.write(module_name, "VALUE = 1\n");
    }
    Python::attach(|py| {
        let _drivers = fixed_drivers(py, &["test_lane.py"]);
        let _index = no_index(py);
        let order = mods.clone();
        let callback =
            PyCFunction::new_closure(py, None, None, move |args, kwargs| -> PyResult<Py<PyAny>> {
                check_args(args, kwargs, 5)?;
                let py = args.py();
                let module_name: String = args.get_item(0)?.extract()?;
                let position = order
                    .iter()
                    .position(|item| item == &module_name)
                    .ok_or_else(|| PyAssertionError::new_err("unexpected module"))?;
                py.import("time")?
                    .getattr("sleep")?
                    .call1((0.02 * (order.len() - position) as f64,))?;
                let mut row = basic("NO_DIFFERENCE_OBSERVED");
                row["qualname"] = json!(module_name);
                py_result_json(py, &json!([row]))
            })
            .unwrap();
        let _probe = AttrPatch::replace(gate(py).as_any(), "probe", callback.as_any());
        let mods_ref = mods.iter().map(String::as_str).collect::<Vec<_>>();
        let (_, serial) = call_run(py, case.root(), &mods_ref, Some(1));
        let (_, parallel) = call_run(py, case.root(), &mods_ref, Some(6));
        let names = parallel["advisory"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["qualname"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(names, mods_ref);
        let serial_names = serial["advisory"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["qualname"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(serial_names, names);
    });
}

#[test]
fn concurrent_probes_never_share_report_path() {
    let case = repo();
    Python::attach(|py| {
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let observed = Arc::clone(&seen);
        let callback = PyCFunction::new_closure(
            py,
            None,
            None,
            move |args, _kwargs| -> PyResult<Py<PyAny>> {
                let py = args.py();
                let argv: Vec<String> = args.get_item(0)?.extract()?;
                let at = argv
                    .iter()
                    .position(|part| part == "--json")
                    .ok_or_else(|| PyAssertionError::new_err("missing --json"))?;
                observed.lock().unwrap().push(argv[at + 1].clone());
                Ok(py
                    .import("subprocess")?
                    .getattr("CompletedProcess")?
                    .call1((args.get_item(0)?, 0, "", ""))?
                    .unbind())
            },
        )
        .unwrap();
        let _run = AttrPatch::replace(
            &gate(py).getattr("subprocess").unwrap(),
            "run",
            callback.as_any(),
        );
        for module_name in ["a.py", "b.py", "c.py"] {
            probe(py, case.root(), module_name);
        }
        let paths = seen.lock().unwrap();
        assert_eq!(paths.iter().collect::<HashSet<_>>().len(), paths.len());
        assert!(fs::read_dir(case.root()).unwrap().all(|entry| !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".slop_gate")));
    });
}

#[test]
fn more_than_one_module_is_probed_at_a_time() {
    let case = repo();
    let mods = (0..8).map(|i| format!("m{i}.py")).collect::<Vec<_>>();
    Python::attach(|py| {
        let _drivers = fixed_drivers(py, &["test_lane.py"]);
        let _index = no_index(py);
        let activity = Arc::new(Mutex::new((0usize, 0usize)));
        let observed = Arc::clone(&activity);
        let callback =
            PyCFunction::new_closure(py, None, None, move |args, kwargs| -> PyResult<Py<PyAny>> {
                check_args(args, kwargs, 5)?;
                let py = args.py();
                {
                    let mut state = observed.lock().unwrap();
                    state.0 += 1;
                    state.1 = state.1.max(state.0);
                }
                py.import("time")?.getattr("sleep")?.call1((0.05,))?;
                observed.lock().unwrap().0 -= 1;
                py_result_json(py, &json!([]))
            })
            .unwrap();
        let _probe = AttrPatch::replace(gate(py).as_any(), "probe", callback.as_any());
        let mods_ref = mods.iter().map(String::as_str).collect::<Vec<_>>();
        call_run(py, case.root(), &mods_ref, Some(4));
        assert!(
            activity.lock().unwrap().1 > 1,
            "modules were probed one at a time"
        );
    });
}

#[test]
fn nonsense_job_count_is_refused_not_clamped() {
    let case = repo();
    Python::attach(|py| {
        let _drivers = fixed_drivers(py, &["test_lane.py"]);
        let _index = no_index(py);
        let _probe = fixed_probe(py, json!([]));
        let kwargs = PyDict::new(py);
        kwargs.set_item("only", vec!["lane.py"]).unwrap();
        kwargs.set_item("jobs", 0).unwrap();
        let error = gate(py)
            .getattr("run")
            .unwrap()
            .call(("HEAD", path(py, case.root())), Some(&kwargs))
            .unwrap_err();
        assert_error(
            py,
            error,
            &module(py, "builtins").getattr("ValueError").unwrap(),
            "--jobs must be at least 1",
        );
    });
}

#[test]
fn default_worker_count_follows_machine() {
    let _case = Case::new();
    Python::attach(|py| {
        let body = gate(py);
        let os = body.getattr("os").unwrap();
        for (cores, pending, expected) in [(32, 100, 8), (8, 100, 2), (8, 1, 1), (2, 100, 1)] {
            let cpu =
                PyCFunction::new_closure(py, None, None, move |args, kwargs| -> PyResult<i32> {
                    check_args(args, kwargs, 0)?;
                    Ok(cores)
                })
                .unwrap();
            let _cpu = AttrPatch::replace(&os, "cpu_count", cpu.as_any());
            let count: i32 = body
                .getattr("_worker_count")
                .unwrap()
                .call1((py.None(), pending))
                .unwrap()
                .extract()
                .unwrap();
            assert_eq!(count, expected);
        }
        assert_eq!(
            body.getattr("MAX_AUTO_JOBS")
                .unwrap()
                .extract::<i32>()
                .unwrap(),
            8
        );
    });
}

#[test]
fn timed_out_probe_is_finding_not_clean_sweep() {
    let case = repo();
    Python::attach(|py| {
        let callback = PyCFunction::new_closure(
            py,
            None,
            None,
            move |args, _kwargs| -> PyResult<Py<PyAny>> {
                let py = args.py();
                let error = py
                    .import("subprocess")?
                    .getattr("TimeoutExpired")?
                    .call1((args.get_item(0)?, gate(py).getattr("PER_MODULE_TIMEOUT")?))?;
                error.setattr(
                    "stderr",
                    pyo3::types::PyBytes::new(py, b"killed mid-import"),
                )?;
                Err(PyErr::from_value(error))
            },
        )
        .unwrap();
        let _run = AttrPatch::replace(
            &gate(py).getattr("subprocess").unwrap(),
            "run",
            callback.as_any(),
        );
        let findings = probe(py, case.root(), "lane.py");
        let row = &findings[0];
        assert_eq!(row["verdict"], "TIMEOUT");
        assert!(row["stderr_tail"]
            .as_str()
            .unwrap()
            .contains("killed mid-import"));
        assert!(row["stderr_tail"].is_string());
        let _drivers = fixed_drivers(py, &["test_lane.py"]);
        let _index = no_index(py);
        let (code, summary) = call_run(py, case.root(), &["lane.py"], None);
        assert_eq!(code, 0);
        assert_eq!(verdicts(&summary, "incomplete"), ["TIMEOUT"]);
        assert_eq!(summary["modules_probed"], 0);
        serde_json::to_string(&summary).unwrap();
    });
}

#[test]
fn crashed_probe_is_finding_not_clean_sweep() {
    let case = repo();
    Python::attach(|py| {
        let _run = probe_stub(py, 1, "ModuleNotFoundError: no module named 'lane'", None);
        let findings = probe(py, case.root(), "lane.py");
        assert_eq!(findings[0]["verdict"], "PROBE_FAILED");
        assert!(findings[0]["stderr_tail"]
            .as_str()
            .unwrap()
            .contains("ModuleNotFoundError"));
        assert_eq!(findings[0]["rule"], "probe-exit");
    });
}

#[test]
fn probe_that_wrote_no_report_is_finding() {
    let case = repo();
    Python::attach(|py| {
        let _run = probe_stub(py, 0, "", None);
        assert_eq!(
            probe(py, case.root(), "lane.py")[0]["verdict"],
            "PROBE_FAILED"
        );
    });
}

#[test]
fn unparseable_report_is_finding() {
    let case = repo();
    Python::attach(|py| {
        let _run = probe_stub(py, 0, "", Some("[{\"qual"));
        assert_eq!(
            probe(py, case.root(), "lane.py")[0]["verdict"],
            "PROBE_FAILED"
        );
    });
}

#[test]
fn completed_probe_still_returns_findings() {
    let case = repo();
    Python::attach(|py| {
        let identity =
            PyCFunction::new_closure(py, None, None, move |args, kwargs| -> PyResult<Py<PyAny>> {
                check_args(args, kwargs, 3)?;
                Ok(args.get_item(0)?.unbind())
            })
            .unwrap();
        let _refine =
            AttrPatch::replace(gate(py).as_any(), "refine_unexercised", identity.as_any());
        let _run = probe_stub(
            py,
            0,
            "",
            Some(&json!([basic("NO_DIFFERENCE_OBSERVED")]).to_string()),
        );
        assert_eq!(
            probe(py, case.root(), "lane.py")[0]["verdict"],
            "NO_DIFFERENCE_OBSERVED"
        );
    });
}

#[test]
fn probe_workdir_is_removed_on_every_exit() {
    let case = repo();
    Python::attach(|py| {
        let identity =
            PyCFunction::new_closure(py, None, None, move |args, kwargs| -> PyResult<Py<PyAny>> {
                check_args(args, kwargs, 3)?;
                Ok(args.get_item(0)?.unbind())
            })
            .unwrap();
        let _refine =
            AttrPatch::replace(gate(py).as_any(), "refine_unexercised", identity.as_any());
        for (code, stderr, report) in [
            (1, "boom", None),
            (0, "", None),
            (0, "", Some("[{\"qual")),
            (0, "", Some("[]")),
        ] {
            let _run = probe_stub(py, code, stderr, report);
            probe(py, case.root(), "lane.py");
        }
        assert!(fs::read_dir(case.root()).unwrap().all(|entry| !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".slop_gate-")));
    });
}
