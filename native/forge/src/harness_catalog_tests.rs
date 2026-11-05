use super::*;
use crate::tool_schema::ToolRegistry;

// Catalog admission must be covered without launching services or opening the
// operator's artifacts. Paths here are schema inputs, never filesystem fixtures.
fn policy() -> Policy {
    serde_json::from_value(json!({
        "version":1,"source_root":"/source","write_paths":["main.rs"],"read_only":[],
        "programs":{"worker":{"executable":"/bin/true","fixed_arguments":[],
            "environment":{},"working_directory":"scratch"}},
        "aggregate":{"cpu_millis":1000,"memory_mib":1024,"processes":128},
        "session":{"cpu_millis":500,"memory_mib":64,"processes":32},
        "max_sessions":2,"max_retained_sessions":8,"max_tasks":16,
        "lifetime_seconds":45,"session_seconds":30,"retention_seconds":10,
        "replay_bytes":65536,"log_bytes":65536,
        "context":{"tokenizer":{"path":"/tokenizer.json","tokenizer_sha256":"a".repeat(64),
            "model_sha256":"b".repeat(64),"add_special_tokens":false},
            "max_input_bytes":262144,"max_tokens":262144,"recovery_bytes":1048576},
        "code_indexes":{"code":{"directory":"/code","max_rows":10,
            "max_mapped_mib":1,"readers":1,"embedding":{
                "endpoint":"http://127.0.0.1:11434","model":"fixture:v1","runner":"fixture",
                "artifact_sha256":"c".repeat(64),"dimension":2,"context_tokens":128,
                "threads":1,"gpu_layers":0,"timeout_seconds":1}}},
        "workspace":{"identity":null,"max_chars":16384},
        "memory_indexes":{"notes":{"source":"/rows.jsonl","snapshot":"/mapped",
            "fingerprint":format!("sha256:{}","d".repeat(64)),"dimension":2,
            "max_rows":10,"max_mapped_mib":1}}
    }))
    .unwrap()
}

fn registry(policy: &Policy) -> ToolRegistry {
    let registry = ToolRegistry::from_json(&catalog(policy).to_string()).unwrap();
    assert!(registry.grammar().contains("tool-call"));
    registry
}

#[test]
fn optional_catalogs_compile_without_live_services() {
    let mut policy = policy();
    for dimension in [1, 2, super::super::workspace::MAX_MEMORY_DIMENSION] {
        policy.memory_indexes.get_mut("notes").unwrap().dimension = dimension;
        assert!(super::super::workspace::reservation(&policy).is_ok());
        registry(&policy);
    }
    policy.code_indexes.get_mut("code").unwrap().embedding = None;
    registry(&policy);
    policy.workspace = None;
    policy.memory_indexes.clear();
    policy.context = None;
    policy.code_indexes.clear();
    policy.programs.clear();
    policy.write_paths.clear();
    registry(&policy);
}

#[test]
fn workspace_catalog_enforces_query_version_and_configured_dimension_bounds() {
    let mut policy = policy();
    let registry = registry(&policy);
    let mut call = json!({"name":"workspace_recall","arguments":{"index":"notes","task":"task",
        "query":{"schema_version":1,"task_sha256":"a".repeat(64),
            "embedding_fingerprint":format!("sha256:{}","d".repeat(64)),"vector":[1.0,0.0]}}});
    assert!(registry.validate_call(&call.to_string(), false).is_ok());
    for vector in [json!([]), json!([1.0]), json!([1.0, 0.0, 0.0])] {
        call["arguments"]["query"]["vector"] = vector;
        assert!(registry.validate_call(&call.to_string(), false).is_err());
    }
    call["arguments"]["query"]["vector"] = json!([1.0, 0.0]);
    for version in [0, 2] {
        call["arguments"]["query"]["schema_version"] = json!(version);
        assert!(registry.validate_call(&call.to_string(), false).is_err());
    }
    call["arguments"]["query"]["schema_version"] = json!(1);
    let mut larger = policy.memory_indexes["notes"].clone();
    larger.dimension = 4;
    policy.memory_indexes.insert("larger".into(), larger);
    let registry = self::registry(&policy);
    call["arguments"]["index"] = json!("larger");
    call["arguments"]["query"]["vector"] = json!([1.0, 0.0, 0.0, 0.0]);
    assert!(registry.validate_call(&call.to_string(), false).is_ok());
    call["arguments"]["query"]["vector"] = json!([1.0, 0.0, 0.0, 0.0, 0.0]);
    assert!(registry.validate_call(&call.to_string(), false).is_err());
}

#[test]
fn prompt_catalog_requires_context_capability_and_sufficient_bytes() {
    let mut policy = policy();
    for bytes in [128, 511, 512, 8192] {
        policy.context.as_mut().unwrap().max_input_bytes = bytes;
        let catalog = catalog(&policy);
        assert_eq!(
            catalog["tools"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|tool| tool["name"] == "context_prompt")
                .count(),
            usize::from(bytes >= 512)
        );
        registry(&policy);
    }
    policy.context = None;
    let registry = registry(&policy);
    assert!(registry
        .validate_call(
            &json!({"name":"context_prompt","arguments":{
        "mode":"inspect","prompt":"test","max_output_bytes":512}})
            .to_string(),
            false
        )
        .is_err());
}

#[test]
fn prompt_catalog_bounds_modes_fields_and_optional_candidate_without_schema_unions() {
    let mut policy = policy();
    policy.context.as_mut().unwrap().max_input_bytes = 8192;
    let registry = registry(&policy);
    let base = json!({"name":"context_prompt","arguments":{
        "mode":"inspect","prompt":"literal prompt","max_output_bytes":8192}});
    assert!(registry.validate_call(&base.to_string(), false).is_ok());
    for mode in ["inspect", "suggest", "plan", "compare"] {
        let mut call = base.clone();
        call["arguments"]["mode"] = json!(mode);
        call["arguments"]["candidate"] = json!("candidate");
        assert!(registry.validate_call(&call.to_string(), false).is_ok());
    }
    for (key, value) in [
        ("mode", json!("execute")),
        ("candidate", Value::Null),
        ("model", json!("untrusted")),
        ("max_output_bytes", json!(8193)),
        ("constraints", json!(vec!["x"; 33])),
    ] {
        let mut call = base.clone();
        call["arguments"][key] = value;
        assert!(
            registry.validate_call(&call.to_string(), false).is_err(),
            "{call}"
        );
    }
}
