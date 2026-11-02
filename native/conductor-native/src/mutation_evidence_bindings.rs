use super::*;
use pyo3::{exceptions::PyValueError, prelude::*};
fn call(text: &str, operation: fn(&Value) -> Result<Value, String>) -> PyResult<String> {
    let request = parse_request(text).map_err(PyValueError::new_err)?;
    let value = operation(&request).map_err(PyValueError::new_err)?;
    serde_json::to_string(&value).map_err(|error| PyValueError::new_err(error.to_string()))
}
#[pyfunction]
fn plan_mutation_evidence_native(request_json: &str) -> PyResult<String> {
    call(request_json, plan)
}
#[pyfunction]
fn verify_mutation_evidence_native(request_json: &str) -> PyResult<String> {
    call(request_json, verify)
}
pub(crate) fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    crate::mutation_manifest::register(module)?;
    crate::mutation_receipt::register(module)?;
    module.add_function(wrap_pyfunction!(plan_mutation_evidence_native, module)?)?;
    module.add_function(wrap_pyfunction!(verify_mutation_evidence_native, module)?)?;
    Ok(())
}
