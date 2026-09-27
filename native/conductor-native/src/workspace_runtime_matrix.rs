//! Deterministic policy for the workspace runtime receipt.
//!
//! The Python entry point retains live process, HTTP, hook and model calls. This
//! module makes the decisions from their captured evidence without performing
//! any of those calls.

#[path = "workspace_runtime_matrix_checks.rs"]
mod checks;
#[path = "workspace_runtime_matrix_clerk.rs"]
mod clerk;
#[path = "workspace_runtime_matrix_receipt.rs"]
mod receipt;

#[cfg(feature = "python")]
use pyo3::exceptions::PyValueError;
#[cfg(feature = "python")]
use pyo3::prelude::*;
use serde_json::Value;

pub fn dispatch(operation: &str, input: &Value) -> Result<Value, String> {
    match operation {
        "aggregate_status" => receipt::aggregate_status(input),
        "extract_reported_tokens" => receipt::extract_reported_tokens(input),
        "replace_receipt_cells" => receipt::replace_receipt_cells(input),
        "check_hook_configs" => checks::check_hook_configs(input),
        "hook_program_cases" => checks::hook_program_cases(input),
        "hook_program_verdict" => checks::hook_program_verdict(input),
        "check_graph_evidence" => checks::check_graph_evidence(input),
        "check_embedding" => checks::check_embedding(input),
        "check_retrievers" => checks::check_retrievers(input),
        "check_active_state" => checks::check_active_state(input),
        "check_launchers" => checks::check_launchers(input),
        "parse_gpu_processes" => clerk::parse_gpu_processes(input),
        "gpu_preflight" => clerk::gpu_preflight(input),
        "ollama_model_rows" => clerk::ollama_model_rows(input),
        "nonnegative_int" => clerk::nonnegative_int(input),
        "clerk_schema" => clerk::clerk_schema(input),
        "clerk_payload" => clerk::clerk_payload(input),
        "clerk_unavailable" => clerk::clerk_unavailable(input),
        "clerk_adjudicate" => clerk::clerk_adjudicate(input),
        other => Err(format!(
            "unknown workspace runtime matrix operation: {other}"
        )),
    }
}

#[cfg(feature = "python")]
#[pyfunction]
fn workspace_runtime_matrix_native(operation: &str, payload_json: &str) -> PyResult<String> {
    let input: Value = serde_json::from_str(payload_json)
        .map_err(|error| PyValueError::new_err(format!("invalid matrix input: {error}")))?;
    let output = dispatch(operation, &input).map_err(PyValueError::new_err)?;
    serde_json::to_string(&output).map_err(|error| PyValueError::new_err(error.to_string()))
}

#[cfg(feature = "python")]
pub fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(workspace_runtime_matrix_native, module)?)?;
    Ok(())
}

#[cfg(test)]
#[path = "workspace_runtime_matrix_tests.rs"]
mod tests;
