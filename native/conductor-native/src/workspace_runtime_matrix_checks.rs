use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const EMBED_MODEL: &str = "qwen3-embed-cpu";
const PROHIBITED: [&str; 2] = ["qwen3.8", "27b"];

fn text<'a>(payload: &'a Value, key: &str) -> Result<&'a str, String> {
    payload
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("matrix {key} must be text"))
}

fn bytes_digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn python_string(value: &str) -> String {
    let quote = if value.contains('\'') && !value.contains('"') {
        '"'
    } else {
        '\''
    };
    let escaped = value
        .replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
        .replace(quote, &format!("\\{quote}"));
    format!("{quote}{escaped}{quote}")
}

fn python_list<'a>(values: impl IntoIterator<Item = &'a str>) -> String {
    format!(
        "[{}]",
        values
            .into_iter()
            .map(python_string)
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn cell(id: &str, status: &str, detail: String, evidence: Value) -> Value {
    json!({"cell_id":id,"status":status,"detail":detail,"required":true,"evidence":evidence})
}

fn hook_config(
    root: &Path,
    launcher: &str,
    relpath: &str,
    expected: &[&str],
) -> (Option<String>, Option<String>) {
    let path = root.join(relpath);
    let source = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) => return (Some(format!("{launcher}:{error}")), None),
    };
    let payload: Value = match serde_json::from_slice(&source) {
        Ok(value @ Value::Object(_)) => value,
        Ok(_) => {
            return (
                Some(format!(
                    "{launcher}:expected JSON object: {}",
                    path.display()
                )),
                None,
            )
        }
        Err(error) => return (Some(format!("{launcher}:{error}")), None),
    };
    let serialized = serde_json::to_string(payload.get("hooks").unwrap_or(&Value::Null))
        .expect("parsed JSON value serializes");
    let missing: Vec<_> = expected
        .iter()
        .filter(|fragment| !serialized.contains(**fragment))
        .collect();
    let issue = if missing.is_empty() {
        None
    } else {
        Some(format!(
            "{launcher}:missing={}",
            python_list(missing.into_iter().copied())
        ))
    };
    (issue, Some(bytes_digest(&source)))
}

pub(super) fn check_hook_configs(input: &Value) -> Result<Value, String> {
    let root = PathBuf::from(text(input, "root")?);
    let controls: [(&str, &str, &[&str]); 4] = [
        (
            "codex",
            ".codex/hooks.json",
            &[
                "pre-edit.sh",
                "crg_gate.py verify",
                "\"Read\"",
                "GOVERNANCE_OWNER",
            ],
        ),
        (
            "claude",
            ".claude/settings.json",
            &[
                "pre-edit.sh",
                "crg_gate.py verify",
                "\"Read\"",
                "GOVERNANCE_OWNER",
            ],
        ),
        (
            "qwen",
            ".qwen/settings.json",
            &[
                "pre-edit.sh",
                "crg_gate.py verify",
                "read_file",
                "run_shell_command",
            ],
        ),
        (
            "grok",
            ".grok/hooks/workspace.json",
            &[
                "pre-edit.sh",
                "crg_gate.py verify",
                "read_file",
                "run_shell_command",
            ],
        ),
    ];
    let mut errors = Vec::new();
    let mut hashes = serde_json::Map::new();
    for (launcher, relpath, fragments) in controls {
        let (error, hash) = hook_config(&root, launcher, relpath, fragments);
        if let Some(error) = error {
            errors.push(error);
        }
        if let Some(hash) = hash {
            hashes.insert(launcher.to_owned(), json!(hash));
        }
    }
    let valid = errors.is_empty();
    Ok(cell(
        "hook-config-contract",
        if valid { "PASS" } else { "FAIL-CLOSED" },
        if valid {
            "native configs bind read, shell, graph, and owner gates".to_owned()
        } else {
            errors.join("; ")
        },
        json!({"config_sha256":hashes}),
    ))
}

pub(super) fn hook_program_cases(_input: &Value) -> Result<Value, String> {
    Ok(json!([
        {"name":"codex-read-deny","payload":{"tool_name":"Read",
            "tool_input":{"file_path":".current_work.md"}},"expect_deny":true},
        {"name":"grok-read-deny","payload":{"hookEventName":"pre_tool_use",
            "toolName":"read_file","toolInput":{"filePath":".current_work.md"}},"expect_deny":true},
        {"name":"shell-read-deny","payload":{"tool_name":"Bash",
            "tool_input":{"command":"sed -n '1,10p' .current_work.md"}},"expect_deny":true},
        {"name":"safe-read-allow","payload":{"tool_name":"Read",
            "tool_input":{"file_path":"README.md"}},"expect_deny":false},
        {"name":"handoff-allow","payload":{"tool_name":"Bash",
            "tool_input":{"command":"python -m conductor.handoff append --owner x --title y --body z"}},
            "expect_deny":false}
    ]))
}

pub(super) fn hook_program_verdict(input: &Value) -> Result<Value, String> {
    let results = input.as_array().ok_or("hook results must be a list")?;
    let mut failures = Vec::new();
    let mut evidence = serde_json::Map::new();
    for result in results {
        let name = text(result, "name")?;
        let code = result
            .get("returncode")
            .and_then(Value::as_i64)
            .ok_or("hook returncode missing")?;
        let denied = text(result, "stdout")?.contains("BLOCKED:");
        let expected = result
            .get("expect_deny")
            .and_then(Value::as_bool)
            .ok_or("hook expectation missing")?;
        evidence.insert(name.to_owned(), json!({"returncode":code,"denied":denied}));
        if code != 0 || denied != expected {
            failures.push(name);
        }
    }
    Ok(json!({"failures":failures,"evidence":evidence}))
}

fn positive_int(value: Option<&Value>) -> bool {
    value
        .and_then(Value::as_i64)
        .is_some_and(|number| number > 0)
}

fn graph_valid(payload: &Value) -> bool {
    let Some(provider) = payload.get("provider").and_then(Value::as_str) else {
        return false;
    };
    let Some(fingerprint) = payload.get("backend_fingerprint").and_then(Value::as_str) else {
        return false;
    };
    let Some(trace) = payload.get("query_trace").and_then(Value::as_object) else {
        return false;
    };
    let dimension = payload.get("dimension");
    let live_nodes = payload.get("live_non_file_node_count");
    provider.starts_with("workspace:")
        && fingerprint.starts_with("sha256:")
        && payload
            .get("model")
            .and_then(Value::as_str)
            .is_some_and(|model| !model.is_empty())
        && positive_int(dimension)
        && payload.get("paid").is_some_and(Value::is_boolean)
        && payload.get("stored_provider") == Some(&json!(provider))
        && matches!(
            payload.get("search_mode").and_then(Value::as_str),
            Some("semantic" | "hybrid")
        )
        && positive_int(payload.get("result_count"))
        && positive_int(payload.get("node_count"))
        && positive_int(live_nodes)
        && payload.get("embedded_node_count") == live_nodes
        && [
            "missing_embedding_count",
            "mixed_provider_live_count",
            "orphan_embedding_count",
        ]
        .iter()
        .all(|key| payload.get(*key) == Some(&json!(0)))
        && payload.get("expected_result_found") == Some(&json!(true))
        && trace.get("provider_name") == Some(&json!(provider))
        && trace.get("backend_fingerprint") == Some(&json!(fingerprint))
        && trace.get("purpose") == Some(&json!("query"))
        && trace.get("vector_count") == Some(&json!(1))
        && positive_int(trace.get("broker_calls"))
        && trace.get("dimension") == dimension
        && trace.get("paid") == payload.get("paid")
}

pub(super) fn check_graph_evidence(input: &Value) -> Result<Value, String> {
    let Some(path) = input.get("path").and_then(Value::as_str) else {
        return Ok(cell(
            "graph-semantic-runtime",
            "NOT_READY",
            "no graph evidence supplied".to_owned(),
            json!({}),
        ));
    };
    let path = Path::new(path);
    if !path.is_file() {
        return Ok(cell(
            "graph-semantic-runtime",
            "NOT_READY",
            "no graph evidence supplied".to_owned(),
            json!({}),
        ));
    }
    let source = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) => {
            return Ok(cell(
                "graph-semantic-runtime",
                "FAIL-CLOSED",
                format!("graph evidence malformed: {error}"),
                json!({}),
            ))
        }
    };
    let mut payload: Value = match serde_json::from_slice(&source) {
        Ok(value @ Value::Object(_)) => value,
        Ok(_) => {
            return Ok(cell(
                "graph-semantic-runtime",
                "FAIL-CLOSED",
                format!(
                    "graph evidence malformed: expected JSON object: {}",
                    path.display()
                ),
                json!({}),
            ))
        }
        Err(error) => {
            return Ok(cell(
                "graph-semantic-runtime",
                "FAIL-CLOSED",
                format!("graph evidence malformed: {error}"),
                json!({}),
            ))
        }
    };
    let valid = graph_valid(&payload);
    payload["source_sha256"] = json!(bytes_digest(&source));
    Ok(cell(
        "graph-semantic-runtime",
        if valid { "PASS" } else { "FAIL-CLOSED" },
        if valid {
            "fingerprint-bound semantic graph query covered the live graph"
        } else {
            "graph fallback or mismatch"
        }
        .to_owned(),
        payload,
    ))
}

pub(super) fn check_embedding(input: &Value) -> Result<Value, String> {
    let health = input.get("health").ok_or("embedding health missing")?;
    let processes = text(input, "processes")?.to_lowercase();
    let prohibited: Vec<_> = PROHIBITED
        .iter()
        .filter(|fragment| processes.contains(**fragment))
        .collect();
    let dimension = input.get("dimension").cloned().unwrap_or(Value::Null);
    let finite = input.get("finite") == Some(&json!(true));
    let valid = health.get("ok") == Some(&json!(true))
        && health.get("model") == Some(&json!(EMBED_MODEL))
        && health.get("num_ctx") == Some(&json!(2048))
        && health.get("keep_alive") == Some(&json!(0))
        && finite
        && dimension == json!(1024)
        && !processes.contains(EMBED_MODEL)
        && prohibited.is_empty();
    Ok(cell(
        "embedding-canary",
        if valid { "PASS" } else { "FAIL-CLOSED" },
        if valid {
            "finite 1024-d vector with bound policy and unload"
        } else {
            "embedding policy or unload failed"
        }
        .to_owned(),
        json!({"model":health.get("model"),"num_ctx":health.get("num_ctx"),"num_gpu":health.get("num_gpu"),
            "keep_alive":health.get("keep_alive"),"dimension":dimension,"finite":finite,"prohibited_loaded":prohibited}),
    ))
}

pub(super) fn check_retrievers(input: &Value) -> Result<Value, String> {
    let results = input
        .as_object()
        .ok_or("retriever results must be an object")?;
    let mut evidence = serde_json::Map::new();
    let mut unavailable = Vec::new();
    let mut invalid = Vec::new();
    for name in ["kb", "memory"] {
        let result = results
            .get(name)
            .ok_or_else(|| format!("retriever result missing: {name}"))?;
        if result.get("timeout") == Some(&json!(true)) {
            unavailable.push(format!("{name}:timeout"));
            continue;
        }
        let code = result
            .get("returncode")
            .and_then(Value::as_i64)
            .ok_or("retriever returncode missing")?;
        if code != 0 {
            unavailable.push(format!("{name}:exit={code}"));
            continue;
        }
        let stdout = text(result, "stdout")?;
        match serde_json::from_str::<Value>(stdout) {
            Ok(Value::Array(rows)) if !rows.is_empty() => {
                evidence.insert(name.to_owned(), json!({"hit_count":rows.len(),
                    "top_path":rows[0].get("path"),"stdout_sha256":bytes_digest(stdout.as_bytes())}));
            }
            Ok(_) => invalid.push(format!("{name}:empty")),
            Err(_) => invalid.push(format!("{name}:malformed")),
        }
    }
    let status = if !invalid.is_empty() {
        "FAIL-CLOSED"
    } else if !unavailable.is_empty() {
        "NOT_READY"
    } else {
        "PASS"
    };
    let detail = if unavailable.is_empty() && invalid.is_empty() {
        "both retrievers returned non-empty JSON".to_owned()
    } else {
        format!(
            "unavailable={}, invalid={}",
            python_list(unavailable.iter().map(String::as_str)),
            python_list(invalid.iter().map(String::as_str))
        )
    };
    Ok(cell(
        "retriever-runtime",
        status,
        detail,
        Value::Object(evidence),
    ))
}

pub(super) fn check_active_state(input: &Value) -> Result<Value, String> {
    let live = input.get("live_ids").ok_or("live claim ids missing")?;
    let cached = input.get("cached_ids").ok_or("cached claim ids missing")?;
    let age = input
        .get("age_seconds")
        .and_then(Value::as_f64)
        .ok_or("active state age missing")?;
    let valid = live == cached && (-60.0..=30.0).contains(&age);
    let detail = if valid {
        "fresh atomic cache agrees with live claim store".to_owned()
    } else {
        let cached_ids = cached.as_array().ok_or("cached claim ids must be a list")?;
        let live_ids = live.as_array().ok_or("live claim ids must be a list")?;
        format!(
            "cached={}, live={}, age={age:.3}s",
            python_list(cached_ids.iter().filter_map(Value::as_str)),
            python_list(live_ids.iter().filter_map(Value::as_str))
        )
    };
    Ok(cell(
        "active-state-live-claims",
        if valid { "PASS" } else { "FAIL-CLOSED" },
        detail,
        json!({"active_state_sha256":input.get("active_state_sha256"),"claim_store_sha256":input.get("claim_store_sha256"),
            "claim_ids":live,"age_seconds":(age * 1_000_000.0).round() / 1_000_000.0}),
    ))
}

pub(super) fn check_launchers(input: &Value) -> Result<Value, String> {
    let missing = input
        .get("missing")
        .and_then(Value::as_array)
        .ok_or("launcher missing list absent")?;
    let versions = input.get("versions").ok_or("launcher versions missing")?;
    Ok(cell(
        "launcher-programs",
        if missing.is_empty() {
            "PASS"
        } else {
            "NOT_READY"
        },
        if missing.is_empty() {
            "all five launcher binaries responded".to_owned()
        } else {
            format!(
                "unavailable={}",
                python_list(missing.iter().filter_map(Value::as_str))
            )
        },
        json!({"versions":versions}),
    ))
}
