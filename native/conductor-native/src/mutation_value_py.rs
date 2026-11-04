//! PyO3 bindings over the pure value analysis in `mutation_value`.

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use serde_json::Value;

use crate::mutation_value::{analyze, analyze_reports, load_spec, VALUE_SCHEMA};
use crate::mutation_value_admission::python_repr;
use crate::mutation_value_inputs::{self as inputs, JunitAdapter};

fn value_error(message: impl Into<String>) -> PyErr {
    PyValueError::new_err(message.into())
}

fn parse_json(text: &str, label: &str) -> PyResult<Value> {
    serde_json::from_str(text).map_err(|error| value_error(format!("invalid {label}: {error}")))
}

#[pyfunction]
fn load_value_analysis_native(
    value_json: &str,
    mutation_pairs_json: &str,
    ranked_nodeids: Vec<String>,
    mutation_ids: Vec<String>,
    source_paths: Vec<String>,
) -> PyResult<Option<String>> {
    load_spec(
        value_json,
        mutation_pairs_json,
        &ranked_nodeids,
        &mutation_ids,
        &source_paths,
    )
    .map(|spec| spec.map(|value| serde_json::to_string(&value).expect("spec is serializable")))
    .map_err(value_error)
}

#[pyfunction]
fn analyze_test_value_native(
    spec_json: &str,
    baseline_reports_json: &str,
    mutant_evidence_json: &str,
) -> PyResult<String> {
    let spec = serde_json::from_str(spec_json)
        .map_err(|error| value_error(format!("invalid value spec: {error}")))?;
    let baseline_reports = serde_json::from_str(baseline_reports_json)
        .map_err(|error| value_error(format!("invalid baseline reports: {error}")))?;
    let mutant_evidence = serde_json::from_str(mutant_evidence_json)
        .map_err(|error| value_error(format!("invalid mutant evidence: {error}")))?;
    serde_json::to_string(&analyze(spec, baseline_reports, mutant_evidence))
        .map_err(|error| value_error(error.to_string()))
}

#[pyfunction]
fn analyze_test_value_reports_native(
    spec_json: &str,
    baseline_reports_json: &str,
    mutant_reports_json: &str,
    mutant_outcomes_json: &str,
) -> PyResult<String> {
    let spec = parse_json(spec_json, "value spec")?;
    let baselines = serde_json::from_str(baseline_reports_json)
        .map_err(|error| value_error(format!("invalid baseline reports: {error}")))?;
    let reports = parse_json(mutant_reports_json, "mutant reports")?;
    let outcomes = parse_json(mutant_outcomes_json, "mutant outcomes")?;
    analyze_reports(spec, baselines, &reports, &outcomes).map_err(value_error)
}

#[pyfunction]
fn admission_errors_native(
    value_json: &str,
    required_nodeids: Vec<String>,
) -> PyResult<Vec<String>> {
    let value = parse_json(value_json, "test_value evidence")?;
    crate::mutation_value_admission::admission_errors(&value, &required_nodeids)
        .map_err(value_error)
}

#[pyfunction]
fn test_value_receipt_errors_native(
    value_json: &str,
    expected_nodeids: Vec<String>,
    expected_repetitions: i64,
) -> PyResult<Vec<String>> {
    let value = parse_json(value_json, "test_value evidence")?;
    let Some(payload) = value.as_object() else {
        return Ok(vec!["test_value evidence is missing".to_owned()]);
    };
    let mut errors = Vec::new();
    if payload.get("schema_version").and_then(Value::as_str) != Some(VALUE_SCHEMA) {
        errors.push("test_value schema is not current".to_owned());
    }
    if payload.get("status").and_then(Value::as_str) != Some("PASS") {
        errors.push(format!(
            "test_value status={}",
            python_repr(payload.get("status"))
        ));
    }
    let actual_nodeids = payload
        .get("tests")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(Value::as_object)
                .map(|row| row.get("nodeid").cloned().unwrap_or(Value::Null))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let expected = expected_nodeids
        .into_iter()
        .map(Value::String)
        .collect::<Vec<_>>();
    if actual_nodeids != expected {
        errors.push("test_value nodeids do not match ranked tests".to_owned());
    }
    if payload.get("baseline_repetitions").and_then(Value::as_i64) != Some(expected_repetitions) {
        errors.push("test_value baseline repetitions mismatch".to_owned());
    }
    Ok(errors)
}

#[pyfunction]
fn mutation_value_pytest_identity_native(nodeid: &str) -> PyResult<(String, String)> {
    inputs::pytest_identity(nodeid).map_err(value_error)
}

#[pyfunction]
fn mutation_value_cargo_identity_native(nodeid: &str) -> PyResult<String> {
    inputs::cargo_identity(nodeid).map_err(value_error)
}

#[pyfunction]
fn mutation_value_ctest_identity_native(nodeid: &str) -> PyResult<String> {
    inputs::ctest_identity(nodeid).map_err(value_error)
}

#[pyfunction]
fn mutation_value_attribution_supported_native(adapter: &str, ranked: Vec<String>) -> bool {
    inputs::attribution_supported(adapter, &ranked)
}

#[pyfunction]
fn mutation_value_parse_junit_native(
    path: &str,
    adapter: &str,
    ranked: Vec<String>,
) -> PyResult<String> {
    let adapter = JunitAdapter::from_name(adapter).map_err(value_error)?;
    let result = inputs::parse_junit_file(std::path::Path::new(path), adapter, &ranked)
        .map_err(value_error)?;
    serde_json::to_string(&result).map_err(|error| value_error(error.to_string()))
}

#[pyfunction]
#[pyo3(signature = (stdout, ranked, repo_root=None, argv=None))]
fn mutation_value_parse_cargo_native(
    stdout: &str,
    ranked: Vec<String>,
    repo_root: Option<&str>,
    argv: Option<Vec<String>>,
) -> PyResult<String> {
    let result = match (repo_root, argv) {
        (Some(root), Some(argv)) => {
            inputs::parse_bound_cargo_libtest(stdout, &ranked, std::path::Path::new(root), &argv)
        }
        (None, None) => inputs::parse_cargo_libtest(stdout, &ranked),
        _ => Err("Cargo attribution requires both root and argv".into()),
    }
    .map_err(value_error)?;
    serde_json::to_string(&result).map_err(|error| value_error(error.to_string()))
}

pub(crate) fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(load_value_analysis_native, module)?)?;
    module.add_function(wrap_pyfunction!(analyze_test_value_native, module)?)?;
    module.add_function(wrap_pyfunction!(analyze_test_value_reports_native, module)?)?;
    module.add_function(wrap_pyfunction!(admission_errors_native, module)?)?;
    module.add_function(wrap_pyfunction!(test_value_receipt_errors_native, module)?)?;
    module.add_function(wrap_pyfunction!(
        mutation_value_pytest_identity_native,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(
        mutation_value_cargo_identity_native,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(
        mutation_value_ctest_identity_native,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(
        mutation_value_attribution_supported_native,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(mutation_value_parse_junit_native, module)?)?;
    module.add_function(wrap_pyfunction!(mutation_value_parse_cargo_native, module)?)?;
    Ok(())
}
