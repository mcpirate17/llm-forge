//! Rust-owned Elements report and pinned Mull campaign fixtures.

use crate::comm_support::{bind_signature, signature};
use crate::support::{module, path, AttrPatch, Case};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyCFunction, PyDict, PyList, PyModule, PyString, PyTuple};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub const SOURCE: &str = "aria_core/src/cpu/norm.cpp";
pub const SOURCE_TEXT: &str =
    "void f(int n) {\n  for (int i = 0; i < n; ++i) {\n    g(i * 2);\n  }\n}\n";

pub fn repo_src() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../src")
        .canonicalize()
        .unwrap()
}

pub fn manifest() -> PathBuf {
    repo_src().join("conductor/testdata/mull/claude_aria_kernels_fixture.json")
}

pub fn mull(py: Python<'_>) -> Bound<'_, PyModule> {
    module(py, "conductor.mutation_engine_mull")
}

pub fn generated(py: Python<'_>) -> Bound<'_, PyModule> {
    module(py, "conductor.mutation_engine_generated")
}

pub fn campaign<'py>(py: Python<'py>) -> Bound<'py, PyAny> {
    generated(py)
        .getattr("load_generated_campaign")
        .unwrap()
        .call1((path(py, &manifest()),))
        .unwrap()
}

pub fn py_json<'py>(py: Python<'py>, value: Value) -> Bound<'py, PyAny> {
    module(py, "json")
        .getattr("loads")
        .unwrap()
        .call1((value.to_string(),))
        .unwrap()
}

pub fn mutant(status: &str, line: i64, column: i64) -> Value {
    mutant_with("cxx_lt_to_le", "<=", status, line, column, None, 13, None)
}

#[allow(clippy::too_many_arguments)]
pub fn mutant_with(
    mutator: &str,
    replacement: &str,
    status: &str,
    line: i64,
    column: i64,
    end_line: Option<i64>,
    end_column: i64,
    identifier: Option<&str>,
) -> Value {
    json!({
        "id":identifier.map(str::to_owned).unwrap_or_else(||format!("{mutator}:{SOURCE}:{line}:{column}")),
        "mutatorName":mutator,
        "replacement":replacement,
        "location":{"start":{"line":line,"column":column},"end":{"line":end_line.unwrap_or(line),"column":end_column}},
        "status":status
    })
}

pub fn report(mutants: Vec<Value>, relative: &str, source: &str) -> Value {
    let file = repo_src().join(relative);
    json!({"files":{file.to_str().unwrap():{"language":"cpp","source":source,"mutants":mutants}}})
}

pub fn default_report(mutants: Vec<Value>) -> Value {
    report(mutants, SOURCE, SOURCE_TEXT)
}

pub fn rows<'py>(py: Python<'py>, report: Value) -> Bound<'py, PyList> {
    mull(py)
        .getattr("_rows")
        .unwrap()
        .call1((py_json(py, report), path(py, &repo_src())))
        .unwrap()
        .cast_into::<PyList>()
        .unwrap()
}

pub fn field<'py>(row: &Bound<'py, PyAny>, key: &str) -> Bound<'py, PyAny> {
    row.get_item(key).unwrap()
}

pub fn equal(actual: &Bound<'_, PyAny>, expected: &Bound<'_, PyAny>) {
    assert!(
        actual.eq(expected).unwrap(),
        "actual {actual:?}, expected {expected:?}"
    );
}

pub fn campaign_error(py: Python<'_>, error: PyErr, message: &str) {
    let class = module(py, "conductor.mutation_scope")
        .getattr("CampaignError")
        .unwrap();
    assert!(error.matches(py, &class).unwrap(), "{error}");
    assert!(error.to_string().contains(message), "{error}");
}

pub fn dict<'py>(py: Python<'py>) -> Bound<'py, PyDict> {
    PyDict::new(py)
}

pub fn result<'py>(py: Python<'py>, code: i32, stderr: &str) -> Bound<'py, PyAny> {
    generated(py)
        .getattr("CommandResult")
        .unwrap()
        .call1((code, false, 0.0, "", stderr))
        .unwrap()
}

pub fn signature_kwargs(py: Python<'_>, positional: &[&str]) -> Py<PyAny> {
    let inspect = module(py, "inspect");
    let parameter = inspect.getattr("Parameter").unwrap();
    let parameters = PyList::empty(py);
    for name in positional {
        parameters
            .append(
                parameter
                    .call1((*name, parameter.getattr("POSITIONAL_OR_KEYWORD").unwrap()))
                    .unwrap(),
            )
            .unwrap();
    }
    parameters
        .append(
            parameter
                .call1(("kwargs", parameter.getattr("VAR_KEYWORD").unwrap()))
                .unwrap(),
        )
        .unwrap();
    inspect
        .getattr("Signature")
        .unwrap()
        .call1((parameters,))
        .unwrap()
        .unbind()
}

pub type Calls = Arc<Mutex<Vec<Vec<String>>>>;

pub fn recorded_runs(py: Python<'_>, outcomes: Vec<(i32, String)>) -> (Calls, AttrPatch) {
    let calls: Calls = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&calls);
    let sig = signature(py, &["argv"], &["cwd", "timeout_seconds", "environment"]);
    let callback = PyCFunction::new_closure(py, None, None, move |args, kw| {
        let bound = bind_signature(&sig, args, kw)?;
        let argv = bound.getattr("arguments")?.get_item("argv")?;
        let values: Vec<String> = argv
            .try_iter()?
            .map(|item| item.unwrap().str().unwrap().extract().unwrap())
            .collect();
        let mut history = recorded.lock().unwrap();
        history.push(values);
        let (code, stderr) = outcomes
            .get(history.len() - 1)
            .map(|(code, stderr)| (*code, stderr.as_str()))
            .unwrap_or((0, ""));
        let py = args.py();
        Ok::<Py<PyTuple>, PyErr>(
            PyTuple::new(
                py,
                [
                    result(py, code, stderr).as_any(),
                    PyString::new(py, "").as_any(),
                ],
            )?
            .unbind(),
        )
    })
    .unwrap();
    let runner = generated(py);
    let patch = AttrPatch::replace(runner.as_any(), "run", callback.as_any());
    (calls, patch)
}

pub fn installed_plugin(py: Python<'_>, case: &Case) -> AttrPatch {
    let plugin = case.write("mull-ir-frontend-18", "");
    let sig = signature(py, &["version"], &[]);
    let callback = PyCFunction::new_closure(py, None, None, move |args, kw| {
        bind_signature(&sig, args, kw)?;
        Ok::<String, PyErr>(plugin.to_string_lossy().into_owned())
    })
    .unwrap();
    AttrPatch::replace(mull(py).as_any(), "_plugin", callback.as_any())
}

pub fn built_tree(py: Python<'_>, case: &Case, reports: bool) -> (PathBuf, Vec<String>) {
    let build = case.mkdir("build");
    let names: Vec<String> = mull(py)
        .getattr("_executables")
        .unwrap()
        .call1((campaign(py),))
        .unwrap()
        .extract()
        .unwrap();
    for name in &names {
        fs::write(build.join(name), "").unwrap();
    }
    if reports {
        fs::create_dir(build.join("mull-reports")).unwrap();
        for name in &names {
            fs::write(
                build.join("mull-reports").join(format!(
                    "{}.json",
                    Path::new(name).file_name().unwrap().to_string_lossy()
                )),
                default_report(vec![mutant("Killed", 2, 12)]).to_string(),
            )
            .unwrap();
        }
    }
    (build, names)
}

pub struct Drive<'py> {
    pub receipt: Bound<'py, PyDict>,
    pub calls: Calls,
    pub seen: Bound<'py, PyDict>,
    pub result: PyResult<Bound<'py, PyAny>>,
}

pub struct DriveInput {
    pub binary: &'static str,
    pub drifted: Value,
    pub baseline: i32,
    pub reports: Option<Vec<Value>>,
}

impl Default for DriveInput {
    fn default() -> Self {
        Self {
            binary: "/bin/mull-runner-18",
            drifted: json!({}),
            baseline: 0,
            reports: None,
        }
    }
}

pub fn drive_execute<'py>(
    py: Python<'py>,
    case: &Case,
    input: DriveInput,
    subject: Option<Bound<'py, PyAny>>,
) -> Drive<'py> {
    let seen = PyDict::new(py);
    let scope = module(py, "conductor.mutation_run_scope");
    let sig = signature(py, &["selected", "worktree"], &[]);
    let captured = seen.clone().unbind();
    let scope_path = case.root().join("mull.yml");
    let callback = PyCFunction::new_closure(py, None, None, move |args, kw| {
        let bound = bind_signature(&sig, args, kw)?;
        let values = bound.getattr("arguments")?;
        let seen = captured.bind(args.py());
        seen.set_item(
            "scope_source",
            values.get_item("selected")?.getattr("source")?,
        )?;
        seen.set_item("scope_worktree", values.get_item("worktree")?)?;
        Ok::<Py<PyAny>, PyErr>(path(args.py(), &scope_path).unbind())
    })
    .unwrap();
    let _scope = AttrPatch::replace(scope.as_any(), "mull_scope_config", callback.as_any());

    let sig = signature(py, &["campaign", "worktree"], &[]);
    let drift = input.drifted;
    let callback = PyCFunction::new_closure(py, None, None, move |args, kw| {
        bind_signature(&sig, args, kw)?;
        Ok::<Py<PyAny>, PyErr>(py_json(args.py(), drift.clone()).unbind())
    })
    .unwrap();
    let _drift = AttrPatch::replace(generated(py).as_any(), "drift", callback.as_any());

    let sig = signature(py, &["name", "version", "hint"], &[]);
    let callback = PyCFunction::new_closure(py, None, None, move |args, kw| {
        let values = bind_signature(&sig, args, kw)?.getattr("arguments")?;
        let name: String = values.get_item("name")?.extract()?;
        let version: String = values.get_item("version")?.extract()?;
        Ok::<String, PyErr>(format!("/bin/{name}-{version}"))
    })
    .unwrap();
    let _tool = AttrPatch::replace(mull(py).as_any(), "_tool", callback.as_any());

    let captured = seen.clone().unbind();
    let sig = signature_kwargs(py, &["campaign", "worktree", "build", "version"]);
    let callback = PyCFunction::new_closure(py, None, None, move |args, kw| {
        let values = bind_signature(&sig, args, kw)?.getattr("arguments")?;
        let kwargs = values.get_item("kwargs")?;
        let seen = captured.bind(args.py());
        seen.set_item("build", values.get_item("build")?)?;
        seen.set_item("environment", kwargs.get_item("environment")?)?;
        Ok::<(), PyErr>(())
    })
    .unwrap();
    let _build = AttrPatch::replace(mull(py).as_any(), "_build", callback.as_any());

    let captured = seen.clone().unbind();
    let reports = input.reports.unwrap_or_else(|| {
        vec![{
            let mut value = default_report(vec![mutant("Killed", 2, 12)]);
            value["config"] = json!({"mullVersion":"18.0.0"});
            value
        }]
    });
    let callback = PyCFunction::new_closure(py, None, None, move |args, kw| {
        let list = PyList::empty(args.py());
        for arg in args.iter() {
            list.append(arg)?;
        }
        if let Some(kw) = kw {
            for (_, value) in kw.iter() {
                list.append(value)?;
            }
        }
        captured.bind(args.py()).set_item("report_args", list)?;
        Ok::<Py<PyAny>, PyErr>(py_json(args.py(), json!(reports)).unbind())
    })
    .unwrap();
    let _reports = AttrPatch::replace(mull(py).as_any(), "_engine_reports", callback.as_any());
    let (calls, _run) = recorded_runs(py, vec![(input.baseline, String::new())]);
    let receipt = PyDict::new(py);
    let kw = PyDict::new(py);
    kw.set_item("binary", input.binary).unwrap();
    kw.set_item("worktree", path(py, &repo_src())).unwrap();
    kw.set_item("output_path", path(py, &case.root().join("receipt.json")))
        .unwrap();
    let result = mull(py).getattr("execute").unwrap().call(
        (subject.unwrap_or_else(|| campaign(py)), &receipt),
        Some(&kw),
    );
    Drive {
        receipt,
        calls,
        seen,
        result,
    }
}
