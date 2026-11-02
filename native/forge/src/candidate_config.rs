//! Pure bounded config/notebook validation over caller-admitted captured text.
//! YAML graph admission never expands aliases or executes tags.
#[path = "candidate_config_yaml.rs"]
mod yaml;
use anyhow::{ensure, Result};
use conductor_native::candidate_checks::Finding;
use serde_json::{json, Value};
use std::collections::BTreeMap;
#[derive(Clone, Debug)]
pub struct ConfigInput {
    pub path: String,
    pub text: String,
    pub notebook: bool,
}
#[derive(Clone, Copy)]
struct Limits {
    files: usize,
    source: usize,
    total: usize,
    nodes: usize,
    depth: usize,
    scalar: usize,
    findings: usize,
    output: usize,
}
const LIMITS: Limits = Limits {
    files: 4096,
    source: 1024 * 1024,
    total: 32 * 1024 * 1024,
    nodes: 65_536,
    depth: 32,
    scalar: 64 * 1024,
    findings: 4096,
    output: 32 * 1024 * 1024,
};
struct Budget {
    limits: Limits,
    nodes: usize,
    scalars: usize,
}
impl Budget {
    fn pending(&self, current: usize, added: usize) -> Result<(), &'static str> {
        self.nodes
            .checked_add(current)
            .and_then(|size| size.checked_add(added))
            .filter(|size| *size <= self.limits.nodes)
            .ok_or("config exceeds pending node limit")?;
        Ok(())
    }
    fn node(&mut self, depth: usize) -> Result<(), &'static str> {
        self.nodes += 1;
        if self.nodes > self.limits.nodes || depth > self.limits.depth {
            return Err("config exceeds node/depth limit");
        }
        Ok(())
    }
    fn scalar(&mut self, size: usize) -> Result<(), &'static str> {
        self.scalars = self
            .scalars
            .checked_add(size)
            .ok_or("config scalar byte overflow")?;
        if size > self.limits.scalar || self.scalars > self.limits.source {
            return Err("config exceeds scalar byte limit");
        }
        Ok(())
    }
}
fn inputs(records: &[ConfigInput], limits: Limits) -> Result<BTreeMap<&str, &ConfigInput>> {
    ensure!(
        records.len() <= limits.files,
        "config input count exceeds limit"
    );
    let mut total = 0usize;
    let mut files = BTreeMap::new();
    for record in records {
        ensure!(
            !record.path.is_empty()
                && record.path.len() <= 4096
                && !record.path.chars().any(char::is_control)
                && record.path.split('/').count() <= 128
                && record
                    .path
                    .split('/')
                    .all(|part| !part.is_empty() && part != "." && part != ".."),
            "config path must be bounded normalized relative UTF8"
        );
        ensure!(
            record.text.len() <= limits.source,
            "config source exceeds per-file byte limit"
        );
        total = total
            .checked_add(record.path.len())
            .and_then(|size| size.checked_add(record.text.len()))
            .filter(|size| *size <= limits.total)
            .ok_or_else(|| anyhow::anyhow!("config inputs exceed aggregate byte limit"))?;
        ensure!(
            files.insert(record.path.as_str(), record).is_none(),
            "duplicate config path"
        );
    }
    Ok(files)
}
fn json_budget(value: &Value, limits: Limits) -> Result<(), &'static str> {
    let mut budget = Budget {
        limits,
        nodes: 0,
        scalars: 0,
    };
    let mut pending = vec![(value, 0usize)];
    while let Some((value, depth)) = pending.pop() {
        budget.node(depth)?;
        match value {
            Value::Object(values) => {
                for (key, value) in values {
                    budget.node(depth + 1)?;
                    budget.scalar(key.len())?;
                    budget.pending(pending.len(), 1)?;
                    pending.push((value, depth + 1));
                }
            }
            Value::Array(values) => {
                budget.pending(pending.len(), values.len())?;
                pending.extend(values.iter().map(|value| (value, depth + 1)));
            }
            Value::String(value) => budget.scalar(value.len())?,
            Value::Number(value) => {
                budget.scalar(value.to_string().len())?;
                if !value.as_f64().is_some_and(f64::is_finite) {
                    return Err("nonfinite or out-of-range JSON number");
                }
            }
            _ => {}
        }
    }
    Ok(())
}
fn parse_json(text: &str, limits: Limits) -> Result<Value, &'static str> {
    let value = crate::tool_schema::parse_document_with_limit(text, limits.source)
        .map_err(|_| "invalid JSON or duplicate mapping key")?;
    json_budget(&value, limits)?;
    Ok(value)
}
fn toml_valid(text: &str, limits: Limits) -> Result<(), &'static str> {
    let value: toml::Value =
        toml::from_str(text).map_err(|_| "invalid TOML syntax or duplicate key")?;
    let mut budget = Budget {
        limits,
        nodes: 0,
        scalars: 0,
    };
    let mut pending = vec![(&value, 0usize)];
    while let Some((value, depth)) = pending.pop() {
        budget.node(depth)?;
        match value {
            toml::Value::Table(values) => {
                for (key, value) in values {
                    budget.node(depth + 1)?;
                    budget.scalar(key.len())?;
                    budget.pending(pending.len(), 1)?;
                    pending.push((value, depth + 1));
                }
            }
            toml::Value::Array(values) => {
                budget.pending(pending.len(), values.len())?;
                pending.extend(values.iter().map(|value| (value, depth + 1)))
            }
            toml::Value::String(value) => budget.scalar(value.len())?,
            toml::Value::Datetime(value) => budget.scalar(value.to_string().len())?,
            _ => {}
        }
    }
    Ok(())
}
fn push(rows: &mut Vec<Finding>, finding: Finding, limits: Limits) -> Result<()> {
    ensure!(
        rows.len() < limits.findings,
        "config findings exceed limit; evaluation incomplete"
    );
    rows.push(finding);
    Ok(())
}
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(value) => value.as_f64().is_some_and(|n| n != 0.0),
        Value::String(value) => !value.is_empty(),
        Value::Array(value) => !value.is_empty(),
        Value::Object(value) => !value.is_empty(),
    }
}
fn notebook(
    value: &Value,
    path: &str,
    rows: &mut Vec<Finding>,
    limits: Limits,
) -> Result<Option<&'static str>> {
    let Some(cells) = value
        .as_object()
        .and_then(|object| object.get("cells"))
        .and_then(Value::as_array)
    else {
        return Ok(Some("notebook must be an object with a cells array"));
    };
    if value.get("nbformat").and_then(Value::as_f64) != Some(4.0) {
        return Ok(Some("unsupported notebook format"));
    }
    for (index, cell) in cells.iter().enumerate() {
        if !cell.is_object() {
            return Ok(Some("notebook cell must be an object"));
        }
        if cell.get("cell_type").and_then(Value::as_str) == Some("code")
            && ["outputs", "execution_count"]
                .iter()
                .any(|key| cell.get(key).is_some_and(truthy))
        {
            push(
                rows,
                Finding::new(
                    "config-parse",
                    "notebook-output",
                    "high",
                    "tracked notebooks must have outputs and execution counts stripped",
                )
                .path(path)
                .line(index + 1),
                limits,
            )?;
        }
    }
    Ok(None)
}
fn parse(record: &ConfigInput, limits: Limits) -> Result<Option<Value>, &'static str> {
    let suffix = record
        .path
        .rsplit('/')
        .next()
        .and_then(|name| name.rsplit_once('.'))
        .map(|(_, suffix)| suffix.to_ascii_lowercase());
    if record.notebook || suffix.as_deref() == Some("ipynb") {
        return parse_json(&record.text, limits).map(Some);
    }
    match suffix.as_deref() {
        Some("json") => parse_json(&record.text, limits).map(Some),
        Some("toml") => toml_valid(&record.text, limits).map(|_| None),
        Some("yaml" | "yml") => yaml::validate(&record.text, limits).map(|_| None),
        _ => Err("unsupported config format"),
    }
}
/// Global admission/output failures are Err; per-file unsupported/malformed
/// content yields critical findings. No source contents are copied into errors.
pub fn evaluate(records: &[ConfigInput]) -> Result<Value> {
    evaluate_with_limits(records, LIMITS)
}
fn evaluate_with_limits(records: &[ConfigInput], limits: Limits) -> Result<Value> {
    let inputs = inputs(records, limits)?;
    let mut rows = Vec::new();
    let mut malformed = 0usize;
    let mut notebooks = 0usize;
    for record in inputs.values() {
        let is_notebook = record.notebook || record.path.to_ascii_lowercase().ends_with(".ipynb");
        notebooks += usize::from(is_notebook);
        let reason = match parse(record, limits) {
            Ok(Some(value)) if is_notebook => notebook(&value, &record.path, &mut rows, limits)?,
            Ok(_) => None,
            Err(reason) => Some(reason),
        };
        if let Some(reason) = reason {
            malformed += 1;
            push(
                &mut rows,
                Finding::new(
                    "config-parse",
                    "malformed-config",
                    "critical",
                    format!("candidate config/notebook cannot be parsed: {reason}"),
                )
                .path(&record.path),
                limits,
            )?;
        }
    }
    let result = json!({"findings":rows,"files":inputs.keys().collect::<Vec<_>>(),
        "metrics":{"files_scanned":inputs.len(),"files_parsed":inputs.len()-malformed,"malformed_files":malformed,"notebooks":notebooks}});
    crate::json_canon::encoded_size(&result, limits.output)?;
    Ok(result)
}
#[cfg(test)]
#[path = "candidate_config_tests.rs"]
mod tests;
