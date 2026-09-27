use serde_json::{json, Value};

const CLERK_MODEL: &str = "qwen3.5:9b";
const NOVEL_PREFIXES: [&str; 4] = [
    "component_fab/",
    "research/scientist/",
    "research/synthesis/",
    "research/tools/",
];
const NOVEL_SIGNALS: [&str; 6] = ["avo", "cuda", "gpu", "optimizer", "throughput", "training"];
const NON_RESEARCH_PROCESSES: [&str; 3] = ["gnome-remote-desktop", "gnome-shell", "xorg"];
const PROHIBITED: [&str; 2] = ["qwen3.8", "27b"];

fn text<'a>(payload: &'a Value, key: &str) -> Result<&'a str, String> {
    payload
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("matrix {key} must be text"))
}

fn rows(raw: &str) -> Result<Vec<String>, String> {
    let mut lines = raw.lines();
    if !lines
        .next()
        .is_some_and(|header| header.trim_start().starts_with("NAME"))
    {
        return Err(format!("unexpected ollama ps output: {raw:?}"));
    }
    Ok(lines
        .filter(|line| !line.trim().is_empty())
        .map(str::to_owned)
        .collect())
}

pub(super) fn ollama_model_rows(input: &Value) -> Result<Value, String> {
    Ok(json!(rows(
        input.as_str().ok_or("ollama output must be text")?
    )?))
}

pub(super) fn nonnegative_int(input: &Value) -> Result<Value, String> {
    let key = text(input, "key")?;
    let value = input.get("payload").and_then(|payload| payload.get(key));
    Ok(json!(value
        .and_then(Value::as_i64)
        .filter(|number| *number >= 0)
        .unwrap_or(-1)))
}

pub(super) fn parse_gpu_processes(input: &Value) -> Result<Value, String> {
    let stdout = text(input, "stdout")?;
    let mut processes = Vec::new();
    for line in stdout.lines().filter(|line| !line.trim().is_empty()) {
        let parts: Vec<_> = line.rsplitn(3, ',').collect();
        if parts.len() != 3 {
            return Err(format!("unexpected nvidia-smi process row: {line:?}"));
        }
        let pid = parts[2].trim().parse::<i64>();
        let memory = parts[0].trim().parse::<i64>();
        match (pid, memory) {
            (Ok(pid), Ok(memory)) => processes.push(json!({
                "pid":pid,"process_name":parts[1].trim(),"used_memory_mib":memory})),
            _ => return Err(format!("invalid nvidia-smi process row: {line:?}")),
        }
    }
    Ok(json!(processes))
}

fn reserves_novel_gpu(claim: &Value) -> bool {
    let paths: Vec<String> = claim
        .get("paths")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_lowercase)
        .collect();
    if !paths
        .iter()
        .any(|path| NOVEL_PREFIXES.iter().any(|prefix| path.starts_with(prefix)))
    {
        return false;
    }
    let mut searchable = format!(
        "{} {}",
        claim.get("owner").and_then(Value::as_str).unwrap_or(""),
        claim
            .get("justification")
            .and_then(Value::as_str)
            .unwrap_or("")
    )
    .to_lowercase();
    for path in paths {
        searchable.push(' ');
        searchable.push_str(&path);
    }
    NOVEL_SIGNALS
        .iter()
        .any(|signal| searchable.contains(signal))
}

pub(super) fn gpu_preflight(input: &Value) -> Result<Value, String> {
    let claims = input
        .get("claims")
        .and_then(Value::as_array)
        .ok_or("GPU claims missing")?;
    let processes = input
        .get("processes")
        .and_then(Value::as_array)
        .ok_or("GPU processes missing")?;
    let loaded_models = rows(text(input, "ollama_ps")?)?;
    let mut blocking_claim_ids: Vec<_> = claims
        .iter()
        .filter(|claim| reserves_novel_gpu(claim))
        .filter_map(|claim| claim.get("claim_id").and_then(Value::as_str))
        .collect();
    blocking_claim_ids.sort_unstable();
    let blocking_processes: Vec<_> = processes
        .iter()
        .filter(|process| {
            let name = process
                .get("process_name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_lowercase();
            !NON_RESEARCH_PROCESSES
                .iter()
                .any(|fragment| name.contains(fragment))
        })
        .collect();
    Ok(
        json!({"ready":blocking_claim_ids.is_empty() && blocking_processes.is_empty() && loaded_models.is_empty(),
        "blocking_claim_ids":blocking_claim_ids,"blocking_processes":blocking_processes,"loaded_models":loaded_models}),
    )
}

pub(super) fn clerk_schema(_input: &Value) -> Result<Value, String> {
    Ok(json!({"type":"object","properties":{
        "status":{"type":"string","enum":["PASS"]},"cells":{"type":"integer","const":5}},
        "required":["status","cells"],"additionalProperties":false}))
}

pub(super) fn clerk_payload(input: &Value) -> Result<Value, String> {
    let schema = input.get("schema").ok_or("clerk schema missing")?;
    let prompt = text(input, "system_prompt")?;
    // Python's compact, key-sorted JSON is passed in to preserve the exact user prompt.
    let schema_text = text(input, "schema_text")?;
    Ok(json!({"model":CLERK_MODEL,"messages":[
        {"role":"system","content":prompt},
        {"role":"user","content":format!(
            "Return only compact JSON matching this schema exactly: {schema_text}. The only valid object is {{\"status\":\"PASS\",\"cells\":5}}.")}
    ],"format":schema,"stream":false,"think":false,"keep_alive":"30s",
    "options":{"num_ctx":2048,"num_gpu":99,"num_predict":32,"presence_penalty":0,
        "seed":0,"temperature":0}}))
}

pub(super) fn clerk_unavailable(input: &Value) -> Result<Value, String> {
    let after = text(input, "after_processes")?;
    let unloaded =
        rows(after).is_ok_and(|after_rows| after_rows.iter().all(|row| !row.contains(CLERK_MODEL)));
    Ok(
        json!({"preflight":input.get("preflight"),"request_error":input.get("request_error"),
        "stop_returncode":input.get("stop_returncode"),"unloaded":unloaded}),
    )
}

fn parse_content(response: &Value) -> Option<Value> {
    response
        .get("message")
        .and_then(|message| message.get("content"))
        .and_then(Value::as_str)
        .and_then(|content| serde_json::from_str(content).ok())
}

fn reported_count(response: &Value, key: &str) -> i64 {
    response
        .get(key)
        .and_then(Value::as_i64)
        .filter(|number| *number >= 0)
        .unwrap_or(-1)
}

fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(value) => value.as_f64().is_some_and(|number| number != 0.0),
        Value::String(value) => !value.is_empty(),
        Value::Array(value) => !value.is_empty(),
        Value::Object(value) => !value.is_empty(),
    }
}

pub(super) fn clerk_adjudicate(input: &Value) -> Result<Value, String> {
    let response = input.get("response").ok_or("clerk response missing")?;
    let resident = text(input, "resident_processes")?;
    let after = text(input, "after_processes")?;
    let stop_returncode = input
        .get("stop_returncode")
        .and_then(Value::as_i64)
        .ok_or("stop returncode missing")?;
    let prompt_tokens = reported_count(response, "prompt_eval_count");
    let eval_tokens = reported_count(response, "eval_count");
    let tokens = prompt_tokens + eval_tokens;
    let (resident_rows, after_rows, ps_valid) = match (rows(resident), rows(after)) {
        (Ok(resident_rows), Ok(after_rows)) => (resident_rows, after_rows, true),
        _ => (
            Vec::new(),
            vec!["invalid ollama ps output".to_owned()],
            false,
        ),
    };
    let gpu_resident = resident_rows
        .iter()
        .any(|row| row.contains(CLERK_MODEL) && row.contains("GPU"));
    let unloaded = after_rows.iter().all(|row| !row.contains(CLERK_MODEL));
    let prohibited: Vec<_> = PROHIBITED
        .iter()
        .filter(|fragment| resident.to_lowercase().contains(**fragment))
        .collect();
    let thinking = response
        .get("message")
        .and_then(|message| message.get("thinking"));
    let thinking_suppressed = thinking.is_none_or(|value| !truthy(value));
    let schema_valid = parse_content(response) == Some(json!({"status":"PASS","cells":5}));
    let generation_bounded = (1..=32).contains(&eval_tokens);
    let ok = response.get("model") == Some(&json!(CLERK_MODEL))
        && schema_valid
        && response.get("done") == Some(&json!(true))
        && response.get("done_reason") == Some(&json!("stop"))
        && thinking_suppressed
        && ps_valid
        && gpu_resident
        && stop_returncode == 0
        && unloaded
        && prohibited.is_empty()
        && tokens > 0
        && generation_bounded;
    let evidence = json!({"preflight":input.get("preflight"),"model":response.get("model"),
        "schema_valid":schema_valid,"reported_tokens":tokens,"prompt_eval_count":prompt_tokens,
        "eval_count":eval_tokens,"done_reason":response.get("done_reason"),"thinking_disabled":true,
        "thinking_suppressed":thinking_suppressed,"num_ctx":2048,"num_gpu":99,"num_predict":32,
        "generation_bounded":generation_bounded,"ollama_ps_valid":ps_valid,"gpu_resident":gpu_resident,
        "unloaded":unloaded,"stop_returncode":stop_returncode,"prohibited_loaded":prohibited,
        "response_sha256":input.get("response_sha256")});
    Ok(json!({"ok":ok,"evidence":evidence}))
}
