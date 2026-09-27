//! Native candidate policy rules; Python keeps file I/O and public dataclasses.

#[path = "candidate_policy_classify.rs"]
mod candidate_policy_classify;
#[path = "candidate_policy_parse.rs"]
mod candidate_policy_parse;
#[path = "candidate_policy_value.rs"]
mod candidate_policy_value;
#[path = "candidate_policy_waivers.rs"]
mod candidate_policy_waivers;

pub use candidate_policy_classify::classify;
pub(crate) use candidate_policy_classify::glob_match;
pub use candidate_policy_parse::{fragment, parse_policy};
pub use candidate_policy_waivers::parse_value_waivers;

#[cfg(feature = "python")]
use pyo3::exceptions::PyValueError;
#[cfg(feature = "python")]
use pyo3::prelude::*;
#[cfg(feature = "python")]
use serde_json::Value;

#[cfg(feature = "python")]
#[pyfunction]
fn candidate_policy_parse_native(raw_json: &str, today_iso: &str) -> PyResult<String> {
    let raw: Value =
        serde_json::from_str(raw_json).map_err(|e| PyValueError::new_err(e.to_string()))?;
    let policy = parse_policy(&raw, today_iso).map_err(PyValueError::new_err)?;
    serde_json::to_string(&policy).map_err(|e| PyValueError::new_err(e.to_string()))
}

#[cfg(feature = "python")]
#[pyfunction]
fn candidate_policy_classify_native(change_json: &str, globs_json: &str) -> PyResult<String> {
    let change: Value =
        serde_json::from_str(change_json).map_err(|e| PyValueError::new_err(e.to_string()))?;
    let globs: Value =
        serde_json::from_str(globs_json).map_err(|e| PyValueError::new_err(e.to_string()))?;
    let result = classify(&change, &globs).map_err(PyValueError::new_err)?;
    serde_json::to_string(&result).map_err(|e| PyValueError::new_err(e.to_string()))
}

#[cfg(feature = "python")]
#[pyfunction]
fn candidate_value_waivers_parse_native(raw_json: &str) -> PyResult<String> {
    let raw: Value =
        serde_json::from_str(raw_json).map_err(|e| PyValueError::new_err(e.to_string()))?;
    let waivers = parse_value_waivers(&raw).map_err(PyValueError::new_err)?;
    serde_json::to_string(&waivers).map_err(|e| PyValueError::new_err(e.to_string()))
}

#[cfg(feature = "python")]
#[pyfunction]
fn candidate_policy_fragment_native(
    operation: &str,
    raw_json: &str,
    today_iso: &str,
) -> PyResult<String> {
    let raw: Value =
        serde_json::from_str(raw_json).map_err(|e| PyValueError::new_err(e.to_string()))?;
    let result = fragment(operation, &raw, today_iso).map_err(PyValueError::new_err)?;
    serde_json::to_string(&result).map_err(|e| PyValueError::new_err(e.to_string()))
}

#[cfg(feature = "python")]
pub fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(candidate_policy_parse_native, module)?)?;
    module.add_function(wrap_pyfunction!(candidate_policy_classify_native, module)?)?;
    module.add_function(wrap_pyfunction!(
        candidate_value_waivers_parse_native,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(candidate_policy_fragment_native, module)?)?;
    Ok(())
}
