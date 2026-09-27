//! Read-only project context decisions and strict configuration parsing.

#[path = "project_context_config.rs"]
mod config;
#[path = "project_context_decision.rs"]
mod decision;
#[path = "project_context_fs.rs"]
mod fs;

pub use config::parse_config;

pub fn evaluate(operation: &str, payload: &serde_json::Value) -> serde_json::Value {
    if operation == "read-config-bytes" {
        return fs::evaluate(payload);
    }
    decision::evaluate(operation, payload)
}

#[cfg(feature = "python")]
use pyo3::exceptions::PyValueError;
#[cfg(feature = "python")]
use pyo3::prelude::*;

#[cfg(feature = "python")]
#[pyfunction]
fn project_context_parse_config_native(raw: &[u8]) -> PyResult<String> {
    let parsed = parse_config(raw)
        .map_err(|error| PyValueError::new_err(format!("{}:{}", error.code, error.message)))?;
    serde_json::to_string(&parsed)
        .map_err(|error| PyValueError::new_err(format!("CONFIG_PARSE:{error}")))
}

#[cfg(feature = "python")]
#[pyfunction]
fn project_context_native(operation: &str, payload_json: &str) -> PyResult<String> {
    let payload: serde_json::Value = serde_json::from_str(payload_json)
        .map_err(|error| PyValueError::new_err(error.to_string()))?;
    serde_json::to_string(&evaluate(operation, &payload))
        .map_err(|error| PyValueError::new_err(error.to_string()))
}

#[cfg(feature = "python")]
pub fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(
        project_context_parse_config_native,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(project_context_native, module)?)?;
    Ok(())
}
