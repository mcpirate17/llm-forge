//! Deterministic candidate verification decisions. Git, files, subprocesses, and
//! the public Python dataclasses stay at the adapter boundary.

use serde_json::{json, Map, Value};

#[path = "candidate_verification_ast.rs"]
mod ast;
#[path = "candidate_verification_evidence.rs"]
mod evidence;
#[path = "candidate_verification_inventory.rs"]
mod inventory;
#[path = "candidate_verification_runtime.rs"]
mod runtime;
#[path = "candidate_verification_selection.rs"]
mod selection;

pub(crate) use ast::target_test_labels;

pub fn python_test_definitions(source: &str, path: &str) -> Result<Value, String> {
    ast::python_test_definitions(source, path)
}
pub fn native_rust_file_has_tests(source: &str) -> Result<bool, String> {
    ast::rust_has_tests(source)
}

pub fn decide(operation: &str, request: &Value) -> Result<Value, String> {
    match operation {
        "inventory_validate" => inventory::validate(request),
        "inventory_divergence" => inventory::divergence(request),
        "inventory_path_unsafe" => inventory::path_unsafe(request),
        "gated_nodeids" => selection::gated_nodeids(request),
        "native_source_role" => ast::native_source_role(
            std::path::Path::new(
                request["repo_root"]
                    .as_str()
                    .ok_or("repository root is required")?,
            ),
            request["path"]
                .as_str()
                .ok_or("native source path is required")?,
        ),
        "native_source_roles" => {
            let root = std::path::Path::new(
                request["repo_root"]
                    .as_str()
                    .ok_or("repository root is required")?,
            );
            let paths = request["paths"]
                .as_array()
                .ok_or("native source paths are required")?
                .iter()
                .map(|v| {
                    v.as_str()
                        .map(str::to_owned)
                        .ok_or("native source path must be a string")
                })
                .collect::<Result<Vec<_>, _>>()?;
            ast::native_source_roles(root, &paths)
        }
        "native_test_definitions" => ast::native_test_definitions(
            request["source"]
                .as_str()
                .ok_or("native test source is required")?,
            request["path"]
                .as_str()
                .ok_or("native test path is required")?,
            request["repo_root"].as_str().map(std::path::Path::new),
        ),
        "selection_plan" => selection::plan(request),
        "selection_decide" => selection::decide(request),
        "contract_runtime_plan" => runtime::plan(request),
        "waiver_states" => selection::waiver_states(request),
        "receipt_findings" => evidence::receipt_findings(request),
        "evidence_index" => evidence::index(request),
        "value_admission_plan" => evidence::admission_plan(request),
        "value_admission_findings" => evidence::admission_findings(request),
        "receipt_required_paths" => evidence::required_paths(request),
        "mutation_metrics" => evidence::metrics(request),
        other => Err(format!("unknown candidate verification operation: {other}")),
    }
}

pub(super) fn finding(
    check_id: &str,
    rule_id: &str,
    severity: &str,
    message: String,
    path: Option<&str>,
    help: Option<&str>,
    evidence: Option<Value>,
) -> Value {
    let mut row = Map::new();
    row.insert("check_id".into(), json!(check_id));
    row.insert("rule_id".into(), json!(rule_id));
    row.insert("severity".into(), json!(severity));
    row.insert("message".into(), json!(message));
    if let Some(path) = path {
        row.insert("path".into(), json!(path));
    }
    if let Some(help) = help {
        row.insert("help".into(), json!(help));
    }
    if let Some(evidence) = evidence {
        row.insert("evidence".into(), evidence);
    }
    Value::Object(row)
}

#[cfg(feature = "python")]
mod python {
    use super::*;
    use pyo3::exceptions::PyValueError;
    use pyo3::prelude::*;

    #[pyfunction]
    pub fn candidate_verification_ast_native(source: &str, path: &str) -> PyResult<String> {
        let value = python_test_definitions(source, path).map_err(PyValueError::new_err)?;
        serde_json::to_string(&value).map_err(|error| PyValueError::new_err(error.to_string()))
    }

    #[pyfunction]
    pub fn candidate_verification_native(operation: &str, request_json: &str) -> PyResult<String> {
        let request: Value = serde_json::from_str(request_json).map_err(|error| {
            PyValueError::new_err(format!("invalid verification request: {error}"))
        })?;
        let result = decide(operation, &request).map_err(PyValueError::new_err)?;
        serde_json::to_string(&result).map_err(|error| PyValueError::new_err(error.to_string()))
    }

    pub fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
        module.add_function(wrap_pyfunction!(candidate_verification_ast_native, module)?)?;
        module.add_function(wrap_pyfunction!(candidate_verification_native, module)?)?;
        Ok(())
    }
}

#[cfg(feature = "python")]
pub use python::register;

/// Extract native test definitions exclusively from a complete captured crate inventory.
pub fn native_test_definitions_captured(
    source: &str,
    path: &str,
    root: &std::path::Path,
    files: &std::collections::BTreeMap<std::path::PathBuf, Vec<u8>>,
) -> Result<serde_json::Value, String> {
    if !path.ends_with(".rs") {
        return ast::native_test_definitions(source, path, None);
    }
    let key = root.join(path);
    if files.get(&key).map(Vec::as_slice) != Some(source.as_bytes()) {
        return Err(format!("captured Rust source mismatch: {path}"));
    }
    ast::captured_rust_test_labels(root, std::path::Path::new(path), files)
        .map(serde_json::Value::Object)
}
