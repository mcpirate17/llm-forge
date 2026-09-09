//! Strict, data-only project configuration parsing.

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use serde_json::json;

fn schema_error(message: &str) -> PyErr {
    PyValueError::new_err(format!("CONFIG_SCHEMA:{message}"))
}

#[pyfunction]
fn project_context_parse_config_native(raw: &[u8]) -> PyResult<String> {
    let text = std::str::from_utf8(raw).map_err(|_| {
        PyValueError::new_err("CONFIG_PARSE:configuration is not strict UTF-8 TOML")
    })?;
    let parsed = text.parse::<toml::Value>().map_err(|_| {
        PyValueError::new_err("CONFIG_PARSE:configuration is not strict UTF-8 TOML")
    })?;
    let root = parsed
        .as_table()
        .ok_or_else(|| schema_error("configuration must be a table"))?;
    if root
        .keys()
        .any(|key| !matches!(key.as_str(), "schema_version" | "project" | "paths"))
    {
        return Err(schema_error(
            "configuration contains unknown top-level keys",
        ));
    }
    if root.get("schema_version").and_then(toml::Value::as_integer) != Some(1) {
        return Err(schema_error("schema_version must be integer 1"));
    }
    let project = match root.get("project") {
        None => toml::map::Map::new(),
        Some(value) => value
            .as_table()
            .cloned()
            .ok_or_else(|| schema_error("[project] only supports id"))?,
    };
    if project.keys().any(|key| key != "id") {
        return Err(schema_error("[project] only supports id"));
    }
    let project_id = match project.get("id") {
        None => None,
        Some(value) => {
            let value = value.as_str().ok_or_else(|| {
                schema_error("project.id must be a trimmed 1-128 character non-control string")
            })?;
            if value.is_empty()
                || value.chars().count() > 128
                || value.trim() != value
                || value.chars().any(char::is_control)
            {
                return Err(schema_error(
                    "project.id must be a trimmed 1-128 character non-control string",
                ));
            }
            Some(value)
        }
    };
    let paths = match root.get("paths") {
        None => toml::map::Map::new(),
        Some(value) => value
            .as_table()
            .cloned()
            .ok_or_else(|| schema_error("[paths] has unknown keys"))?,
    };
    if paths
        .keys()
        .any(|key| !matches!(key.as_str(), "policy" | "registry" | "notes"))
    {
        return Err(schema_error("[paths] has unknown keys"));
    }
    let path_value = |field: &str| -> PyResult<Option<&str>> {
        match paths.get(field) {
            None => Ok(None),
            Some(value) => value
                .as_str()
                .filter(|item| !item.is_empty() && !item.contains(['\0', '\r', '\n']))
                .map(Some)
                .ok_or_else(|| schema_error(&format!("paths.{field} must be a non-empty string"))),
        }
    };
    let notes = match paths.get("notes") {
        None => Vec::new(),
        Some(value) => value
            .as_array()
            .ok_or_else(|| schema_error("paths.notes must be an array of non-empty strings"))?
            .iter()
            .map(|item| {
                item.as_str()
                    .filter(|text| !text.is_empty() && !text.contains(['\0', '\r', '\n']))
                    .map(str::to_owned)
                    .ok_or_else(|| {
                        schema_error("paths.notes must be an array of non-empty strings")
                    })
            })
            .collect::<PyResult<Vec<_>>>()?,
    };
    serde_json::to_string(&json!({"project_id":project_id,"policy":path_value("policy")?,"registry":path_value("registry")?,"notes":notes})).map_err(|error| PyValueError::new_err(format!("CONFIG_PARSE:{error}")))
}

pub fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(
        project_context_parse_config_native,
        module
    )?)?;
    Ok(())
}
