//! Rust-owned subprocess fixtures for the production CRG MCP probe.

use crate::support::{assert_error, module, path};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyModule, PyTuple};
use std::path::Path;

pub const CHILD: &str = env!("CARGO_BIN_EXE_crg_mcp_probe_child");
pub const CHILD_ARG: &str = "fixture-child";

pub fn probe_module(py: Python<'_>) -> Bound<'_, PyModule> {
    module(py, "conductor.crg_mcp_probe")
}

pub fn call_probe<'py>(
    py: Python<'py>,
    repo: &Path,
    child_env: &[(&str, &str)],
    expected_tools: Option<usize>,
) -> PyResult<Bound<'py, PyAny>> {
    let env = PyDict::new(py);
    for (name, value) in child_env {
        env.set_item(name, value)?;
    }
    let arguments = PyDict::new(py);
    let call = PyTuple::new(
        py,
        ["stats_tool".into_pyobject(py)?.as_any(), arguments.as_any()],
    )?;
    let kwargs = PyDict::new(py);
    kwargs.set_item("timeout", 30.0)?;
    if let Some(expected_tools) = expected_tools {
        kwargs.set_item("expect_tools", expected_tools)?;
    }
    probe_module(py).getattr("probe")?.call(
        (vec![CHILD, CHILD_ARG], path(py, repo), env, call),
        Some(&kwargs),
    )
}

pub fn probe_error(py: Python<'_>, result: PyResult<Bound<'_, PyAny>>, part: &str) {
    probe_error_parts(py, result, &[part]);
}

pub fn probe_error_parts(py: Python<'_>, result: PyResult<Bound<'_, PyAny>>, parts: &[&str]) {
    let class = probe_module(py).getattr("ProbeError").unwrap();
    let error = result.unwrap_err();
    let message = error.to_string();
    for part in parts {
        assert!(message.contains(part), "missing {part:?} in {message:?}");
    }
    assert_error(py, error, &class, parts[0]);
}
