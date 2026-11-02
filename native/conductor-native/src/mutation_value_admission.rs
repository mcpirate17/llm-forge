//! Shared pure value admission; no interpreter, filesystem or execution proof.
use serde_json::Value;
use std::collections::HashMap;
const VALUE_SCHEMA: &str = "llm.mutation-testing.test-value.v1";
pub(crate) fn python_repr(value: Option<&Value>) -> String {
    match value.unwrap_or(&Value::Null) {
        Value::Null => "None".to_owned(),
        Value::Bool(true) => "True".to_owned(),
        Value::Bool(false) => "False".to_owned(),
        Value::Number(number) => number.to_string(),
        Value::String(text) => format!("'{}'", text.replace('\\', "\\\\").replace('\'', "\\'")),
        other => other.to_string(),
    }
}

pub fn admission_errors(value: &Value, required_nodeids: &[String]) -> Result<Vec<String>, String> {
    if required_nodeids.len() > 4096 || required_nodeids.iter().any(|node| node.len() > 4096) {
        return Err("value admission nodeid bounds exceeded".into());
    }
    crate::mutation_evidence::admit_captured(value)?;
    let Some(payload) = value.as_object() else {
        return Ok(vec!["receipt has no test_value evidence".to_owned()]);
    };
    let mut errors = Vec::new();
    if payload.get("schema_version").and_then(Value::as_str) != Some(VALUE_SCHEMA) {
        errors.push("test_value schema is not current".to_owned());
    }
    if payload.get("status").and_then(Value::as_str) != Some("PASS") {
        errors.push(format!(
            "test_value status={}",
            python_repr(payload.get("status"))
        ));
    }
    let Some(rows) = payload.get("tests").and_then(Value::as_array) else {
        errors.push("test_value.tests must be a list".to_owned());
        return Ok(errors);
    };
    let by_nodeid = rows
        .iter()
        .filter_map(|row| row.as_object())
        .filter_map(|row| {
            row.get("nodeid")
                .and_then(Value::as_str)
                .map(|nodeid| (nodeid, row))
        })
        .collect::<HashMap<_, _>>();
    for nodeid in required_nodeids {
        let Some(row) = by_nodeid.get(nodeid.as_str()) else {
            errors.push(format!(
                "new test {} has no value classification",
                python_repr(Some(&Value::String(nodeid.clone())))
            ));
            continue;
        };
        let classification = row.get("classification");
        if !classification
            .and_then(Value::as_str)
            .is_some_and(|value| value == "CORE" || value == "INTENTIONAL_REDUNDANCY")
        {
            errors.push(format!(
                "new test {} is classified {}; only CORE or INTENTIONAL_REDUNDANCY may be added",
                python_repr(Some(&Value::String(nodeid.clone()))),
                python_repr(classification)
            ));
        }
    }
    Ok(errors)
}
