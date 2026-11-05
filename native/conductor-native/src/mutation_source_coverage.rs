//! Deterministic lexical per-source mutation coverage, shared by runner and validators.
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::path::{Component, Path};

pub const SCHEMA_VERSION: u64 = 1;

pub fn normalise(raw: &str) -> Result<String, String> {
    let path = Path::new(raw);
    if raw.is_empty()
        || raw.contains('\0')
        || path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir | Component::Prefix(_)))
    {
        return Err(format!("invalid mutant source path: {raw:?}"));
    }
    let parts: Vec<&str> = path
        .components()
        .filter_map(|part| match part {
            Component::Normal(value) => value.to_str(),
            _ => None,
        })
        .collect();
    if parts.is_empty() {
        return Err(format!("invalid mutant source path: {raw:?}"));
    }
    Ok(parts.join("/"))
}

pub fn report(pins: &Map<String, Value>, rows: &[Value]) -> Result<Value, String> {
    let mut counts = BTreeMap::<String, u64>::new();
    for path in pins.keys() {
        counts.insert(normalise(path)?, 0);
    }
    if counts.is_empty() {
        return Err("no selected mutation sources".into());
    }
    for row in rows {
        let raw = row
            .get("path")
            .and_then(Value::as_str)
            .ok_or("mutant source path is missing")?;
        let path = normalise(raw)?;
        let count = counts
            .get_mut(&path)
            .ok_or_else(|| format!("mutant source is outside selected targets: {raw:?}"))?;
        if matches!(
            row.get("outcome").and_then(Value::as_str),
            Some("KILLED" | "SURVIVED")
        ) {
            *count += 1;
        }
    }
    let missing: Vec<String> = counts
        .iter()
        .filter(|(_, count)| **count == 0)
        .map(|(path, _)| path.clone())
        .collect();
    Ok(
        json!({"schema_version":SCHEMA_VERSION,"complete":missing.is_empty(),"scored_by_source":counts,"missing":missing}),
    )
}

/// Recompute every field; a declaration alone is not source coverage evidence.
pub fn validate(payload: &Map<String, Value>) -> Result<(), String> {
    let pins = payload
        .get("source_sha256")
        .and_then(Value::as_object)
        .ok_or("source coverage pins are missing")?;
    let rows = payload
        .get("mutants")
        .and_then(Value::as_array)
        .ok_or("source coverage mutant rows are missing")?;
    let actual = report(pins, rows)?;
    if actual["complete"] != true {
        return Err("selected mutation sources have no scored mutants".into());
    }
    if payload.get("source_coverage") != Some(&actual) {
        return Err("source coverage report differs from scored mutant rows".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Map<String, Value> {
        json!({
            "source_sha256": {"src/a.rs":"a", "src/b.rs":"b"},
            "mutants": [{"path":"./src/a.rs","outcome":"KILLED"}, {"path":"src/b.rs","outcome":"SURVIVED"}],
            "source_coverage": {"schema_version":1,"complete":true,"scored_by_source":{"src/a.rs":1,"src/b.rs":1},"missing":[]}
        }).as_object().unwrap().clone()
    }

    #[test]
    fn report_requires_each_selected_source_and_exact_recorded_counts() {
        let mut payload = fixture();
        assert!(validate(&payload).is_ok());
        payload["source_coverage"]["scored_by_source"]["src/b.rs"] = json!(2);
        assert!(validate(&payload).unwrap_err().contains("differs"));
        payload = fixture();
        payload.remove("source_coverage");
        assert!(validate(&payload).unwrap_err().contains("differs"));
        payload = fixture();
        payload["mutants"] = json!([{"path":"src/a.rs","outcome":"KILLED"}]);
        assert!(validate(&payload)
            .unwrap_err()
            .contains("no scored mutants"));
    }

    #[test]
    fn unexpected_and_unsafe_row_paths_are_rejected() {
        let mut payload = fixture();
        for path in ["src/c.rs", "../src/a.rs", "/src/a.rs", ""] {
            payload["mutants"][0]["path"] = json!(path);
            assert!(validate(&payload).is_err(), "{path}");
        }
    }
}
