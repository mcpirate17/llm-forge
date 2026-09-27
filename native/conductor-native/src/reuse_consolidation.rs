//! Pure report shaping for deterministic duplicate-consolidation clusters.
//! AST extraction and cluster scoring remain in the existing slop-core lane.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashSet;

#[derive(Clone, Deserialize, Serialize)]
struct Site {
    file: String,
    line_start: i64,
    line_end: i64,
    name: String,
    node_hash: String,
    tokens: i64,
    source: String,
}

#[derive(Deserialize)]
struct Cluster {
    kind: String,
    tokens: i64,
    sites: Vec<Site>,
    id: String,
    batch: i64,
    confidence: f64,
    value_score: i64,
    risk: String,
    disposition: String,
    rationale: String,
}

impl Cluster {
    fn est_bytes(&self) -> i64 {
        self.tokens * (self.sites.len() as i64 - 1)
    }

    fn row(&self, home: Option<&str>) -> Value {
        json!({
            "id": self.id, "kind": self.kind, "tokens": self.tokens,
            "n_sites": self.sites.len(), "est_bytes": self.est_bytes(),
            "confidence": self.confidence, "value_score": self.value_score,
            "risk": self.risk, "disposition": self.disposition,
            "rationale": self.rationale, "batch": self.batch,
            "suggested_home": home,
            "sites": self.sites.iter().map(|site| json!({
                "file": site.file, "line_start": site.line_start,
                "line_end": site.line_end, "name": site.name
            })).collect::<Vec<_>>()
        })
    }
}

fn clusters(payload: &Value) -> Result<Vec<Cluster>, String> {
    serde_json::from_value(payload["clusters"].clone())
        .map_err(|error| format!("invalid consolidation clusters: {error}"))
}

fn homes(payload: &Value, count: usize) -> Result<Vec<Option<String>>, String> {
    let homes: Vec<Option<String>> = serde_json::from_value(payload["homes"].clone())
        .map_err(|error| format!("invalid consolidation homes: {error}"))?;
    if homes.len() != count {
        return Err("consolidation homes must match clusters".into());
    }
    Ok(homes)
}

fn cluster_rows(payload: &Value) -> Result<Value, String> {
    let clusters = clusters(payload)?;
    let homes = homes(payload, clusters.len())?;
    Ok(json!(clusters
        .iter()
        .zip(homes.iter())
        .map(|(cluster, home)| cluster.row(home.as_deref()))
        .collect::<Vec<_>>()))
}

fn report_summary(payload: &Value) -> Result<Value, String> {
    let clusters = clusters(payload)?;
    let actionable: Vec<_> = clusters
        .iter()
        .filter(|cluster| cluster.disposition == "auto")
        .collect();
    let batches: HashSet<_> = actionable.iter().map(|cluster| cluster.batch).collect();
    Ok(json!({
        "n_clusters": clusters.len(),
        "n_actionable": actionable.len(),
        "n_validate": clusters.iter().filter(|cluster| cluster.disposition == "validate").count(),
        "n_ignored": clusters.iter().filter(|cluster| cluster.disposition == "ignore").count(),
        "n_batches": batches.len(),
        "total_redundant_bytes": clusters.iter().map(Cluster::est_bytes).sum::<i64>(),
        "actionable_redundant_bytes": actionable.iter().map(|cluster| cluster.est_bytes()).sum::<i64>(),
        "actionable_value_score": actionable.iter().map(|cluster| cluster.value_score).sum::<i64>(),
        "files_scanned": payload["files_scanned"],
        "files_unparsable": payload["files_unparsable"],
        "functions_considered": payload["functions_considered"]
    }))
}

fn nonempty_object<'a>(
    value: &'a Value,
    field: &str,
) -> Option<&'a serde_json::Map<String, Value>> {
    value
        .get(field)
        .and_then(Value::as_object)
        .filter(|row| !row.is_empty())
}

fn token_count(row: &Value) -> i64 {
    row.get("tokens")
        .and_then(Value::as_i64)
        .filter(|count| *count != 0)
        .or_else(|| {
            row.get("lines")
                .and_then(Value::as_i64)
                .filter(|count| *count != 0)
        })
        .unwrap_or(0)
}

fn token_clones(payload: &Value) -> Result<Value, String> {
    let empty = Vec::new();
    let rows = match payload.get("duplicates") {
        None => &empty,
        Some(value) => value.as_array().ok_or("duplicates must be a list")?,
    };
    let mut clusters = Vec::new();
    for row in rows {
        let (Some(first), Some(second)) = (
            nonempty_object(row, "firstFile"),
            nonempty_object(row, "secondFile"),
        ) else {
            continue;
        };
        let tokens = token_count(row);
        let site = |fields: &serde_json::Map<String, Value>| -> Result<Site, String> {
            Ok(Site {
                file: fields
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or("clone site lacks name")?
                    .to_owned(),
                line_start: fields
                    .get("start")
                    .and_then(Value::as_i64)
                    .ok_or("clone site lacks start")?,
                line_end: fields
                    .get("end")
                    .and_then(Value::as_i64)
                    .ok_or("clone site lacks end")?,
                name: "<clone>".into(),
                node_hash: String::new(),
                tokens,
                source: String::new(),
            })
        };
        clusters.push(json!({"kind":"token", "tokens":tokens,
            "sites":[site(first)?, site(second)?]}));
    }
    Ok(json!(clusters))
}

fn markdown_row(row: &Value) -> Result<String, String> {
    let sites = row["sites"]
        .as_array()
        .ok_or("cluster sites must be a list")?;
    let site_strings = sites
        .iter()
        .map(|site| {
            format!(
                "{}:{}",
                display(&site["file"]),
                display(&site["line_start"])
            )
        })
        .collect::<Vec<_>>();
    let mut site_field = site_strings
        .iter()
        .take(6)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    if site_strings.len() > 6 {
        site_field.push_str(&format!(", +{} more", site_strings.len() - 6));
    }
    let confidence = row["confidence"]
        .as_f64()
        .ok_or("cluster confidence must be numeric")?;
    let home = row["suggested_home"]
        .as_str()
        .filter(|home| !home.is_empty())
        .unwrap_or("review required");
    Ok(format!(
        "| {} | {} | {} | {} | {:.2} | {} | {} | {} | {} | {} |",
        display(&row["id"]),
        display(&row["batch"]),
        display(&row["kind"]),
        display(&row["disposition"]),
        confidence,
        display(&row["value_score"]),
        display(&row["n_sites"]),
        display(&row["est_bytes"]),
        home,
        site_field
    ))
}

fn display(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        _ => value.to_string(),
    }
}

fn markdown(payload: &Value) -> Result<Value, String> {
    let summary = &payload["summary"];
    let rows = payload["clusters"]
        .as_array()
        .ok_or("report clusters must be a list")?;
    let mut lines = vec![
        format!("# Consolidation report — generated {}", display(&payload["generated_at"])),
        String::new(),
        format!(
            "{} clusters across {} batches; {} auto-actionable, {} require validation; ~{} redundant AST-node-units; {} files scanned ({} unparsable), {} functions considered.",
            display(&summary["n_clusters"]), display(&summary["n_batches"]),
            display(&summary["n_actionable"]), display(&summary["n_validate"]),
            display(&summary["total_redundant_bytes"]), display(&summary["files_scanned"]),
            display(&summary["files_unparsable"]), display(&summary["functions_considered"])
        ),
        String::new(),
        "| id | batch | kind | disposition | confidence | value | n_sites | est_bytes | suggested_home | sites (file:line, ...) |".into(),
        "|---|---|---|---|---|---|---|---|---|---|".into(),
    ];
    for row in rows {
        lines.push(markdown_row(row)?);
    }
    Ok(json!(lines.join("\n") + "\n"))
}

pub fn decide(operation: &str, payload: &Value) -> Result<Value, String> {
    match operation {
        "token_clones" => token_clones(payload),
        "cluster_dicts" => cluster_rows(payload),
        "report_summary" => report_summary(payload),
        "markdown_row" => markdown_row(payload).map(|row| json!(row)),
        "markdown" => markdown(payload),
        other => Err(format!("unknown consolidation operation: {other}")),
    }
}

#[cfg(feature = "python")]
mod python {
    use super::*;
    use pyo3::exceptions::PyValueError;
    use pyo3::prelude::*;

    #[pyfunction]
    fn reuse_consolidation_native(operation: &str, payload_json: &str) -> PyResult<String> {
        let payload: Value = serde_json::from_str(payload_json)
            .map_err(|error| PyValueError::new_err(error.to_string()))?;
        let result = decide(operation, &payload).map_err(PyValueError::new_err)?;
        serde_json::to_string(&result).map_err(|error| PyValueError::new_err(error.to_string()))
    }

    pub fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
        module.add_function(wrap_pyfunction!(reuse_consolidation_native, module)?)?;
        Ok(())
    }
}

#[cfg(feature = "python")]
pub use python::register;
