//! Deterministic hook-response decision algebra.

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use serde::Deserialize;
use serde_json::{json, Map, Value};

const SEP: &str = "\n\n";
const REWRITE: [&str; 2] = ["updatedToolOutput", "updatedMCPToolOutput"];

#[derive(Deserialize)]
struct Outcome {
    name: String,
    output: Option<Value>,
    error: Option<String>,
    fail_closed: bool,
}

fn rank(value: &str) -> u8 {
    match value {
        "allow" => 1,
        "ask" => 2,
        "deny" => 3,
        _ => 0,
    }
}
fn text(value: Option<&Value>) -> String {
    value.and_then(Value::as_str).unwrap_or_default().to_owned()
}
fn object(value: &Value) -> Option<&Map<String, Value>> {
    value.as_object()
}

#[pyfunction]
fn hook_merge_native(event: &str, outcomes_json: &str) -> PyResult<String> {
    let outcomes: Vec<Outcome> = serde_json::from_str(outcomes_json)
        .map_err(|error| PyValueError::new_err(format!("invalid hook outcomes: {error}")))?;
    let mut votes: Vec<(String, String)> = Vec::new();
    let mut contexts = Vec::new();
    let mut system = Vec::new();
    let mut blocks = Vec::new();
    let mut stops = Vec::new();
    let mut rewrites = Map::new();
    let mut extras = Map::new();
    let mut stop = false;
    let mut suppress = false;
    for outcome in outcomes {
        if let Some(error) = outcome.error {
            let line = format!("HOOK ERROR [{}]: {error}", outcome.name);
            if event == "PreToolUse" && outcome.fail_closed {
                votes.push(("deny".to_owned(), line.clone()));
            } else {
                contexts.push(line.clone());
            }
            system.push(line);
        }
        let Some(output) = outcome.output.and_then(|value| object(&value).cloned()) else {
            continue;
        };
        let specific = output
            .get("hookSpecificOutput")
            .and_then(object)
            .cloned()
            .unwrap_or_default();
        if let Some(decision) = specific
            .get("permissionDecision")
            .and_then(Value::as_str)
            .filter(|value| rank(value) > 0)
        {
            votes.push((
                decision.to_owned(),
                text(specific.get("permissionDecisionReason")),
            ));
        }
        if let Some(context) = specific
            .get("additionalContext")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
        {
            contexts.push(context.to_owned());
        }
        for key in REWRITE {
            if let Some(value) = specific.get(key) {
                if rewrites.contains_key(key) {
                    system.push(format!("HOOK CONFLICT [{}]: {key} already set by an earlier hook; the later value wins", outcome.name));
                }
                rewrites.insert(key.to_owned(), value.clone());
            }
        }
        for (key, value) in specific {
            if !matches!(
                key.as_str(),
                "hookEventName"
                    | "permissionDecision"
                    | "permissionDecisionReason"
                    | "additionalContext"
                    | "updatedToolOutput"
                    | "updatedMCPToolOutput"
            ) {
                extras.insert(key, value);
            }
        }
        if matches!(
            output.get("decision").and_then(Value::as_str),
            Some("block" | "deny")
        ) {
            let reason = text(output.get("reason"));
            if event == "PreToolUse" {
                votes.push(("deny".to_owned(), reason));
            } else {
                blocks.push(reason);
            }
        }
        if output.get("continue") == Some(&Value::Bool(false)) {
            stop = true;
            if let Some(reason) = output.get("stopReason") {
                stops.push(text(Some(reason)));
            }
        }
        suppress |= output.get("suppressOutput") == Some(&Value::Bool(true));
        if let Some(message) = output
            .get("systemMessage")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
        {
            system.push(message.to_owned());
        }
    }
    let mut specific = Map::new();
    specific.insert("hookEventName".to_owned(), json!(event));
    if let Some(winner) = votes
        .iter()
        .max_by_key(|(decision, _)| rank(decision))
        .map(|(decision, _)| decision)
    {
        specific.insert("permissionDecision".to_owned(), json!(winner));
        let reasons: Vec<_> = votes
            .iter()
            .filter(|(decision, reason)| decision == winner && !reason.is_empty())
            .map(|(_, reason)| reason.clone())
            .collect();
        if !reasons.is_empty() {
            specific.insert(
                "permissionDecisionReason".to_owned(),
                json!(reasons.join(SEP)),
            );
        }
    }
    if !contexts.is_empty() {
        specific.insert("additionalContext".to_owned(), json!(contexts.join(SEP)));
    }
    specific.extend(rewrites);
    specific.extend(extras);
    let mut result = Map::new();
    result.insert("hookSpecificOutput".to_owned(), Value::Object(specific));
    if !blocks.is_empty() {
        result.insert("decision".to_owned(), json!("block"));
        result.insert(
            "reason".to_owned(),
            json!(blocks
                .into_iter()
                .filter(|reason| !reason.is_empty())
                .collect::<Vec<_>>()
                .join(SEP)),
        );
    }
    if stop {
        result.insert("continue".to_owned(), json!(false));
        if !stops.is_empty() {
            result.insert("stopReason".to_owned(), json!(stops.join(SEP)));
        }
    }
    if suppress {
        result.insert("suppressOutput".to_owned(), json!(true));
    }
    if !system.is_empty() {
        result.insert("systemMessage".to_owned(), json!(system.join(SEP)));
    }
    serde_json::to_string(&Value::Object(result))
        .map_err(|error| PyValueError::new_err(format!("hook merge serialization failed: {error}")))
}

pub fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(hook_merge_native, module)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::hook_merge_native;
    use serde_json::Value;

    #[test]
    fn pretool_fail_closed_error_beats_allow_and_preserves_context() {
        let result: Value = serde_json::from_str(
            &hook_merge_native(
                "PreToolUse",
                r#"[
                  {"name":"allow","output":{"hookSpecificOutput":{"permissionDecision":"allow","additionalContext":"kept"}},"error":null,"fail_closed":false},
                  {"name":"gate","output":null,"error":"boom","fail_closed":true}
                ]"#,
            )
            .expect("merge should succeed"),
        )
        .expect("result must be JSON");
        let specific = &result["hookSpecificOutput"];
        assert_eq!(specific["permissionDecision"], "deny");
        assert_eq!(specific["additionalContext"], "kept");
        assert!(result["systemMessage"]
            .as_str()
            .unwrap()
            .contains("HOOK ERROR [gate]"));
    }

    #[test]
    fn later_rewrite_wins_and_reports_conflict() {
        let result: Value = serde_json::from_str(
            &hook_merge_native(
                "PostToolUse",
                r#"[
                  {"name":"first","output":{"hookSpecificOutput":{"updatedToolOutput":{"value":1}}},"error":null,"fail_closed":false},
                  {"name":"second","output":{"hookSpecificOutput":{"updatedToolOutput":{"value":2}},"continue":false,"stopReason":"done"},"error":null,"fail_closed":false}
                ]"#,
            )
            .expect("merge should succeed"),
        )
        .expect("result must be JSON");
        assert_eq!(
            result["hookSpecificOutput"]["updatedToolOutput"]["value"],
            2
        );
        assert_eq!(result["continue"], false);
        assert_eq!(result["stopReason"], "done");
        assert!(result["systemMessage"]
            .as_str()
            .unwrap()
            .contains("HOOK CONFLICT [second]"));
    }
}
