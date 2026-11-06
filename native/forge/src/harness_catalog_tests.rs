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

#[test]
fn only_reviewed_source_reads_advertise_effect_annotations() {
    let mut policy = policy();
    // A name resembling a source read must not classify an arbitrary executable.
    policy.outbound_mcp = Some(serde_json::from_value(json!({
        "public_name":"outbound_code_inspect","remote_name":"read_file",
        "executable":{"path":"/server","sha256":"e".repeat(64)},
        "fixed_arguments":[],"pinned_inputs":[],"read_only":[],
        "input_schema_json":"{\"type\":\"object\",\"properties\":{},\"additionalProperties\":false}",
        "resources":{"cpu_millis":10,"memory_mib":16,"processes":1},"max_wall_seconds":1
    })).unwrap());
    let catalog = catalog(&policy);
    let expected = json!({"readOnlyHint":true,"destructiveHint":false,
        "openWorldHint":false,"idempotentHint":true});
    let mut annotated = Vec::new();
    for tool in catalog["tools"].as_array().unwrap() {
        let name = tool["name"].as_str().unwrap();
        if matches!(name, "code_inspect" | "code_fold") {
            assert_eq!(tool["annotations"], expected, "{name}");
            annotated.push(name);
        } else {
            assert!(
                tool.get("annotations").is_none(),
                "unreviewed hints: {name}"
            );
        }
    }
    assert_eq!(annotated, ["code_inspect", "code_fold"]);
    for name in [
        "code_replace",
        "code_diff",
        "code_restore",
        "terminal_spawn",
        "terminal_write",
        "outbound_code_inspect",
    ] {
        assert!(catalog["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["name"] == name));
    }
    assert!(
        tool("code_inspect_unreviewed", "unknown behavior", vec![], &[])
            .get("annotations")
            .is_none()
    );
}

#[test]
fn source_annotations_preserve_registry_grammar_and_call_validation() {
    let annotated = catalog(&policy());
    let mut plain = annotated.clone();
    for tool in plain["tools"].as_array_mut().unwrap() {
        tool.as_object_mut().unwrap().remove("annotations");
    }
    let with = ToolRegistry::from_json(&annotated.to_string()).unwrap();
    let without = ToolRegistry::from_json(&plain.to_string()).unwrap();
    assert_eq!(with.grammar(), without.grammar());
    for name in ["code_inspect", "code_fold"] {
        let valid = json!({"name":name,"arguments":{"path":"main.rs","max_bytes":8192}});
        let input = valid.to_string();
        let admitted = with.validate_call(&input, false).unwrap();
        let original = without.validate_call(&input, false).unwrap();
        assert_eq!(admitted.name(), original.name());
        assert_eq!(admitted.arguments(), original.arguments());
        let mut invalid = valid;
        invalid["arguments"]["apply"] = json!(true);
        assert!(with.validate_call(&invalid.to_string(), false).is_err());
        assert!(without.validate_call(&invalid.to_string(), false).is_err());
    }
    assert!(with
        .validate_call(
            r#"{"name":"code_inspect_unreviewed","arguments":{"path":"main.rs"}}"#,
            false
        )
        .is_err());
}

#[test]
fn registry_annotations_are_optional_strict_boolean_metadata_without_defaults() {
    let base = json!({"tools":[tool("fixture", "unclassified", vec![], &[])]});
    let original = ToolRegistry::from_json(&base.to_string()).unwrap();
    for annotations in [
        json!({}),
        json!({"readOnlyHint":false}),
        json!({"readOnlyHint":true,"destructiveHint":false,"idempotentHint":true,"openWorldHint":false}),
    ] {
        let mut value = base.clone();
        value["tools"][0]["annotations"] = annotations;
        let registry = ToolRegistry::from_json(&value.to_string()).unwrap();
        assert_eq!(registry.grammar(), original.grammar());
        assert!(registry
            .validate_call(r#"{"name":"unknown","arguments":{}}"#, false)
            .is_err());
    }
    for annotations in [
        Value::Null,
        json!(true),
        json!([]),
        json!({"title":"read-only"}),
        json!({"readOnlyHint":"true"}),
        json!({"destructiveHint":0}),
        json!({"openWorldHint":null}),
        json!({"idempotentHint":[]}),
    ] {
        let mut value = base.clone();
        value["tools"][0]["annotations"] = annotations;
        assert!(
            ToolRegistry::from_json(&value.to_string()).is_err(),
            "{value}"
        );
    }
    let mut unknown = base.clone();
    unknown["tools"][0]["approval"] = json!(true);
    assert!(ToolRegistry::from_json(&unknown.to_string()).is_err());
    let duplicate = base.to_string().replace(
        "\"inputSchema\":",
        "\"annotations\":{\"readOnlyHint\":true,\"readOnlyHint\":false},\"inputSchema\":",
    );
    assert!(ToolRegistry::from_json(&duplicate).is_err());
}
