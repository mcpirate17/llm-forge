#![cfg(target_os = "linux")]
//! Adapter parity and operator-budget admission; no client activation claim.
#[path = "support/forge_binary.rs"]
mod forge_binary;
#[path = "support/mcp_client.rs"]
mod mcp_client;
#[path = "support/tokenizer_fixture.rs"]
mod tokenizer_fixture;
use forge::prompt_contract::{self, Request};
use serde_json::{json, Value};
use std::fs;
use std::path::Path;
use std::process::Output;

fn cli(root: &Path, bytes: &[u8]) -> Output {
    let path = root.join("request.json");
    fs::write(&path, bytes).unwrap();
    std::process::Command::new(forge_binary::path())
        .args(["context", "prompt", "--request"])
        .arg(path)
        .env("PATH", "")
        .output()
        .unwrap()
}
fn core(request: &Value) -> Value {
    let request: Request = serde_json::from_value(request.clone()).unwrap();
    serde_json::to_value(prompt_contract::analyze(&request).unwrap()).unwrap()
}
fn request(mode: &str) -> Value {
    let mut request = json!({"mode":mode,"prompt":"Keep literal /tmp/model.rs and `--seed 7`.\nNo deletion.",
        "constraints":["Preserve EXACT Unicode: λ🦀", "Report test evidence"],"max_output_bytes":16384});
    if mode == "compare" {
        request["candidate"] =
            json!("Keep /tmp/model.rs; preserve --seed 7 and report test evidence.");
    }
    request
}

#[test]
fn cli_matches_core_for_all_modes_and_plan_preserves_exact_execution_text() {
    let dir = tempfile::tempdir().unwrap();
    for mode in ["inspect", "suggest", "plan", "compare"] {
        let request = request(mode);
        let output = cli(dir.path(), &serde_json::to_vec(&request).unwrap());
        assert!(output.status.success(), "{output:?}");
        let value: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value, core(&request));
        assert!(output.stdout.len() <= request["max_output_bytes"].as_u64().unwrap() as usize);
        if mode == "plan" {
            assert_eq!(value["execution_contract"]["prompt"], request["prompt"]);
            assert_eq!(
                value["execution_contract"]["constraints"],
                request["constraints"]
            );
        }
    }
}

#[test]
fn cli_accepts_exact_request_byte_boundary_and_emits_compact_unicode_control_json() {
    let dir = tempfile::tempdir().unwrap();
    let mut request = request("plan");
    request["prompt"] = json!("Keep exact Unicode λ🦀 and controls:\n\tthen report evidence.");
    let typed: Request = serde_json::from_value(request.clone()).unwrap();
    let expected = serde_json::to_vec(&prompt_contract::analyze(&typed).unwrap()).unwrap();
    let mut bytes = serde_json::to_vec(&request).unwrap();
    bytes.resize(prompt_contract::MAX_REQUEST_BYTES, b' ');
    let output = cli(dir.path(), &bytes);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, expected);
    bytes.push(b' ');
    let refused = cli(dir.path(), &bytes);
    assert!(!refused.status.success());
    assert!(refused.stdout.is_empty());
}

#[test]
fn cli_rejects_malformed_unknown_null_and_out_of_bound_requests() {
    let dir = tempfile::tempdir().unwrap();
    let mut invalid = vec![
        b"{".to_vec(),
        vec![b' '; prompt_contract::MAX_REQUEST_BYTES + 1],
    ];
    for (key, value) in [
        ("model", json!("untrusted")),
        ("candidate", Value::Null),
        ("candidate", json!("present outside compare")),
        (
            "max_output_bytes",
            json!(prompt_contract::MIN_OUTPUT_BYTES - 1),
        ),
        (
            "max_output_bytes",
            json!(prompt_contract::MAX_OUTPUT_BYTES + 1),
        ),
        (
            "prompt",
            json!("x".repeat(prompt_contract::MAX_TOTAL_TEXT_BYTES + 1)),
        ),
    ] {
        let mut request = request("inspect");
        request[key] = value;
        invalid.push(serde_json::to_vec(&request).unwrap());
    }
    for bytes in invalid {
        let output = cli(dir.path(), &bytes);
        assert!(!output.status.success(), "{output:?}");
        assert!(output.stdout.is_empty());
        assert!(!output.stderr.is_empty());
    }
}

struct PromptService {
    _service: forge::managed_service::Service,
    state: std::path::PathBuf,
    _root: tempfile::TempDir,
}
impl PromptService {
    async fn start(config: Option<&forge::tokenizer::Config>) -> Self {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let state = root.path().join("private");
        fs::create_dir(&source).unwrap();
        git2::Repository::init(&source).unwrap();
        fs::create_dir(&state).unwrap();
        fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
        let document = json!({"version":1,"source_root":source,"write_paths":[],"read_only":[],"programs":{},
            "aggregate":{"cpu_millis":1000,"memory_mib":1024,"processes":128},
            "session":{"cpu_millis":500,"memory_mib":64,"processes":32},
            "max_sessions":1,"max_retained_sessions":2,"max_tasks":4,"lifetime_seconds":30,"session_seconds":20,
            "retention_seconds":5,"replay_bytes":65536,"log_bytes":65536,
            "context":config.map(|config| json!({"tokenizer":config,"max_input_bytes":8192,
                "max_tokens":2048,"recovery_bytes":1048576}))});
        let policy: forge::harness::policy::Policy =
            serde_json::from_value(document.clone()).unwrap();
        policy.clone().validate(&state).unwrap();
        fs::write(
            state.join("policy.json"),
            serde_json::to_vec(&document).unwrap(),
        )
        .unwrap();
        let service = forge::managed_service::Service::launch(
            Path::new(forge_binary::path()),
            &[
                "harness".into(),
                "serve".into(),
                "--state".into(),
                state.as_os_str().into(),
            ],
            policy.aggregate,
            policy.lifetime_seconds,
            &std::collections::BTreeMap::new(),
        )
        .unwrap();
        let running = Self {
            _service: service,
            state,
            _root: root,
        };
        tokio::time::timeout(std::time::Duration::from_secs(8), async {
            while !running.state.join("ready.json").exists() {
                assert!(
                    !running.state.join("failure.json").exists(),
                    "service failed to start"
                );
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        running
    }
}

async fn mcp_client(state: &Path) -> mcp_client::Mcp {
    let mut client = mcp_client::Mcp::new(Path::new(forge_binary::path()), &state.join("socket"));
    client
        .send(
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
        "protocolVersion":"2025-11-25","capabilities":{},
        "clientInfo":{"name":"native-prompt-contract-test","version":"1"}}}),
        )
        .await;
    let response = client.receive().await;
    assert_eq!(response["id"], 1);
    assert_eq!(response["result"]["protocolVersion"], "2025-11-25");
    client
        .send(json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
        .await;
    client
}

async fn mcp_call(client: &mut mcp_client::Mcp, id: u64, request: Value) -> Value {
    client
        .send(
            json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{
        "name":"context_prompt","arguments":request}}),
        )
        .await;
    let response = client.receive().await;
    assert_eq!(response["id"], id);
    response
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "starts sequential owned bounded services and raw MCP stdio clients; requires service-manager support"]
async fn prompt_contract_matches_core_over_mcp_stdio_and_enforces_operator_capabilities() {
    let tokenizer = tokenizer_fixture::Fixture::new();
    let service = PromptService::start(Some(&tokenizer.config)).await;
    let mut client = mcp_client(&service.state).await;
    client
        .send(json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}))
        .await;
    let listed = client.receive().await;
    assert_eq!(listed["id"], 2);
    let tools = listed["result"]["tools"].as_array().unwrap();
    let prompt_tools: Vec<_> = tools
        .iter()
        .filter(|tool| tool["name"] == "context_prompt")
        .collect();
    assert_eq!(prompt_tools.len(), 1);
    let schema = &prompt_tools[0]["inputSchema"];
    assert_eq!(schema["additionalProperties"], false);
    assert_eq!(
        schema["properties"]["mode"]["enum"],
        json!(["inspect", "suggest", "plan", "compare"])
    );
    assert_eq!(schema["properties"]["max_output_bytes"]["maximum"], 8192);
    assert_eq!(schema["properties"]["candidate"]["type"], "string");
    for (offset, mode) in ["inspect", "suggest", "plan", "compare"]
        .into_iter()
        .enumerate()
    {
        let mut request = request(mode);
        request["max_output_bytes"] = json!(8192);
        let response = mcp_call(&mut client, offset as u64 + 3, request.clone()).await;
        assert_eq!(response["result"]["isError"], false, "{response}");
        assert_eq!(
            response["result"]["structuredContent"]["result"],
            core(&request)
        );
        let text = response["result"]["content"][0]["text"].as_str().unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(text).unwrap(),
            response["result"]["structuredContent"]
        );
    }
    let mut denied = Vec::new();
    for (key, value) in [
        ("max_output_bytes", json!(8193)),
        ("candidate", json!("present outside compare")),
        ("candidate", Value::Null),
        ("prompt", json!("🦀".repeat(2100))),
        (
            "constraints",
            json!(["a".repeat(4000), "b".repeat(4000), "c".repeat(4000)]),
        ),
    ] {
        let mut request = request("inspect");
        request["max_output_bytes"] = json!(8192);
        request[key] = value;
        denied.push(request);
    }
    let mut missing_candidate = request("compare");
    missing_candidate["max_output_bytes"] = json!(8192);
    missing_candidate
        .as_object_mut()
        .unwrap()
        .remove("candidate");
    denied.push(missing_candidate);
    for (offset, request) in denied.into_iter().enumerate() {
        let response = mcp_call(&mut client, offset as u64 + 20, request).await;
        assert_eq!(response["result"]["isError"], true, "{response}");
    }
    client.finish().await;
    drop(service);

    // A separate operator policy lacks context authority altogether. The native
    // MCP catalog omits the tool and direct naming cannot grant that authority.
    let service = PromptService::start(None).await;
    let mut client = mcp_client(&service.state).await;
    client
        .send(json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}))
        .await;
    let listed = client.receive().await;
    assert_eq!(listed["id"], 2);
    assert!(!listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|tool| tool["name"] == "context_prompt"));
    let response = mcp_call(&mut client, 3, request("inspect")).await;
    assert_eq!(response["error"]["code"], -32602, "{response}");
    client.finish().await;
}
