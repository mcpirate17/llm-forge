//! Native graph relationship and Markdown policy for `conductor.graph_context`.
//!
//! Python retains AST canonicalization and CLI presentation. This core is
//! callable without an interpreter and never opens a writable graph database.

#[path = "graph_context_markdown.rs"]
mod markdown;
#[path = "graph_context_relationships.rs"]
mod relationships;

#[cfg(feature = "python")]
use pyo3::exceptions::PyValueError;
#[cfg(feature = "python")]
use pyo3::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::Path;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GraphRelationship {
    pub qualified_name: String,
    pub kind: String,
    pub file_path: String,
}

#[derive(Debug, Serialize)]
pub struct GraphResult {
    pub callers: Vec<GraphRelationship>,
    pub callees: Vec<GraphRelationship>,
    pub status: String,
}

fn string_field<'a>(input: &'a Value, name: &str) -> Result<&'a str, String> {
    input
        .get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("graph context {name} must be a string"))
}

fn target_symbol(input: &Value) -> Result<Option<&str>, String> {
    match input.get("target_symbol") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if value.is_empty() => Ok(None),
        Some(Value::String(value)) => Ok(Some(value)),
        _ => Err("graph context target_symbol must be a string or null".to_owned()),
    }
}

pub fn dispatch(operation: &str, input: &Value) -> Result<Value, String> {
    match operation {
        "relationships" => serde_json::to_value(relationships::query(
            Path::new(string_field(input, "repo")?),
            string_field(input, "file_path")?,
            target_symbol(input)?,
        )?)
        .map_err(|error| error.to_string()),
        "syntactic_callers" => serde_json::to_value(relationships::syntactic_callers(
            Path::new(string_field(input, "repo")?),
            string_field(input, "symbol_name")?,
            string_field(input, "target_file_rel")?,
        )?)
        .map_err(|error| error.to_string()),
        "is_test_path" => Ok(json!(markdown::is_test_path(string_field(input, "path")?))),
        "markdown" => Ok(json!(markdown::format(input)?)),
        other => Err(format!("unknown graph context operation: {other}")),
    }
}

#[cfg(feature = "python")]
#[pyfunction]
fn graph_context_native(operation: &str, payload_json: &str) -> PyResult<String> {
    let input: Value = serde_json::from_str(payload_json)
        .map_err(|error| PyValueError::new_err(format!("invalid graph context input: {error}")))?;
    let result = dispatch(operation, &input).map_err(PyValueError::new_err)?;
    serde_json::to_string(&result).map_err(|error| PyValueError::new_err(error.to_string()))
}

#[cfg(feature = "python")]
pub fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(graph_context_native, module)?)?;
    Ok(())
}
