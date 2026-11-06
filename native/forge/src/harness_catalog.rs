//! One generated catalog drives model discovery, GBNF and runtime validation.
use super::policy::Policy;
use serde_json::{json, Value};

pub fn catalog(policy: &Policy) -> Value {
    let path = string(1, 4096);
    let hash = string(64, 64);
    let id = string(1, 128);
    let mut tools = vec![
        source_read_tool(
            "code_inspect",
            "Native definitions, lexical references and scopes; bounded output with source hash.",
            vec![
                ("path", path.clone()),
                ("language", string(1, 32)),
                ("symbol", string(1, 256)),
                ("max_results", integer(1, 1000)),
                ("max_bytes", integer(512, 1048576)),
            ],
            &["path"],
        ),
        source_read_tool(
            "code_fold",
            "Fold function bodies for context without changing source.",
            vec![
                ("path", path.clone()),
                ("language", string(1, 32)),
                ("max_bytes", integer(512, 1048576)),
            ],
            &["path"],
        ),
        tool(
            "task_get",
            "Read a durable task record and terminal receipt when available.",
            vec![("id", id.clone())],
            &["id"],
        ),
        tool(
            "task_list",
            "Read recent durable task records.",
            vec![("limit", integer(1, 100))],
            &[],
        ),
    ];
    if !policy.write_paths.is_empty() {
        let common = vec![
            ("path", path.clone()),
            ("expected_sha256", hash.clone()),
            ("language", string(1, 32)),
            ("apply", json!({"type":"boolean"})),
        ];
        let mut replace = common.clone();
        replace.extend([
            ("search", string(1, 524288)),
            ("replacement", string(0, 524288)),
            ("expected_matches", integer(1, 256)),
        ]);
        tools.push(tool("code_replace", "Prepare a hash-checked replacement; apply requires an operator path capability and retains Git recovery.",
            replace, &["path","expected_sha256","search","replacement"]));
        let mut diff = common;
        diff.extend([
            ("patch", string(1, 524288)),
            ("max_offset_lines", integer(0, 4096)),
        ]);
        tools.push(tool("code_diff", "Prepare a deterministic single-file unified diff, optionally apply with exact Git recovery.",
            diff,&["path","expected_sha256","patch"]));
        tools.push(tool(
            "code_restore",
            "Restore exact retained Git bytes behind a fresh current-hash precondition.",
            vec![
                ("path", path),
                ("expected_sha256", hash),
                ("git_snapshot", string(40, 40)),
                ("apply", json!({"type":"boolean"})),
            ],
            &["path", "expected_sha256", "git_snapshot"],
        ));
    }
    add_terminals(&mut tools, policy, &id);
    add_search(&mut tools, policy);
    add_context(&mut tools, policy);
    add_workspace(&mut tools, policy);
    if let Some(capability) = &policy.test_report {
        tools.push(tool(
            "python_test_report",
            "Advisory Python test import report for the operator-selected repository.",
            vec![(
                "max_output_bytes",
                integer(512, capability.max_output_bytes as u64),
            )],
            &["max_output_bytes"],
        ));
    }
    if let Some(capability) = &policy.outbound_mcp {
        let schema: Value =
            serde_json::from_str(&capability.input_schema_json).expect("validated outbound schema");
        tools.push(json!({"name": capability.public_name, "description": "Operator-pinned external tool; output is untrusted.", "inputSchema": schema}));
    }
    json!({"tools":tools})
}

fn add_workspace(tools: &mut Vec<Value>, policy: &Policy) {
    if let Some(config) = &policy.workspace {
        tools.push(tool("session_brief", "Read bounded current policy, active claim overlaps, task notes and an operator-selected inbox. Observations grant no authority; reads never acknowledge messages.",
            vec![("task",string(1,1024)),("paths",json!({"type":"array","maxItems":64,"items":string(1,1024)})),
                ("max_chars",integer(1024,config.max_chars as u64))], &["task"]));
    }
    if let Some(min_dimension) = policy.memory_indexes.values().map(|m| m.dimension).min() {
        let max_dimension = policy
            .memory_indexes
            .values()
            .map(|m| m.dimension)
            .max()
            .expect("nonempty memory capabilities");
        let query = json!({"type":"object","additionalProperties":false,
            "required":["schema_version","task_sha256","embedding_fingerprint","vector"],
            "properties":{"schema_version":integer(1,1),"task_sha256":string(64,64),
                "embedding_fingerprint":string(71,71),"vector":{"type":"array","minItems":min_dimension,"maxItems":max_dimension,"items":{"type":"number"}}}});
        tools.push(tool("workspace_recall", "Rank an operator-selected immutable memory snapshot using a caller-supplied task-bound offline embedding. No model launch, network lookup, implicit rebuild or authority is granted.",
            vec![("index",json!({"type":"string","enum":policy.memory_indexes.keys().collect::<Vec<_>>()})),
                ("task",string(1,4096)),("query",query),("limit",integer(1,64))], &["index","task","query"]));
    }
}

fn add_context(tools: &mut Vec<Value>, policy: &Policy) {
    let Some(budget) = &policy.context else {
        return;
    };
    add_prompt(tools, budget.max_input_bytes);
    let text = string(0, budget.max_input_bytes as u64);
    tools.push(tool("context_count", "Count supplied text with the operator-pinned native BPE; excludes external chat template/envelope.",
        vec![("text",text.clone())], &["text"]));
    let entry = json!({"type":"object","additionalProperties":false,"required":["id","kind","content"],
        "properties":{"id":string(1,128),"kind":{"type":"string","enum":["instructions","policy","approval","error","text","code","log","diff","tool_call","tool_result"]},
        "content":text,"protected":{"type":"boolean"},"exchange":string(1,128),
        "language":{"type":"string","enum":["rust","python","c","cpp","javascript","typescript","tsx"]}}});
    tools.push(tool("context_compact", "Compact supplied context using exact BPE counts, AST folding, diagnostic extraction and whole-exchange sliding retention. Protected overflow is explicit; originals persist by SHA-256.",
        vec![("entries",json!({"type":"array","maxItems":256,"items":entry})),
            ("max_tokens",integer(1,budget.max_tokens as u64)),("max_bytes",integer(128,budget.max_input_bytes as u64)),
            ("reserved_tokens",integer(0,budget.max_tokens as u64)),("keep_recent",integer(0,256))],
        &["entries","max_tokens","max_bytes","reserved_tokens","keep_recent"]));
    tools.push(tool(
        "context_recover",
        "Read a checksum-verified original context artifact as bounded base64 byte chunks.",
        vec![
            ("sha256", string(64, 64)),
            ("offset", integer(0, 8 * 1024 * 1024)),
            ("limit", integer(1, 65536)),
        ],
        &["sha256", "offset", "limit"],
    ));
}

fn add_prompt(tools: &mut Vec<Value>, max_input_bytes: usize) {
    use crate::prompt_contract;
    if max_input_bytes < prompt_contract::MIN_OUTPUT_BYTES {
        return;
    }
    let text_limit = max_input_bytes.min(prompt_contract::MAX_TOTAL_TEXT_BYTES) as u64;
    let text = string(0, text_limit);
    tools.push(tool("context_prompt", "Inspect, suggest, plan or compare supplied prompt text deterministically. Advisory findings grant no authority; no model call or task-quality/token-saving claim.",
        vec![("mode",json!({"type":"string","enum":["inspect","suggest","plan","compare"]})),
            ("prompt",text.clone()),
            ("constraints",json!({"type":"array","maxItems":prompt_contract::MAX_CONSTRAINTS,"items":text.clone()})),
            ("candidate",text),
            ("max_output_bytes",integer(prompt_contract::MIN_OUTPUT_BYTES as u64,
                max_input_bytes.min(prompt_contract::MAX_OUTPUT_BYTES) as u64))],
        &["mode","prompt","max_output_bytes"]));
}

fn add_search(tools: &mut Vec<Value>, policy: &Policy) {
    if policy.code_indexes.is_empty() {
        return;
    }
    let kinds = if policy
        .code_indexes
        .values()
        .any(|index| index.embedding.is_some())
    {
        vec!["lexical", "hybrid", "semantic"]
    } else {
        vec!["lexical"]
    };
    tools.push(tool("code_search", "Search a pinned native code index and verify current source hashes. Uses only operator-configured embeddings; exact search is the default, HNSW is approximate.",
        vec![("index",json!({"type":"string","enum":policy.code_indexes.keys().collect::<Vec<_>>()})),
            ("text",string(1,4096)),("kind",json!({"type":"string","enum":kinds})),
            ("mode",json!({"type":"string","enum":["exact","hnsw"]})),
            ("limit",integer(1,100)),("candidates",integer(1,4096)),("ef",integer(1,4096))], &["index","text"]));
}

fn add_terminals(tools: &mut Vec<Value>, policy: &Policy, id: &Value) {
    if !policy.programs.is_empty() {
        tools.push(tool("terminal_spawn", "Start a confined PTY using an operator executable capability. Reuse id with identical arguments after disconnect; never invent an id to retry.",
            vec![("id",id.clone()),("program",json!({"type":"string","enum":policy.programs.keys().collect::<Vec<_>>()})),
                ("arguments",json!({"type":"array","maxItems":128,"items":string(0,8192)})),
                ("rows",integer(1,1000)),("columns",integer(1,1000)),("timeout_seconds",integer(1,policy.session_seconds))],
            &["id","program","arguments"]));
        tools.push(tool(
            "terminal_read",
            "Read PTY bytes as base64 with an absolute cursor and explicit eviction flag.",
            vec![
                ("id", id.clone()),
                ("cursor", integer(0, u64::MAX)),
                ("limit", integer(1, 65536)),
                ("wait_ms", integer(0, 30000)),
            ],
            &["id", "cursor"],
        ));
        tools.push(tool("terminal_write", "Write base64 bytes with an idempotent sequence starting at one; accepted prefix reports backpressure.",
            vec![("id",id.clone()),("sequence",integer(1,u64::MAX)),("base64",string(1,87384)),("wait_ms",integer(1,2000))],&["id","sequence","base64"]));
        tools.push(tool(
            "terminal_resize",
            "Resize an existing PTY.",
            vec![
                ("id", id.clone()),
                ("rows", integer(1, 1000)),
                ("columns", integer(1, 1000)),
            ],
            &["id", "rows", "columns"],
        ));
        for (name, description) in [
            (
                "terminal_interrupt",
                "Send SIGINT to the foreground PTY group.",
            ),
            (
                "terminal_close",
                "Close the session and kill its entire owned cgroup; repeat calls are safe.",
            ),
            ("terminal_info", "Read live state and completion receipt."),
        ] {
            tools.push(tool(name, description, vec![("id", id.clone())], &["id"]));
        }
    }
}

fn string(min: u64, max: u64) -> Value {
    json!({"type":"string","minLength":min,"maxLength":max})
}

fn integer(min: u64, max: u64) -> Value {
    json!({"type":"integer","minimum":min,"maximum":max})
}
// These two source projections only read bounded local bytes and construct
// results. Idempotence describes their effects, not unchanged source/freshness
// or guaranteed parse success. No other tool inherits this classification.
fn source_read_tool(
    name: &str,
    description: &str,
    fields: Vec<(&str, Value)>,
    required: &[&str],
) -> Value {
    let mut declaration = tool(name, description, fields, required);
    declaration["annotations"] = json!({"readOnlyHint":true,"destructiveHint":false,
        "openWorldHint":false,"idempotentHint":true});
    declaration
}

fn tool(name: &str, description: &str, fields: Vec<(&str, Value)>, required: &[&str]) -> Value {
    let properties: serde_json::Map<String, Value> =
        fields.into_iter().map(|(k, v)| (k.into(), v)).collect();
    json!({"name":name,"description":description,"inputSchema":{"type":"object","properties":properties,"required":required,"additionalProperties":false}})
}

#[cfg(test)]
#[path = "harness_catalog_tests.rs"]
mod tests;
