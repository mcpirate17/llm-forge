use serde_json::{json, Value};

fn rows(input: &Value) -> Result<&[Value], String> {
    input
        .as_array()
        .map(Vec::as_slice)
        .ok_or_else(|| "receipt cells must be a list".to_owned())
}

fn required_status(row: &Value) -> Result<Option<&str>, String> {
    let Some(object) = row.as_object() else {
        return Ok(None);
    };
    if object.get("required").is_some_and(|value| value == false) {
        return Ok(None);
    }
    let status = object
        .get("status")
        .and_then(Value::as_str)
        .ok_or_else(|| "receipt cell status is missing".to_owned())?;
    match status {
        "PASS" | "NOT_READY" | "FAIL-CLOSED" => Ok(Some(status)),
        other => Err(format!("invalid receipt cell status: {other}")),
    }
}

pub(super) fn aggregate_status(input: &Value) -> Result<Value, String> {
    let mut not_ready = false;
    for row in rows(input)? {
        match required_status(row)? {
            Some("FAIL-CLOSED") => return Ok(json!("FAIL-CLOSED")),
            Some("NOT_READY") => not_ready = true,
            _ => (),
        }
    }
    Ok(json!(if not_ready { "NOT_READY" } else { "PASS" }))
}

fn numeric_tokens(value: &Value) -> Option<i64> {
    if let Some(number) = value.as_i64() {
        return Some(number);
    }
    value.as_f64().and_then(|number| {
        if number.is_finite() && number >= i64::MIN as f64 && number <= i64::MAX as f64 {
            Some(number as i64)
        } else {
            None
        }
    })
}

fn token_total(value: &Value) -> i64 {
    match value {
        Value::Array(items) => items.iter().map(token_total).max().unwrap_or(0),
        Value::Object(fields) => {
            if let Some(total) = fields.get("total_tokens").and_then(numeric_tokens) {
                return total;
            }
            let mut maximum = 0;
            for (input_key, output_key) in [
                ("input_tokens", "output_tokens"),
                ("prompt_tokens", "completion_tokens"),
            ] {
                if let (Some(input), Some(output)) = (
                    fields.get(input_key).and_then(numeric_tokens),
                    fields.get(output_key).and_then(numeric_tokens),
                ) {
                    maximum = maximum.max(input.saturating_add(output));
                }
            }
            fields
                .values()
                .fold(maximum, |best, item| best.max(token_total(item)))
        }
        _ => 0,
    }
}

pub(super) fn extract_reported_tokens(input: &Value) -> Result<Value, String> {
    let output = input
        .as_str()
        .ok_or_else(|| "launcher output must be text".to_owned())?;
    let maximum = output
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .map(|payload| token_total(&payload))
        .max()
        .unwrap_or(0);
    Ok(json!(maximum))
}

pub(super) fn replace_receipt_cells(input: &Value) -> Result<Value, String> {
    let receipt = input
        .get("receipt")
        .ok_or_else(|| "receipt payload missing".to_owned())?;
    let replacements = input
        .get("replacements")
        .and_then(Value::as_object)
        .ok_or_else(|| "receipt replacements must be an object".to_owned())?;
    let mut payload = receipt.clone();
    let cells = payload
        .get_mut("cells")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| "receipt cells must be a list".to_owned())?;
    let present: std::collections::HashSet<_> = cells
        .iter()
        .filter_map(|row| row.get("cell_id").and_then(Value::as_str))
        .map(str::to_owned)
        .collect();
    if input.get("require_launcher").and_then(Value::as_bool) == Some(true)
        && !present.contains("launcher-real-smokes")
    {
        return Err("launcher-real-smokes cell missing".to_owned());
    }
    let missing: Vec<_> = replacements
        .keys()
        .filter(|key| !present.contains(key.as_str()))
        .collect();
    if let Some(single) = input.get("single_cell").and_then(Value::as_str) {
        if missing.iter().any(|key| key.as_str() == single) {
            return Err(format!("{single} cell missing"));
        }
    }
    for row in cells.iter_mut() {
        let Some(id) = row.get("cell_id").and_then(Value::as_str) else {
            continue;
        };
        if let Some(replacement) = replacements.get(id) {
            *row = replacement.clone();
        }
    }
    let status = aggregate_status(&Value::Array(cells.clone()))?;
    payload["status"] = status;
    if !payload.get("provenance").is_none_or(Value::is_object) {
        return Err("receipt provenance must be an object".to_owned());
    }
    if payload.get("provenance").is_none() {
        payload["provenance"] = json!({});
    }
    Ok(payload)
}
