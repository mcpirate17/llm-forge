//! Rust fixtures for the Python mutation-attribution contract.
//! The callback grades the actual source bytes on disk; Python supplies only
//! production behavior through PyO3. No Python test helpers are imported.

use pyo3::prelude::*;
use pyo3::types::{PyAny, PyCFunction, PyDict, PyList, PyModule, PyString, PyTuple};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use super::support::{module, path};

pub const SOURCE: &str = "def add(a, b):\n    return a + b\n\n\ndef mul(a, b):\n    return a * b\n";
pub const MODULE: &str = "pkg/calc.py";
pub const TESTS: &str = "pkg/test_calc.py";
pub const ADD: &str = "pkg/test_calc.py::test_add";
pub const MUL: &str = "pkg/test_calc.py::test_mul";

#[derive(Clone, Default)]
pub struct RunState {
    pub commands: Vec<Vec<String>>,
    pub environments: Vec<Value>,
    pub reports: Vec<PathBuf>,
    pub timeouts: Vec<i32>,
}

#[derive(Clone)]
pub enum Behavior {
    Ordinary,
    AbortOn(&'static str),
    HangOn(&'static str, f64),
}

pub struct Runner {
    pub state: Arc<Mutex<RunState>>,
    pub callback: Py<PyAny>,
}

pub fn json_to_py<'py>(py: Python<'py>, value: &Value) -> Bound<'py, PyAny> {
    module(py, "json")
        .getattr("loads")
        .unwrap()
        .call1((value.to_string(),))
        .unwrap()
}

pub fn py_to_json(value: &Bound<'_, PyAny>) -> Value {
    let py = value.py();
    let encoded: String = module(py, "json")
        .getattr("dumps")
        .unwrap()
        .call1((value,))
        .unwrap()
        .extract()
        .unwrap();
    serde_json::from_str(&encoded).unwrap()
}

pub fn fixture(root: &Path) {
    fs::create_dir_all(root.join("pkg")).unwrap();
    fs::write(root.join(MODULE), SOURCE).unwrap();
    fs::write(root.join(TESTS), "# graded by the Rust runner\n").unwrap();
}

pub fn mutant(
    id: &str,
    original: &str,
    replacement: &str,
    outcome: &str,
    covering: &[&str],
) -> Value {
    let offset = SOURCE.find(original).expect("fixture mutation span");
    json!({
        "id": id,
        "outcome": outcome,
        "path": MODULE,
        "line": SOURCE[..offset].bytes().filter(|byte| *byte == b'\n').count() + 1,
        "byte_offset": offset,
        "byte_length": original.len(),
        "operator": "binary_operator",
        "original_text": original,
        "mutated_text": replacement,
        "tests_run": covering,
        "duration_seconds": 0.01
    })
}

pub fn killed(id: &str, original: &str, replacement: &str) -> Value {
    mutant(id, original, replacement, "KILLED", &[ADD, MUL])
}

fn junit(path: &Path, command: &[String], source: &str) {
    let mut cases = Vec::new();
    for (nodeid, needed) in [(ADD, "a + b"), (MUL, "a * b")] {
        if command.iter().any(|arg| arg == nodeid) {
            let failure = if source.contains(needed) {
                ""
            } else {
                "<failure>assert</failure>"
            };
            let name = nodeid.rsplit("::").next().unwrap();
            cases.push(format!(
                "<testcase classname=\"pkg.test_calc\" name=\"{name}\" time=\"0.01\">{failure}</testcase>"
            ));
        }
    }
    let xml = format!(
        "<testsuites><testsuite name=\"pytest\" tests=\"{}\">{}</testsuite></testsuites>",
        cases.len(),
        cases.join("")
    );
    fs::write(path, xml).unwrap();
}

pub fn runner(py: Python<'_>, root: &Path, behavior: Behavior) -> Runner {
    let state = Arc::new(Mutex::new(RunState::default()));
    let observed = Arc::clone(&state);
    let target = root.join(MODULE);
    let callback =
        PyCFunction::new_closure(py, None, None, move |args, kwargs| -> PyResult<Py<PyAny>> {
            let py = args.py();
            let command: Vec<String> = args.get_item(0)?.extract()?;
            let kwargs = kwargs.expect("attribute supplies runner keyword arguments");
            let timeout: i32 = kwargs.get_item("timeout_seconds")?.unwrap().extract()?;
            let environment = py_to_json(&kwargs.get_item("environment")?.unwrap());
            let report = command
                .iter()
                .find_map(|arg| arg.strip_prefix("--junitxml="))
                .map(PathBuf::from)
                .expect("runner command has JUnit path");
            let source = fs::read_to_string(&target).expect("read live mutation fixture");
            {
                let mut record = observed.lock().unwrap();
                record.commands.push(command.clone());
                record.environments.push(environment);
                record.reports.push(report.clone());
                record.timeouts.push(timeout);
            }
            let abort =
                matches!(&behavior, Behavior::AbortOn(fragment) if source.contains(fragment));
            let hang =
                matches!(&behavior, Behavior::HangOn(fragment, _) if source.contains(fragment));
            if !abort && !hang {
                junit(&report, &command, &source);
            }
            let failed = command.iter().any(|arg| {
                (arg == ADD && !source.contains("a + b"))
                    || (arg == MUL && !source.contains("a * b"))
            });
            let returncode = if abort {
                Some(-6)
            } else if hang {
                None
            } else {
                Some(if failed { 1 } else { 0 })
            };
            let duration = match &behavior {
                Behavior::HangOn(_, baseline) if !hang => *baseline,
                _ if hang => f64::from(timeout),
                _ => 0.01,
            };
            let result = module(py, "conductor.mutation_campaign_model")
                .getattr("CommandResult")?
                .call1((
                    returncode,
                    hang,
                    duration,
                    "",
                    if abort { "Aborted" } else { "" },
                ))?;
            let pair = PyTuple::new(py, [result.into_any(), PyString::new(py, "").into_any()])?;
            Ok(pair.into_any().unbind())
        })
        .unwrap();
    Runner {
        state,
        callback: callback.into_any().unbind(),
    }
}

pub fn campaign<'py>(py: Python<'py>, timeout: i32) -> Bound<'py, PyAny> {
    let kwargs = PyDict::new(py);
    kwargs.set_item("campaign_id", "attribution-demo").unwrap();
    kwargs.set_item("run_timeout_seconds", timeout).unwrap();
    kwargs
        .set_item("test_argv", ("python", "-m", "pytest", "-q", TESTS))
        .unwrap();
    module(py, "types")
        .getattr("SimpleNamespace")
        .unwrap()
        .call((), Some(&kwargs))
        .unwrap()
}

pub fn attribute<'py>(
    py: Python<'py>,
    root: &Path,
    rows: &[Value],
    runner: &Runner,
    timeout: i32,
    environment: Value,
    progress: Option<&Bound<'py, PyAny>>,
) -> PyResult<Bound<'py, PyAny>> {
    let receipt = PyDict::new(py);
    receipt.set_item("mutants", json_to_py(py, &json!(rows)))?;
    receipt.set_item("test_value", py.None())?;
    let kwargs = PyDict::new(py);
    kwargs.set_item("worktree", path(py, root))?;
    kwargs.set_item("environment", json_to_py(py, &environment))?;
    let interpreter = module(py, "sys").getattr("executable")?;
    kwargs.set_item("interpreter", interpreter)?;
    kwargs.set_item("run", runner.callback.bind(py))?;
    if let Some(progress) = progress {
        kwargs.set_item("progress", progress)?;
    }
    module(py, "conductor.mutation_attribution")
        .getattr("attribute")?
        .call((campaign(py, timeout), &receipt), Some(&kwargs))?;
    Ok(receipt.into_any())
}

pub fn call<'py>(
    py: Python<'py>,
    name: &str,
    args: impl pyo3::call::PyCallArgs<'py>,
) -> Bound<'py, PyAny> {
    module(py, "conductor.mutation_attribution")
        .getattr(name)
        .unwrap()
        .call1(args)
        .unwrap()
}

pub fn lines(value: &Bound<'_, PyAny>) -> Vec<String> {
    value
        .call_method0("getvalue")
        .unwrap()
        .extract::<String>()
        .unwrap()
        .trim()
        .lines()
        .map(str::to_owned)
        .collect()
}

pub fn capture_stderr(py: Python<'_>) -> (super::support::AttrPatch, Bound<'_, PyAny>) {
    let stream = module(py, "io")
        .getattr("StringIO")
        .unwrap()
        .call0()
        .unwrap();
    let sys = PyModule::import(py, "sys").unwrap();
    let patch = super::support::AttrPatch::replace(sys.as_any(), "stderr", &stream);
    (patch, stream)
}

pub fn selected<'py>(
    py: Python<'py>,
    argv: &[&str],
    root: &Path,
    selected: &[&str],
    report: &Path,
) -> PyResult<Bound<'py, PyAny>> {
    module(py, "conductor.mutation_attribution")
        .getattr("_selection")?
        .call1((
            PyList::new(py, argv)?,
            path(py, root),
            PyList::new(py, selected)?,
            path(py, report),
        ))
}
