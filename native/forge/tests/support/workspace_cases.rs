use super::*;
use conductor_native::memory_index_sidecar;

fn fingerprint() -> String {
    format!("sha256:{}", "b".repeat(64))
}
fn row(text: &str) -> String {
    json!({"schema_version":3,"embedding":{"fingerprint":fingerprint(),"dimension":2,"paid":true},
        "source_sha256":"a".repeat(64),"source":"notes","path":"native.md","title":"Native workspace","text":text,"vector":[1.0,0.0]}).to_string()
}
fn configure(host: &support::Host, _: &Path, policy: &mut Value) {
    let memory = host.0.join("memory");
    fs::create_dir(&memory).unwrap();
    fs::set_permissions(&memory, fs::Permissions::from_mode(0o700)).unwrap();
    let source = memory.join("rows.jsonl");
    let snapshot = memory.join("mapped");
    fs::write(&source, row("original context")).unwrap();
    memory_index_sidecar::build_sidecar(&source, &snapshot).unwrap();
    policy["workspace"] = json!({"identity":"native-agent","max_chars":2048});
    policy["memory_indexes"] = json!({"notes":{"source":source,"snapshot":snapshot,"fingerprint":fingerprint(),"dimension":2,"max_rows":10,"max_mapped_mib":1}});
}
fn recall() -> Value {
    json!({"index":"notes","task":"native","query":{"schema_version":1,"task_sha256":format!("{:x}",Sha256::digest(b"native")),"embedding_fingerprint":fingerprint(),"vector":[1.0,0.0]},"limit":1})
}

#[test]
fn workspace_policy_rejects_unsafe_paths_spaces_counts_and_unfunded_readers() {
    let host = support::Host::new();
    let (source, state, mut policy) = fixture(&host);
    configure(&host, &source, &mut policy);
    assert!(serde_json::from_value::<Policy>(policy.clone())
        .unwrap()
        .validate(&state)
        .is_ok());
    for dimension in [4097, 8192] {
        let mut bad = policy.clone();
        bad["memory_indexes"]["notes"]["dimension"] = json!(dimension);
        let error = serde_json::from_value::<Policy>(bad)
            .unwrap()
            .validate(&state)
            .unwrap_err();
        assert!(
            error.to_string().contains("supported range 1..=4096"),
            "{error:#}"
        );
    }
    for (field, value) in [
        ("dimension", json!(0)),
        ("max_rows", json!(0)),
        ("max_mapped_mib", json!(2049)),
        ("fingerprint", json!("sha256:bad")),
        ("dimension", json!(3)),
        ("source", json!("relative.jsonl")),
        (
            "snapshot",
            policy["memory_indexes"]["notes"]["source"].clone(),
        ),
    ] {
        let mut bad = policy.clone();
        bad["memory_indexes"]["notes"][field] = value;
        assert!(
            serde_json::from_value::<Policy>(bad)
                .unwrap()
                .validate(&state)
                .is_err(),
            "{field}"
        );
    }
    for brief in [
        json!({"identity":"../escape","max_chars":2048}),
        json!({"identity":null,"max_chars":16385}),
    ] {
        let mut bad = policy.clone();
        bad["workspace"] = brief;
        assert!(serde_json::from_value::<Policy>(bad)
            .unwrap()
            .validate(&state)
            .is_err());
    }
    let mut bad = policy.clone();
    bad["aggregate"]["memory_mib"] = 512.into();
    assert!(serde_json::from_value::<Policy>(bad)
        .unwrap()
        .validate(&state)
        .is_err());
    let link = host.0.join("link");
    std::os::unix::fs::symlink(host.0.join("memory/rows.jsonl"), &link).unwrap();
    policy["memory_indexes"]["notes"]["source"] = json!(link);
    assert!(serde_json::from_value::<Policy>(policy)
        .unwrap()
        .validate(&state)
        .is_err());
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "starts an owned bounded transient service"]
async fn workspace_tools_share_native_context_without_acknowledging_mail() {
    let service = Running::configured(|host, source, policy| {
        configure(host, source, policy);
        fs::write(source.join("forge.toml"), "schema_version=1\n[conductor.session]\npreamble=[]\nstanding_mandates=['KB-NATIVE: Preserve native execution']\n").unwrap();
        let mut mailbox=conductor_native::a2a_store::Store::initialize(&source.join(".agents/a2a"),"native-agent").unwrap();
        mailbox.record_inbound("fixture-message","fixture-peer","native-agent","Review native context",None,"2026-10-31T00:00:00+00:00").unwrap();
    }).await;
    let catalog = service.request(json!({"method":"catalog"})).await;
    for name in ["session_brief", "workspace_recall"] {
        assert!(catalog["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["name"] == name));
    }
    let brief = service
        .call(
            "session_brief",
            json!({"task":"native","paths":["main.rs"]}),
        )
        .await;
    assert_eq!(brief["authority"], "non-authoritative-session-brief");
    let text = brief["text"].as_str().unwrap();
    assert!(
        text.contains("KB-NATIVE") && text.contains("Review native context"),
        "{text}"
    );
    assert!(text.chars().count() <= 2048);
    let mailbox = conductor_native::a2a_store::Store::required(
        &service.source.join(".agents/a2a"),
        "native-agent",
        false,
    )
    .unwrap();
    assert_eq!(mailbox.previews(true, 8, 140).unwrap().1, 1);
    assert_eq!(mailbox.presentation_previews(false, 8, 140).unwrap().1, 1);
    for args in [
        json!({"task":"native","host":"/etc"}),
        json!({"task":"native","identity":"another"}),
        json!({"task":"native","max_chars":2049}),
        json!({"task":"native","paths":["../escape"]}),
    ] {
        assert_eq!(
            service
                .request(json!({"method":"call","call":{"name":"session_brief","arguments":args}}))
                .await["ok"],
            false
        );
    }
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "starts an owned bounded transient service"]
async fn mapped_recall_reloads_atomic_rebuilds_and_rejects_stale_or_widened_requests() {
    let service = Running::configured(configure).await;
    mcp_workspace_calls(&service).await;
    mapped_recall_updates(&service).await;
}

async fn mcp_workspace_calls(service: &Running) {
    let mut mcp = Mcp::new(Path::new(support::forge_binary::path()), &service.socket());
    mcp.send(modern_request(1, "tools/list", json!({}))).await;
    assert!(mcp.receive().await["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|tool| tool["name"] == "workspace_recall"));
    for (id, name, arguments, authority) in [
        (
            2,
            "session_brief",
            json!({"task":"native"}),
            "non-authoritative-session-brief",
        ),
        (
            3,
            "workspace_recall",
            recall(),
            "non-authoritative-workspace-recall",
        ),
    ] {
        mcp.send(modern_request(
            id,
            "tools/call",
            json!({"name":name,"arguments":arguments}),
        ))
        .await;
        let response = mcp.receive().await;
        assert_eq!(response["result"]["isError"], false, "{response}");
        assert_eq!(
            response["result"]["structuredContent"]["result"]["authority"],
            authority
        );
    }
    mcp.finish().await;
}

async fn mapped_recall_updates(service: &Running) {
    let first = service.call("workspace_recall", recall()).await;
    assert_eq!(first["hits"][0]["text"], "original context");
    assert_eq!(first["authority"], "non-authoritative-workspace-recall");
    let mut calls = tokio::task::JoinSet::new();
    for _ in 0..24 {
        let socket = service.socket();
        let request =
            json!({"method":"call","call":{"name":"workspace_recall","arguments":recall()}});
        calls.spawn(async move {
            let response = transport::request(&socket, &serde_json::to_vec(&request).unwrap())
                .await
                .unwrap();
            assert_eq!(
                response["result"]["result"]["hits"][0]["text"],
                "original context"
            );
        });
    }
    while let Some(result) = calls.join_next().await {
        result.unwrap();
    }
    for (field, value) in [
        ("index", json!("other")),
        ("task", json!("wrong")),
        ("limit", json!(65)),
        ("source", json!("/etc/passwd")),
    ] {
        let mut request = recall();
        request[field] = value;
        assert_eq!(
            service
                .request(
                    json!({"method":"call","call":{"name":"workspace_recall","arguments":request}})
                )
                .await["ok"],
            false
        );
    }
    let source = service._host.0.join("memory/rows.jsonl");
    let snapshot = service._host.0.join("memory/mapped");
    let old = fs::read(&snapshot).unwrap();
    fs::write(&source, row("updated context")).unwrap();
    assert_eq!(
        service
            .request(
                json!({"method":"call","call":{"name":"workspace_recall","arguments":recall()}})
            )
            .await["ok"],
        false
    );
    assert_eq!(fs::read(&snapshot).unwrap(), old);
    memory_index_sidecar::build_sidecar(&source, &snapshot).unwrap();
    assert_eq!(
        service.call("workspace_recall", recall()).await["hits"][0]["text"],
        "updated context"
    );
    fs::set_permissions(&snapshot, fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(
        service
            .request(
                json!({"method":"call","call":{"name":"workspace_recall","arguments":recall()}})
            )
            .await["ok"],
        false
    );
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "starts an owned bounded transient service"]
async fn workspace_tools_are_absent_without_operator_capabilities() {
    let service = Running::new().await;
    let catalog = service.request(json!({"method":"catalog"})).await;
    for name in ["session_brief", "workspace_recall"] {
        assert!(!catalog["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["name"] == name));
        assert_eq!(
            service
                .request(json!({"method":"call","call":{"name":name,"arguments":recall()}}))
                .await["ok"],
            false
        );
    }
}
