#![cfg(feature = "python-compat-tests")]
//! Rust-owned admission contracts for upstream invariant waivers.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::exceptions::PyAssertionError;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyCFunction, PyFrozenSet, PyString, PyTuple};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use support::{module, path, AttrPatch, Case};

const TORCH_VERSION: &str = "2.13.0+cu130";
const NODEID: &str = "research/tests/test_x.py::test_aten_sum_is_order_stable";
const SOURCE: &str = "research/synthesis/x.py";

#[derive(Clone)]
struct Measurement {
    label: &'static str,
    args: Vec<&'static str>,
    lines: Vec<(&'static str, i32)>,
    native: Vec<&'static str>,
    error: Option<&'static str>,
}

impl Measurement {
    fn import(lines: Vec<(&'static str, i32)>, error: Option<&'static str>) -> Self {
        Self {
            label: "import",
            args: vec!["--collect-only", "research/tests/test_x.py"],
            lines,
            native: vec![],
            error,
        }
    }

    fn call(lines: Vec<(&'static str, i32)>) -> Self {
        Self {
            label: "call",
            args: vec![NODEID],
            lines,
            native: vec![],
            error: None,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Outcome {
    nodeid: String,
    admitted: bool,
    reason: String,
}

fn declaration() -> Value {
    json!({
        "nodeid": NODEID,
        "justification": "pins ATen sum(dim=2) accumulation order",
        "pinned": {"torch": TORCH_VERSION}
    })
}

fn py_json<'py>(py: Python<'py>, value: Value) -> Bound<'py, PyAny> {
    module(py, "json")
        .getattr("loads")
        .unwrap()
        .call1((value.to_string(),))
        .unwrap()
}

fn stub_measure<'py>(
    py: Python<'py>,
    case: &Case,
    expected: Vec<Measurement>,
    timeout: bool,
) -> (Bound<'py, PyCFunction>, Arc<Mutex<Vec<String>>>) {
    let snapshot = case.root().join("snapshot");
    let runtime = case.root().join("runtime");
    let seen = Arc::new(Mutex::new(Vec::new()));
    let callback_seen = Arc::clone(&seen);
    let measurement =
        PyCFunction::new_closure(py, None, None, move |args, kwargs| -> PyResult<Py<PyAny>> {
            let py = args.py();
            if args.len() != 4 || kwargs.is_some_and(|kw| !kw.is_empty()) {
                return Err(PyAssertionError::new_err(
                    "_measure callback requires four positional arguments",
                ));
            }
            if !args.get_item(0)?.eq(path(py, &snapshot))?
                || !args.get_item(1)?.eq(path(py, &runtime))?
            {
                return Err(PyAssertionError::new_err(
                    "_measure received wrong scratch paths",
                ));
            }
            let label: String = args.get_item(2)?.extract()?;
            let pytest_args: Vec<String> = args.get_item(3)?.extract()?;
            let step = callback_seen.lock().unwrap().len();
            let expected_step = expected.get(step).ok_or_else(|| {
                PyAssertionError::new_err(format!("_measure must not run for label {label:?}"))
            })?;
            if label != expected_step.label || pytest_args != expected_step.args {
                return Err(PyAssertionError::new_err(format!(
                "_measure call {step}: actual ({label:?}, {pytest_args:?}), expected ({:?}, {:?})",
                expected_step.label, expected_step.args
            )));
            }
            callback_seen.lock().unwrap().push(label);
            if timeout {
                let error = py
                    .import("subprocess")?
                    .getattr("TimeoutExpired")?
                    .call1(("pytest", 300))?;
                return Err(PyErr::from_value(error));
            }
            let lines = PyFrozenSet::new(py, expected_step.lines.iter().copied())?;
            let native = PyFrozenSet::new(py, expected_step.native.iter().copied())?;
            let error: Py<PyAny> = match expected_step.error {
                Some(message) => PyString::new(py, message).into_any().unbind(),
                None => py.None(),
            };
            Ok(
                PyTuple::new(py, [lines.as_any(), native.as_any(), error.bind(py)])?
                    .into_any()
                    .unbind(),
            )
        })
        .unwrap();
    (measurement, seen)
}

fn evaluate_case(
    py: Python<'_>,
    case: &Case,
    declarations: Value,
    gated: Value,
    installed: Option<&str>,
    expected_calls: Vec<Measurement>,
    timeout: bool,
) -> (Vec<Outcome>, Vec<String>) {
    let body = module(py, "conductor.candidate_review.external_invariants");
    let version = installed.map(str::to_owned);
    let version_stub = PyCFunction::new_closure(
        py,
        None,
        None,
        move |args, kwargs| -> PyResult<Option<String>> {
            if !args.is_empty() || kwargs.is_some_and(|kw| !kw.is_empty()) {
                return Err(PyAssertionError::new_err(
                    "unexpected torch-version probe arguments",
                ));
            }
            Ok(version.clone())
        },
    )
    .unwrap();
    let _version = AttrPatch::replace(
        body.as_any(),
        "_installed_torch_version",
        version_stub.as_any(),
    );

    let expected_count = expected_calls.len();
    let (measurement, seen) = stub_measure(py, case, expected_calls, timeout);
    let _measure = AttrPatch::replace(body.as_any(), "_measure", measurement.as_any());

    let results = body
        .getattr("evaluate")
        .unwrap()
        .call1((
            path(py, &case.root().join("snapshot")),
            path(py, &case.root().join("runtime")),
            py_json(py, declarations),
            py_json(py, gated),
        ))
        .unwrap();
    let outcomes = results
        .try_iter()
        .unwrap()
        .map(|row| {
            let row = row.unwrap();
            Outcome {
                nodeid: row.getattr("nodeid").unwrap().extract().unwrap(),
                admitted: row.getattr("admitted").unwrap().extract().unwrap(),
                reason: row.getattr("reason").unwrap().extract().unwrap(),
            }
        })
        .collect();
    let calls = seen.lock().unwrap().clone();
    assert_eq!(calls.len(), expected_count, "missing _measure call");
    (outcomes, calls)
}

fn one(
    case: &Case,
    declaration: Value,
    installed: Option<&str>,
    calls: Vec<Measurement>,
    timeout: bool,
) -> Outcome {
    Python::attach(|py| {
        let (outcomes, _) = evaluate_case(
            py,
            case,
            json!([declaration]),
            json!([NODEID]),
            installed,
            calls,
            timeout,
        );
        assert_eq!(outcomes.len(), 1);
        outcomes.into_iter().next().unwrap()
    })
}

#[test]
fn admits_when_invariant_is_verified_clean() {
    let case = Case::new();
    let baseline = vec![(SOURCE, 10)];
    let outcome = one(
        &case,
        declaration(),
        Some(TORCH_VERSION),
        vec![
            Measurement::import(baseline.clone(), None),
            Measurement::call(baseline),
        ],
        false,
    );
    assert_eq!(outcome.nodeid, NODEID);
    assert!(outcome.admitted);
    assert!(outcome.reason.contains("torch pinned at"));
}

#[test]
fn refuses_when_call_touches_repo_source_beyond_import() {
    let case = Case::new();
    let outcome = one(
        &case,
        declaration(),
        Some(TORCH_VERSION),
        vec![
            Measurement::import(vec![(SOURCE, 10)], None),
            Measurement::call(vec![(SOURCE, 10), (SOURCE, 42)]),
        ],
        false,
    );
    assert!(!outcome.admitted);
    assert!(outcome
        .reason
        .contains("executes repository source beyond importing its module"));
}

#[test]
fn refuses_stale_torch_pin() {
    let case = Case::new();
    let mut declaration = declaration();
    declaration["pinned"]["torch"] = json!("2.12.0+cu126");
    let outcome = one(&case, declaration, Some(TORCH_VERSION), vec![], false);
    assert!(!outcome.admitted);
    assert!(outcome.reason.contains("2.12.0+cu126"));
    assert!(outcome.reason.contains(TORCH_VERSION));
    assert!(outcome
        .reason
        .contains("re-verify the invariant and re-pin"));
}

#[test]
fn refuses_empty_justification() {
    let case = Case::new();
    let mut declaration = declaration();
    declaration["justification"] = json!("");
    let outcome = one(&case, declaration, Some(TORCH_VERSION), vec![], false);
    assert!(!outcome.admitted);
    assert_eq!(outcome.reason, "the justification is empty");
}

#[test]
fn refuses_whitespace_only_justification() {
    let case = Case::new();
    let mut declaration = declaration();
    declaration["justification"] = json!("   \n\t  ");
    let outcome = one(&case, declaration, Some(TORCH_VERSION), vec![], false);
    assert!(!outcome.admitted);
    assert_eq!(outcome.reason, "the justification is empty");
}

#[test]
fn refuses_missing_pinned_mapping() {
    let case = Case::new();
    let outcome = one(
        &case,
        json!({"nodeid":NODEID,"justification":"why this holds"}),
        Some(TORCH_VERSION),
        vec![],
        false,
    );
    assert!(!outcome.admitted);
    assert_eq!(outcome.reason, "no pinned torch version is declared");
}

#[test]
fn refuses_pinned_without_torch_key() {
    let case = Case::new();
    let mut declaration = declaration();
    declaration["pinned"] = json!({"torch":213});
    let outcome = one(&case, declaration, Some(TORCH_VERSION), vec![], false);
    assert!(!outcome.admitted);
    assert_eq!(outcome.reason, "no pinned torch version is declared");
}

#[test]
fn ignores_declaration_not_in_gated_nodeids() {
    let case = Case::new();
    Python::attach(|py| {
        let mut declared = declaration();
        declared["nodeid"] = json!("research/tests/test_x.py::test_not_gated_this_run");
        let (outcomes, calls) = evaluate_case(
            py,
            &case,
            json!([declared]),
            json!([NODEID]),
            Some(TORCH_VERSION),
            vec![],
            false,
        );
        assert!(outcomes.is_empty());
        assert!(calls.is_empty());
    });
}

#[test]
fn ignores_declaration_with_non_string_nodeid() {
    let case = Case::new();
    Python::attach(|py| {
        let mut declared = declaration();
        declared["nodeid"] = json!(1234);
        let (outcomes, calls) = evaluate_case(
            py,
            &case,
            json!([declared]),
            json!([NODEID, 1234]),
            Some(TORCH_VERSION),
            vec![],
            false,
        );
        assert!(outcomes.is_empty());
        assert!(calls.is_empty());
    });
}

#[test]
fn refuses_when_a_native_artifact_is_mapped() {
    let case = Case::new();
    let mut call = Measurement::call(vec![]);
    call.native = vec!["compiled-cache::research_native_thing.so"];
    let outcome = one(
        &case,
        declaration(),
        Some(TORCH_VERSION),
        vec![Measurement::import(vec![], None), call],
        false,
    );
    assert!(!outcome.admitted);
    assert!(outcome.reason.contains("native extensions"));
}

#[test]
fn refuses_when_measurement_reports_explicit_error() {
    let case = Case::new();
    let outcome = one(
        &case,
        declaration(),
        Some(TORCH_VERSION),
        vec![Measurement::import(
            vec![],
            Some("import run failed under coverage: boom"),
        )],
        false,
    );
    assert!(!outcome.admitted);
    assert_eq!(outcome.reason, "import run failed under coverage: boom");
}

#[test]
fn refuses_when_measurement_raises_timeout() {
    let case = Case::new();
    let outcome = one(
        &case,
        declaration(),
        Some(TORCH_VERSION),
        vec![Measurement::import(vec![], None)],
        true,
    );
    assert!(!outcome.admitted);
    assert!(outcome.reason.contains("coverage could not be measured"));
}

#[test]
fn refuses_when_torch_is_not_installed() {
    let case = Case::new();
    let outcome = one(&case, declaration(), None, vec![], false);
    assert!(!outcome.admitted);
    assert_eq!(
        outcome.reason,
        "torch is not installed, so the pin cannot be checked"
    );
}
