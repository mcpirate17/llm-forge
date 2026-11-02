use super::*;
use pyo3::{exceptions::PyValueError, prelude::*};
fn parse(text: &str) -> PyResult<Value> {
    crate::mutation_evidence::parse_request(text).map_err(PyValueError::new_err)
}
#[pyfunction]
fn validate_mutation_receipt_native(request_json: &str) -> PyResult<String> {
    let value = crate::mutation_evidence::validate_receipt(&parse(request_json)?)
        .map_err(PyValueError::new_err)?;
    serde_json::to_string(&value).map_err(|error| PyValueError::new_err(error.to_string()))
}
#[pyfunction]
fn mutation_runner_lineage_accepts_native(request_json: &str) -> PyResult<bool> {
    crate::mutation_evidence::runner_lineage_accepts(&parse(request_json)?)
        .map_err(PyValueError::new_err)
}
pub(crate) fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(validate_mutation_receipt_native, module)?)?;
    module.add_function(wrap_pyfunction!(
        mutation_runner_lineage_accepts_native,
        module
    )?)?;
    Ok(())
}
