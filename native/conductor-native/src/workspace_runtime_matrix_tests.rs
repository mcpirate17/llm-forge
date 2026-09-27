use super::dispatch;
use serde_json::{json, Value};
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

fn call(operation: &str, input: Value) -> Value {
    dispatch(operation, &input).expect("native matrix operation must succeed")
}

fn temporary_dir(name: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "conductor-matrix-{name}-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir_all(&path).unwrap();
    path
}

fn valid_graph() -> Value {
    json!({
        "provider":"workspace:provider-fingerprint","backend_fingerprint":"sha256:backend-fingerprint",
        "stored_provider":"workspace:provider-fingerprint","model":"local-model","dimension":1024,
        "paid":false,"search_mode":"hybrid","result_count":2,"node_count":3,
        "live_non_file_node_count":2,"embedded_node_count":2,"missing_embedding_count":0,
        "mixed_provider_live_count":0,"orphan_embedding_count":0,"expected_result_found":true,
        "query_trace":{"provider_name":"workspace:provider-fingerprint",
            "backend_fingerprint":"sha256:backend-fingerprint","purpose":"query",
            "vector_count":1,"broker_calls":1,"dimension":1024,"paid":false}
    })
}

#[test]
fn required_status_precedence_and_optional_failure() {
    let cells = json!([
        {"status":"PASS","required":true},
        {"status":"FAIL-CLOSED","required":false},
        {"status":"NOT_READY","required":true}
    ]);
    assert_eq!(call("aggregate_status", cells), "NOT_READY");
    assert_eq!(
        call("aggregate_status", json!([{"status":"FAIL-CLOSED"}])),
        "FAIL-CLOSED"
    );
}

#[test]
fn cumulative_usage_ignores_cached_breakdown_and_repeated_final() {
    let output = [
        r#"{"usage":{"input_tokens":11,"cached_input_tokens":9,"output_tokens":7}}"#,
        "ordinary launcher output",
        r#"{"type":"result","usage":{"input_tokens":21,"output_tokens":4,"total_tokens":25}}"#,
        r#"{"usage":{"input_tokens":11,"output_tokens":7}}"#,
    ]
    .join("\n");
    assert_eq!(call("extract_reported_tokens", json!(output)), 25);
}

#[test]
fn graph_requires_semantic_trace_and_matching_live_provider() {
    let dir = temporary_dir("graph");
    let path = dir.join("graph.json");
    let mut graph = valid_graph();
    fs::write(&path, graph.to_string()).unwrap();
    let input = json!({"path":path.to_string_lossy()});
    let pass = call("check_graph_evidence", input.clone());
    assert_eq!(pass["status"], "PASS");
    assert_eq!(
        pass["evidence"]["source_sha256"].as_str().unwrap().len(),
        64
    );
    graph["mixed_provider_live_count"] = json!(1);
    fs::write(&path, graph.to_string()).unwrap();
    assert_eq!(
        call("check_graph_evidence", input.clone())["status"],
        "FAIL-CLOSED"
    );
    graph["mixed_provider_live_count"] = json!(0);
    graph["query_trace"]["dimension"] = json!(768);
    fs::write(&path, graph.to_string()).unwrap();
    assert_eq!(call("check_graph_evidence", input)["status"], "FAIL-CLOSED");
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn receipt_replacement_preserves_expensive_cell_and_provenance() {
    let receipt = json!({"status":"FAIL-CLOSED","cells":[
        {"cell_id":"graph-semantic-runtime","status":"FAIL-CLOSED","required":true,"evidence":{"old":1}},
        {"cell_id":"expensive-cell","status":"PASS","required":true,"evidence":{"tokens":99}}
    ],"provenance":{"git_head":"abc"}});
    let replacement = json!({"cell_id":"graph-semantic-runtime","status":"PASS","required":true,"evidence":{"new":2}});
    let output = call(
        "replace_receipt_cells",
        json!({"receipt":receipt,"single_cell":"graph-semantic-runtime",
        "replacements":{"graph-semantic-runtime":replacement}}),
    );
    assert_eq!(output["status"], "PASS");
    assert_eq!(output["cells"][1]["evidence"]["tokens"], 99);
    assert_eq!(output["provenance"]["git_head"], "abc");
}

#[test]
fn gpu_preflight_blocks_research_claims_and_active_compute() {
    assert!(dispatch(
        "parse_gpu_processes",
        &json!({"stdout":"42, python, 4096 MiB"})
    )
    .is_err());
    let processes = call(
        "parse_gpu_processes",
        json!({"stdout":"42, python, 4096\n43, gnome-shell, 512"}),
    );
    let preflight = call(
        "gpu_preflight",
        json!({
            "claims":[{"claim_id":"claim-a","owner":"researcher","justification":"AVO throughput",
                "paths":["research/tools/model.py"]}],
            "processes":processes,"ollama_ps":"NAME ID SIZE PROCESSOR\nother-model id 1GB 100% GPU"
        }),
    );
    assert_eq!(preflight["ready"], false);
    assert_eq!(preflight["blocking_claim_ids"], json!(["claim-a"]));
    assert_eq!(preflight["blocking_processes"].as_array().unwrap().len(), 1);
    assert_eq!(preflight["loaded_models"].as_array().unwrap().len(), 1);
}

#[test]
fn clerk_requires_bounded_schema_gpu_residency_and_unload() {
    let response = json!({"model":"qwen3.5:9b","message":{"content":"{\"status\":\"PASS\",\"cells\":5}","thinking":""},
        "done":true,"done_reason":"stop","prompt_eval_count":20,"eval_count":9});
    let resident = "NAME ID SIZE PROCESSOR\nqwen3.5:9b id 6.6GB 100% GPU";
    let input = json!({"preflight":{"ready":true},"response":response,
        "resident_processes":resident,"after_processes":"NAME ID SIZE PROCESSOR",
        "stop_returncode":0,"response_sha256":"digest"});
    let pass = call("clerk_adjudicate", input.clone());
    assert_eq!(pass["ok"], true);
    assert_eq!(pass["evidence"]["generation_bounded"], true);
    let mut bad = input;
    bad["response"]["eval_count"] = json!(33);
    assert_eq!(call("clerk_adjudicate", bad.clone())["ok"], false);
    bad["response"]["eval_count"] = json!(9);
    bad["after_processes"] = json!(resident);
    assert_eq!(call("clerk_adjudicate", bad)["ok"], false);
}

#[test]
fn hook_config_check_requires_every_guard_fragment() {
    let root = temporary_dir("hooks");
    for relpath in [
        ".codex/hooks.json",
        ".claude/settings.json",
        ".qwen/settings.json",
        ".grok/hooks/workspace.json",
    ] {
        let path = root.join(relpath);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, r#"{"hooks":{"Read":"pre-edit.sh crg_gate.py verify GOVERNANCE_OWNER read_file run_shell_command"}}"#).unwrap();
    }
    let input = json!({"root":root.to_string_lossy()});
    assert_eq!(call("check_hook_configs", input.clone())["status"], "PASS");
    fs::write(root.join(".grok/hooks/workspace.json"), r#"{"hooks":{}}"#).unwrap();
    assert_eq!(call("check_hook_configs", input)["status"], "FAIL-CLOSED");
    fs::remove_dir_all(root).unwrap();
}

#[cfg(feature = "python")]
#[test]
fn pyo3_json_boundary_preserves_status_and_rejects_unknown_operation() {
    pyo3::Python::initialize();
    pyo3::Python::attach(|_| {
        let output = super::workspace_runtime_matrix_native(
            "aggregate_status",
            r#"[{"status":"NOT_READY","required":true}]"#,
        )
        .unwrap();
        assert_eq!(output, r#""NOT_READY""#);
        assert!(super::workspace_runtime_matrix_native("unknown", "{}").is_err());
    });
}
