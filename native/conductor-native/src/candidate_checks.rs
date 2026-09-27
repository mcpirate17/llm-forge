//! Candidate-review built-in decisions. Python supplies snapshot I/O and receipt types.

#[cfg(feature = "source-analysis")]
#[path = "candidate_checks_ast.rs"]
mod ast;
#[path = "candidate_checks_evidence.rs"]
mod evidence;
#[path = "candidate_checks_integrity.rs"]
mod integrity;
#[path = "candidate_checks_scan.rs"]
mod scan;

use serde::Serialize;
use serde_json::{json, Value};

#[cfg(feature = "python")]
use pyo3::exceptions::PyValueError;
#[cfg(feature = "python")]
use pyo3::prelude::*;

#[derive(Debug, Serialize)]
pub struct Finding {
    check_id: &'static str,
    rule_id: &'static str,
    severity: &'static str,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    line: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    column: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    help: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    evidence: Option<Value>,
}

impl Finding {
    pub fn new(
        check_id: &'static str,
        rule_id: &'static str,
        severity: &'static str,
        message: impl Into<String>,
    ) -> Self {
        Self {
            check_id,
            rule_id,
            severity,
            message: message.into(),
            path: None,
            line: None,
            column: None,
            help: None,
            evidence: None,
        }
    }

    pub fn path(mut self, path: impl Into<String>) -> Self {
        self.path = Some(path.into());
        self
    }

    pub fn line(mut self, line: usize) -> Self {
        self.line = Some(line);
        self
    }

    pub fn column(mut self, column: usize) -> Self {
        self.column = Some(column);
        self
    }

    pub fn help(mut self, help: &'static str) -> Self {
        self.help = Some(help);
        self
    }

    pub fn evidence(mut self, evidence: Value) -> Self {
        self.evidence = Some(evidence);
        self
    }
}

fn string<'a>(payload: &'a Value, field: &str) -> Result<&'a str, String> {
    payload[field]
        .as_str()
        .ok_or_else(|| format!("candidate checks require string {field}"))
}

fn array<'a>(payload: &'a Value, field: &str) -> Result<&'a [Value], String> {
    payload[field]
        .as_array()
        .map(Vec::as_slice)
        .ok_or_else(|| format!("candidate checks require array {field}"))
}

pub fn evaluate(operation: &str, payload: &Value) -> Result<Value, String> {
    match operation {
        "tree-integrity" => {
            Ok(json!({"findings": integrity::tree_integrity(array(payload, "entries")?)?}))
        }
        "candidate-integrity" => integrity::candidate_integrity(payload),
        "dependency-integrity" => integrity::dependency_integrity(payload),
        "files-for-policy" => integrity::files_for_policy(payload),
        "secret-scan" => scan::secret_scan(payload),
        "native-source" => scan::native_source(payload),
        "performance-selection" => evidence::performance_selection(payload),
        "performance-evidence" => evidence::performance_evidence(payload),
        "research-integrity" => evidence::research_integrity(payload),
        #[cfg(feature = "source-analysis")]
        "python-ast" => ast::python_ast(payload),
        _ => Err(format!("unknown candidate checks operation: {operation}")),
    }
}

#[cfg(feature = "python")]
#[pyfunction]
fn candidate_checks_native(operation: &str, payload_json: &str) -> PyResult<String> {
    let payload: Value = serde_json::from_str(payload_json)
        .map_err(|error| PyValueError::new_err(error.to_string()))?;
    let result = evaluate(operation, &payload).map_err(PyValueError::new_err)?;
    serde_json::to_string(&result).map_err(|error| PyValueError::new_err(error.to_string()))
}

#[cfg(feature = "python")]
pub fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(candidate_checks_native, module)?)
}
