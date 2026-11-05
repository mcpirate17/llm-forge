#![cfg(target_os = "linux")]
#[path = "support/embedding_fixture.rs"]
mod embedding_fixture;
#[path = "support/mcp_client.rs"]
mod mcp_client;
#[allow(dead_code)]
mod support;
#[path = "support/tokenizer_fixture.rs"]
mod tokenizer_fixture;
use base64::{engine::general_purpose::STANDARD, Engine};
use forge::harness::{policy::Policy, transport};
use forge::managed_service::Service;
use mcp_client::Mcp;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

fn context_capability(config: &forge::tokenizer::Config) -> Value {
    json!({"tokenizer":config,"max_input_bytes":8192,"max_tokens":2048,"recovery_bytes":1048576})
}

#[test]
fn context_policy_checks_bounds_artifacts_and_memory_reservations() {
    let tokenizer = tokenizer_fixture::Fixture::new();
    let host = support::Host::new();
    let (_, root, mut document) = fixture(&host);
    document["context"] = context_capability(&tokenizer.config);
    assert!(serde_json::from_value::<Policy>(document.clone())
        .unwrap()
        .validate(&root)
        .is_err());
    document["aggregate"]["memory_mib"] = 768.into();
    assert!(serde_json::from_value::<Policy>(document.clone())
        .unwrap()
        .validate(&root)
        .is_ok());
    for (field, value) in [
        ("max_input_bytes", u64::MAX),
        ("max_tokens", 0),
        ("recovery_bytes", u64::MAX),
    ] {
        let mut invalid = document.clone();
        invalid["context"][field] = value.into();
        assert!(serde_json::from_value::<Policy>(invalid)
            .unwrap()
            .validate(&root)
            .is_err());
    }
    document["context"]["tokenizer"]["path"] = json!(host.0.join("missing"));
    assert!(serde_json::from_value::<Policy>(document)
        .unwrap()
        .validate(&root)
        .is_err());
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "starts an owned bounded transient service"]
async fn native_context_tools_count_compact_and_recover_through_persistent_harness() {
    let tokenizer = tokenizer_fixture::Fixture::new();
    let service = Running::configured(|_, _, policy| {
        policy["context"] = context_capability(&tokenizer.config);
        policy["aggregate"]["memory_mib"] = 768.into();
    })
    .await;
    assert_eq!(
        service
            .call("context_count", json!({"text":"hello world!"}))
            .await["tokens"],
        3
    );
    let request = json!({"entries":[{"id":"policy","kind":"policy","content":"EXACT POLICY"},
        {"id":"old","kind":"text","content":"past work ".repeat(300)},
        {"id":"recent","kind":"text","content":"latest request"}],
        "max_tokens":250,"max_bytes":1024,"reserved_tokens":10,"keep_recent":1});
    let output = service.call("context_compact", request.clone()).await;
    assert_eq!(output["protected_overflow"], false);
    assert_eq!(output["omitted_ids"], json!(["old"]));
    assert!(output["text"].as_str().unwrap().starts_with("EXACT POLICY"));
    let counter = service
        .call("context_count", json!({"text":output["text"]}))
        .await;
    assert_eq!(counter["tokens"], output["output_tokens"]);
    let mut bytes = Vec::new();
    loop {
        let chunk = service
            .call(
                "context_recover",
                json!({"sha256":output["recovery_sha256"],"offset":bytes.len(),"limit":127}),
            )
            .await;
        bytes.extend(STANDARD.decode(chunk["base64"].as_str().unwrap()).unwrap());
        assert_eq!(chunk["next"].as_u64().unwrap() as usize, bytes.len());
        if chunk["eof"] == true {
            break;
        }
    }
    let original: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        original["request"]["entries"][1]["content"],
        request["entries"][1]["content"]
    );
    assert_eq!(
        format!("{:x}", Sha256::digest(&bytes)),
        output["recovery_sha256"]
    );
    for args in [
        json!({"text":"🦀".repeat(3000)}),
        json!({"text":"hello","tokenizer":"/attacker.json"}),
    ] {
        let denied = service
            .request(json!({"method":"call","call":{"name":"context_count","arguments":args}}))
            .await;
        assert_eq!(denied["ok"], false);
    }
    let mut tasks = Vec::new();
    for _ in 0..20 {
        let socket = service.root.join("socket");
        tasks.push(tokio::spawn(async move {
            let response=transport::request(&socket,&serde_json::to_vec(&json!({"method":"call","call":{"name":"context_count","arguments":{"text":"hello world!"}}})).unwrap()).await.unwrap();
        assert_eq!(response["result"]["result"]["tokens"],3);
    }));
    }
    for task in tasks {
        task.await.unwrap();
    }
}

struct Running {
    service: Service,
    root: PathBuf,
    source: PathBuf,
    _host: support::Host,
}

fn fixture(host: &support::Host) -> (PathBuf, PathBuf, Value) {
    let source = host.0.join("source");
    fs::create_dir(&source).unwrap();
    git2::Repository::init(&source).unwrap();
    fs::write(source.join("main.rs"), "fn answer() -> u32 { 42 }\n").unwrap();
    fs::write(source.join("secret.rs"), "fn secret() {}\n").unwrap();
    let root = host.0.join("private");
    fs::create_dir(&root).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    let document = json!({"version":1,"source_root":source,"write_paths":["main.rs"],
        "read_only":["/usr","/lib","/lib64","/etc/ld.so.cache"],
        "programs":{"worker":{"executable":support::task_worker(),"fixed_arguments":[],"environment":{},"working_directory":"scratch"}},
        "aggregate":{"cpu_millis":1000,"memory_mib":640,"processes":128},
        "session":{"cpu_millis":500,"memory_mib":64,"processes":32},
        "max_sessions":2,"max_retained_sessions":8,"max_tasks":16,"lifetime_seconds":45,"session_seconds":30,
        "retention_seconds":10,"replay_bytes":65536,"log_bytes":65536});
    (source, root, document)
}

impl Running {
    async fn new() -> Self {
        Self::configured(|_, _, _| {}).await
    }
    async fn configured(configure: impl FnOnce(&support::Host, &Path, &mut Value)) -> Self {
        let host = support::Host::new();
        let (source, root, mut document) = fixture(&host);
        configure(&host, &source, &mut document);
        let policy: Policy = serde_json::from_value(document.clone()).unwrap();
        policy.clone().validate(&root).unwrap();
        fs::write(
            root.join("policy.json"),
            serde_json::to_vec(&document).unwrap(),
        )
        .unwrap();
        let service = Service::launch(
            Path::new(support::forge_binary::path()),
            &[
                "harness".into(),
                "serve".into(),
                "--state".into(),
                root.as_os_str().into(),
            ],
            policy.aggregate,
            policy.lifetime_seconds,
            &BTreeMap::new(),
        )
        .unwrap();
        let result = Self {
            service,
            root,
            source,
            _host: host,
        };
        tokio::time::timeout(Duration::from_secs(8), async {
            while !result.root.join("ready.json").exists() {
                assert!(
                    !result.root.join("failure.json").exists(),
                    "{}",
                    fs::read_to_string(result.root.join("failure.json")).unwrap()
                );
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        result
    }
    fn socket(&self) -> PathBuf {
        self.root.join("socket")
    }
    async fn request(&self, value: Value) -> Value {
        tokio::time::timeout(
            Duration::from_secs(35),
            transport::request(&self.socket(), &serde_json::to_vec(&value).unwrap()),
        )
        .await
        .unwrap()
        .unwrap()
    }
    async fn call(&self, name: &str, args: Value) -> Value {
        let result = self
            .request(json!({"method":"call","call":{"name":name,"arguments":args}}))
            .await;
        assert_eq!(result["ok"], true, "{result}");
        result["result"]["result"].clone()
    }
    async fn read_until(&self, id: &str, cursor: &mut u64, needle: &str) -> String {
        let mut output = String::new();
        tokio::time::timeout(Duration::from_secs(4), async {
            loop {
                let response = self
                    .call(
                        "terminal_read",
                        json!({"id":id,"cursor":cursor,"wait_ms":1000}),
                    )
                    .await;
                *cursor = response["next"].as_u64().unwrap();
                output.push_str(&String::from_utf8_lossy(
                    &STANDARD
                        .decode(response["base64"].as_str().unwrap())
                        .unwrap(),
                ));
                if output.contains(needle) {
                    break;
                }
            }
        })
        .await
        .unwrap();
        output
    }
}

fn index_sources(
    index: &Path,
    source: &Path,
    space: conductor_native::vector_index::Space,
    paths: &[&str],
) -> String {
    use forge::code_index::{build, BuildHeader, EmbeddingRow, Store};
    let rows = paths.iter().enumerate().map(|(i, path)| {
        let text = fs::read_to_string(source.join(path)).unwrap();
        Ok(EmbeddingRow {
            path: (*path).into(),
            source_sha256: conductor_native::syntax::fingerprint(&text),
            start_byte: 0,
            end_byte: text.len(),
            vector: if i == 0 {
                vec![1.0, 0.0]
            } else {
                vec![0.6, 0.8]
            },
        })
    });
    build(
        &Store::create(index).unwrap(),
        source,
        BuildHeader {
            version: 1,
            rows: paths.len(),
            space,
            config: Default::default(),
        },
        rows,
    )
    .unwrap()
    .generation
}

#[test]
fn search_policy_bounds_reserve_memory_and_reject_state_exposure_without_overflow() {
    let host = support::Host::new();
    let (_, root, mut document) = fixture(&host);
    let index = host.0.join("index");
    forge::code_index::Store::create(&index).unwrap();
    document["code_indexes"] =
        json!({"repo":{"directory":index,"max_rows":16,"max_mapped_mib":1,"readers":2}});
    serde_json::from_value::<Policy>(document.clone())
        .unwrap()
        .validate(&root)
        .unwrap();
    for (key, value) in [
        ("readers", json!(u64::MAX)),
        ("max_rows", json!(u64::MAX)),
        ("max_mapped_mib", json!(u64::MAX)),
        ("max_mapped_mib", json!(8192)),
        ("directory", json!(root)),
    ] {
        let mut bad = document.clone();
        bad["code_indexes"]["repo"][key] = value;
        assert!(
            serde_json::from_value::<Policy>(bad)
                .unwrap()
                .validate(&root)
                .is_err(),
            "{key}"
        );
    }
    let mut exposed = document;
    exposed["read_only"] = json!([host.0]);
    assert!(serde_json::from_value::<Policy>(exposed)
        .unwrap()
        .validate(&root)
        .is_err());
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "launches bounded native harness and exercises cached search across refresh"]
async fn code_search_reuses_readers_and_observes_atomic_generations_and_caps() {
    let space = conductor_native::vector_index::Space {
        dimension: 2,
        model_sha256: [4; 32],
    };
    let service = Running::configured(|host, source, document| {
        let directory = host.0.join("index");
        index_sources(&directory, source, space.clone(), &["main.rs", "secret.rs"]);
        document["code_indexes"] =
            json!({"repo":{"directory":directory,"max_rows":2,"max_mapped_mib":1,"readers":2}});
    })
    .await;
    let catalog = service.request(json!({"method":"catalog"})).await;
    assert!(catalog["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|t| t["name"] == "code_search"));
    let first = service
        .call("code_search", json!({"index":"repo","text":"answer"}))
        .await;
    assert_eq!(first["hits"][0]["path"], "main.rs");
    assert_eq!(first["embedding_performed"], false);
    let mut calls = tokio::task::JoinSet::new();
    for _ in 0..24 {
        let socket = service.socket();
        calls.spawn(async move {
            transport::request(&socket,br#"{"method":"call","call":{"name":"code_search","arguments":{"index":"repo","text":"answer"}}}"#).await.unwrap()
        });
    }
    while let Some(call) = calls.join_next().await {
        assert_eq!(call.unwrap()["ok"], true);
    }
    let directory = service._host.0.join("index");
    fs::write(service.source.join("main.rs"), "fn refreshed() {}\n").unwrap();
    let current = index_sources(
        &directory,
        &service.source,
        space.clone(),
        &["main.rs", "secret.rs"],
    );
    let fresh = service
        .call("code_search", json!({"index":"repo","text":"refreshed"}))
        .await;
    assert_eq!(fresh["generation"], current);
    assert_ne!(fresh["generation"], first["generation"]);
    assert_eq!(fresh["hits"][0]["text"], "fn refreshed() {}\n");
    fs::write(service.source.join("extra.rs"), "fn extra() {}\n").unwrap();
    index_sources(
        &directory,
        &service.source,
        space,
        &["extra.rs", "main.rs", "secret.rs"],
    );
    let denied=service.request(json!({"method":"call","call":{"name":"code_search","arguments":{"index":"repo","text":"refreshed"}}})).await;
    assert_eq!(denied["ok"], false);
    assert!(denied["error"]
        .as_str()
        .unwrap()
        .contains("operator reader limits"));
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "launches bounded native harness with local HTTP embedding fixture"]
async fn code_search_validates_before_embedding_and_uses_only_pinned_provider() {
    use embedding_fixture::{config, inventory, output, reply, server};
    let (endpoint, worker) = server(vec![
        reply("/api/tags", inventory(7)),
        reply("/api/embed", output()),
        reply("/api/tags", inventory(7)),
    ]);
    let config = config(endpoint);
    let space = config.space().unwrap();
    let service=Running::configured(|host,source,document| {
        let directory=host.0.join("index"); index_sources(&directory,source,space,&["main.rs","secret.rs"]);
        document["code_indexes"]=json!({"repo":{"directory":directory,"max_rows":2,"max_mapped_mib":1,"readers":1,"embedding":config}});
    }).await;
    for arguments in [
        json!({"index":"repo","text":"answer","limit":10,"candidates":1}),
        json!({"index":"repo","text":"\u{0}"}),
        json!({"index":"repo","text":"🦀".repeat(2000)}),
        json!({"index":"repo","text":"answer","endpoint":"http://127.0.0.1:1"}),
    ] {
        assert_eq!(
            service
                .request(
                    json!({"method":"call","call":{"name":"code_search","arguments":arguments}})
                )
                .await["ok"],
            false
        );
    }
    let result = service
        .call(
            "code_search",
            json!({"index":"repo","text":"answer","kind":"semantic"}),
        )
        .await;
    assert_eq!(result["hits"][0]["path"], "secret.rs");
    assert_eq!(result["embedding_input_tokens"], 8);
    assert_eq!(result["embedding_performed"], true);
    assert_eq!(worker.join().unwrap().len(), 3);
    let lexical = service
        .call(
            "code_search",
            json!({"index":"repo","text":"answer","kind":"lexical"}),
        )
        .await;
    assert_eq!(lexical["hits"][0]["path"], "main.rs");
    assert_eq!(lexical["embedding_performed"], false);
}

#[tokio::test(flavor = "current_thread")]
async fn frame_reader_bounds_split_utf8_empty_truncated_and_oversized_lines() {
    let mut buffer = Vec::new();
    let mut reader = BufReader::with_capacity(1, "λ\n{}\n".as_bytes());
    assert!(transport::read_frame(&mut reader, &mut buffer, 2)
        .await
        .unwrap());
    assert_eq!(buffer, "λ".as_bytes());
    assert!(transport::read_frame(&mut reader, &mut buffer, 2)
        .await
        .unwrap());
    assert_eq!(buffer, b"{}");
    assert!(!transport::read_frame(&mut reader, &mut buffer, 2)
        .await
        .unwrap());
    for bytes in [&b"{}"[..], &b"12345\n"[..]] {
        assert!(
            transport::read_frame(&mut BufReader::with_capacity(2, bytes), &mut buffer, 4)
                .await
                .is_err()
        );
        assert!(buffer.len() <= 4);
    }
}

#[test]
fn policy_rejects_state_exposure_extra_fields_and_underreserved_resources() {
    let host = support::Host::new();
    let (_, root, document) = fixture(&host);
    for (key, value) in [
        ("read_only", json!(["/"])),
        ("write_paths", json!(["../secret"])),
        ("max_sessions", json!(0)),
        ("session_seconds", json!(46)),
        ("extra", json!(true)),
        (
            "programs",
            json!({"worker":{"executable":support::task_worker(),"fixed_arguments":[],
            "environment":{"HOME":"/outside"},"working_directory":"scratch"}}),
        ),
        (
            "aggregate",
            json!({"cpu_millis":1000,"memory_mib":128,"processes":128}),
        ),
    ] {
        let mut invalid = document.clone();
        invalid[key] = value;
        assert!(
            serde_json::from_value::<Policy>(invalid)
                .and_then(|p| p.validate(&root).map_err(serde::de::Error::custom))
                .is_err(),
            "{key}"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "launches a bounded native harness under the systemd user manager"]
async fn validated_source_dispatch_and_patch_capabilities_are_end_to_end() {
    let service = Running::new().await;
    let socket_meta = fs::metadata(service.socket()).unwrap();
    assert_eq!(socket_meta.mode() & 0o777, 0o600);
    let catalog = service.request(json!({"method":"catalog"})).await;
    forge::tool_schema::ToolRegistry::from_json(
        &serde_json::to_string(&catalog["result"]).unwrap(),
    )
    .unwrap();
    let inspection = service
        .call("code_inspect", json!({"path":"main.rs"}))
        .await;
    assert_eq!(inspection["symbols"][0]["name"], "answer");
    let fenced = "```json\n{\"name\":\"code_fold\",\"arguments\":{\"path\":\"main.rs\"}}\n```";
    let repaired = service
        .request(json!({"method":"call_text","input":fenced,"repair":true}))
        .await;
    assert_eq!(repaired["ok"], true);
    assert_eq!(repaired["result"]["repaired"], true);
    assert_eq!(
        service
            .request(json!({"method":"call_text","input":fenced}))
            .await["ok"],
        false
    );
    let hash = inspection["source_sha256"].clone();
    let edit = json!({"path":"main.rs","expected_sha256":hash,"search":"42","replacement":"43"});
    let preview = service.call("code_replace", edit.clone()).await;
    assert_eq!(preview["applied"], false);
    assert!(fs::read_to_string(service.source.join("main.rs"))
        .unwrap()
        .contains("42"));
    let mut apply = edit.clone();
    apply["apply"] = json!(true);
    let changed = service.call("code_replace", apply).await;
    assert_eq!(changed["applied"], true);
    let denied = service
        .request(json!({"method":"call","call":{"name":"code_replace","arguments":edit}}))
        .await;
    assert_eq!(denied["ok"], false);
    let restored=service.call("code_restore",json!({"path":"main.rs","expected_sha256":changed["candidate_sha256"],"git_snapshot":changed["git_snapshot"],"apply":true})).await;
    assert_eq!(restored["candidate_sha256"], hash);
    for raw in [
        r#"{"method":"call","call":{"name":"terminal_spawn","arguments":{"id":"evil","program":"worker","arguments":[],"arguments":["sleep","99999"]}}}"#,
        r#"{"method":"call","call":{"name":"terminal_spawn","arguments":{"id":"evil","program":"/bin/sh","arguments":[]}}}"#,
        r#"{"method":"call","call":{"name":"code_inspect","arguments":{"path":"../private/policy.json"}}}"#,
        r#"{"method":"call","call":{"name":"code_replace","arguments":{"path":"secret.rs","expected_sha256":"0000000000000000000000000000000000000000000000000000000000000000","search":"secret","replacement":"owned","apply":true}}}"#,
    ] {
        assert_eq!(
            transport::request(&service.socket(), raw.as_bytes())
                .await
                .unwrap()["ok"],
            false
        );
    }
    assert_eq!(service.call("task_list", json!({})).await, json!([]));
    let mut clients = tokio::task::JoinSet::new();
    for _ in 0..32 {
        let socket = service.socket();
        clients.spawn(async move {
            transport::request(&socket, br#"{"method":"catalog"}"#)
                .await
                .unwrap()
        });
    }
    while let Some(result) = clients.join_next().await {
        assert_eq!(result.unwrap()["ok"], true);
    }
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "launches a bounded native harness with confined PTYs"]
async fn disconnected_spawn_and_write_retries_do_not_duplicate_execution() {
    let service = Running::new().await;
    let spawn = json!({"id":"interactive","program":"worker","arguments":["terminal"]});
    let mut lost = UnixStream::connect(service.socket()).await.unwrap();
    lost.write_all(
        format!(
            "{}\n",
            json!({"method":"call","call":{"name":"terminal_spawn","arguments":spawn}})
        )
        .as_bytes(),
    )
    .await
    .unwrap();
    drop(lost);
    let started = service.call("terminal_spawn", spawn.clone()).await;
    let retried = service.call("terminal_spawn", spawn).await;
    assert_eq!(started["status"]["pid"], retried["status"]["pid"]);
    assert_eq!(retried["reused"], true);
    let mut cursor = 0;
    service
        .read_until("interactive", &mut cursor, "READY")
        .await;
    let write = json!({"id":"interactive","sequence":1,"base64":STANDARD.encode("λ-native\n")});
    let first = service.call("terminal_write", write.clone()).await;
    let retry = service.call("terminal_write", write).await;
    assert_eq!(first, retry);
    let output = service
        .read_until("interactive", &mut cursor, "ECHO λ-native")
        .await;
    assert_eq!(output.matches("ECHO λ-native").count(), 1);
    service
        .call(
            "terminal_resize",
            json!({"id":"interactive","rows":33,"columns":99}),
        )
        .await;
    service
        .call(
            "terminal_write",
            json!({"id":"interactive","sequence":2,"base64":STANDARD.encode("size\n")}),
        )
        .await;
    service
        .read_until("interactive", &mut cursor, "SIZE 33 99")
        .await;
    let empty = service
        .call(
            "terminal_read",
            json!({"id":"interactive","cursor":cursor,"wait_ms":20}),
        )
        .await;
    assert_eq!(empty["base64"], "");
    service
        .call(
            "terminal_write",
            json!({"id":"interactive","sequence":3,"base64":STANDARD.encode("exit\n")}),
        )
        .await;
    let task = tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            let task = service.call("task_get", json!({"id":"interactive"})).await;
            if task["state"] == "succeeded" {
                break task;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(task["attempt"], 1);
    let receipt = fs::read(task["receipt"]["path"].as_str().unwrap()).unwrap();
    assert_eq!(
        task["receipt"]["sha256"],
        format!("{:x}", Sha256::digest(&receipt))
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&receipt).unwrap()["resources"]["verified"],
        true
    );
    assert_eq!(
        service
            .call("task_list", json!({}))
            .await
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "launches a bounded native harness and tests shutdown during a long poll"]
async fn shutdown_releases_long_polls_and_persists_child_cleanup() {
    let mut service = Running::new().await;
    service
        .call(
            "terminal_spawn",
            json!({"id":"waiter","program":"worker","arguments":["terminal"]}),
        )
        .await;
    let mut cursor = 0;
    service.read_until("waiter", &mut cursor, "READY").await;
    let socket = service.socket();
    let waiter = tokio::spawn(async move {
        transport::request(&socket,&serde_json::to_vec(&json!({"method":"call","call":{"name":"terminal_read","arguments":{"id":"waiter","cursor":cursor,"wait_ms":30000}}})).unwrap()).await
    });
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(
        service.request(json!({"method":"shutdown"})).await["ok"],
        true
    );
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(3), waiter)
            .await
            .unwrap()
            .unwrap()
            .unwrap()["ok"],
        true
    );
    service.service.stop().unwrap();
    assert!(!service.root.join("failure.json").exists());
    let store = forge::task_store::Store::open_path(&service.root.join("tasks.sqlite3"), false)
        .unwrap()
        .unwrap();
    let task = store.get("waiter").unwrap();
    assert_eq!(task.state, forge::task_store::State::Failed);
    assert!(task.receipt.is_some());
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "starts and explicitly stops a bounded native service through the public CLI"]
async fn public_start_reports_verified_ready_socket_and_grammar() {
    let host = support::Host::new();
    let (_, root, mut policy) = fixture(&host);
    policy["lifetime_seconds"] = json!(10);
    policy["session_seconds"] = json!(5);
    let policy_file = host.0.join("operator.json");
    fs::write(&policy_file, serde_json::to_vec(&policy).unwrap()).unwrap();
    let output = forge::process_probe::run(
        std::process::Command::new(support::forge_binary::path())
            .args(["harness", "start", "--policy"])
            .arg(&policy_file)
            .arg("--state-parent")
            .arg(&root),
        Duration::from_secs(12),
        65536,
    )
    .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    let socket = PathBuf::from(result["socket"].as_str().unwrap());
    let response = transport::request(&socket, br#"{"method":"grammar"}"#)
        .await
        .unwrap();
    assert_eq!(response["ok"], true);
    assert!(response["result"]["grammar"]
        .as_str()
        .unwrap()
        .contains("tool-call"));
    assert_eq!(
        transport::request(&socket, br#"{"method":"shutdown"}"#)
            .await
            .unwrap()["ok"],
        true
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        while socket.exists() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert!(!socket.parent().unwrap().join("failure.json").exists());
}

fn modern_request(id: u64, method: &str, mut params: Value) -> Value {
    params["_meta"] = json!({"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}});
    json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "starts one bounded transient service and binds its native MCP adapter"]
async fn project_mcp_configuration_runs_against_the_bound_live_service() {
    let service = Running::new().await;
    fs::set_permissions(&service.source, fs::Permissions::from_mode(0o700)).unwrap();
    let output = std::process::Command::new(support::forge_binary::path())
        .env("PATH", "")
        .args(["mcp", "--host"])
        .arg(&service.source)
        .args(["bind", "--socket"])
        .arg(service.socket())
        .arg("--apply")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let plan: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        plan["service"]["status"]["source_root"],
        json!(service.source)
    );
    assert_eq!(plan["services_started"], false);
    let installed: Value =
        serde_json::from_slice(&fs::read(service.source.join(".mcp.json")).unwrap()).unwrap();
    let entry = &installed["mcpServers"]["forge"];
    let mut command = std::process::Command::new(entry["command"].as_str().unwrap());
    command.args(
        entry["args"]
            .as_array()
            .unwrap()
            .iter()
            .map(|arg| arg.as_str().unwrap()),
    );
    let mut client = Mcp::from_command(command);
    client
        .send(modern_request(
            1,
            "tools/call",
            json!({"name":"code_inspect","arguments":{"path":"main.rs"}}),
        ))
        .await;
    let reply = client.receive().await;
    assert_eq!(reply["result"]["isError"], false);
    assert_eq!(
        reply["result"]["structuredContent"]["result"]["symbols"][0]["name"],
        "answer"
    );
    client.finish().await;
    assert_eq!(
        service.request(json!({"method":"status"})).await["ok"],
        true
    );
    let output = std::process::Command::new(support::forge_binary::path())
        .env("PATH", "")
        .args(["mcp", "--host"])
        .arg(&service.source)
        .args(["rollback", "--apply"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(!service.source.join(".mcp.json").exists());
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "exercises native MCP stdio against a bounded live harness"]
async fn mcp_legacy_stdio_runs_without_an_interpreter_and_keeps_sessions_external() {
    let service = Running::new().await;
    let mut client = Mcp::new(Path::new(support::forge_binary::path()), &service.socket());
    client.send(json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25",
        "capabilities":{},"clientInfo":{"name":"native-test","version":"1"}}})).await;
    assert_eq!(
        client.receive().await["result"]["protocolVersion"],
        "2025-11-25"
    );
    client
        .send(json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
        .await;
    client
        .send(json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}))
        .await;
    assert!(
        client.receive().await["result"]["tools"]
            .as_array()
            .unwrap()
            .len()
            > 10
    );
    for id in 3..11 {
        client
            .send(
                json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{
        "name":"code_inspect","arguments":{"path":"main.rs"}}}),
            )
            .await;
    }
    let mut ids = std::collections::BTreeSet::new();
    for _ in 3..11 {
        let response = client.receive().await;
        ids.insert(response["id"].as_u64().unwrap());
        assert_eq!(response["result"]["isError"], false);
        assert_eq!(
            response["result"]["structuredContent"]["result"]["symbols"][0]["name"],
            "answer"
        );
    }
    assert_eq!(ids, (3..11).collect());
    client.finish().await;
    assert_eq!(
        service.request(json!({"method":"status"})).await["ok"],
        true
    );
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "exercises modern MCP metadata, cancellation and concurrent PTY reads"]
async fn mcp_modern_stdio_remains_responsive_during_a_cancelled_long_poll() {
    let service = Running::new().await;
    service
        .call(
            "terminal_spawn",
            json!({"id":"mcp-terminal","program":"worker","arguments":["terminal"]}),
        )
        .await;
    let mut cursor = 0;
    service
        .read_until("mcp-terminal", &mut cursor, "READY")
        .await;
    let mut client = Mcp::new(Path::new(support::forge_binary::path()), &service.socket());
    client
        .send(modern_request(1, "server/discover", json!({})))
        .await;
    let discovery = client.receive().await;
    assert_eq!(discovery["result"]["resultType"], "complete");
    assert_eq!(discovery["result"]["cacheScope"], "private");
    client.send(modern_request(10,"tools/call",json!({"name":"terminal_read","arguments":{"id":"mcp-terminal","cursor":cursor,"wait_ms":30000}}))).await;
    client.send(modern_request(11, "ping", json!({}))).await;
    assert_eq!(client.receive().await["id"], 11);
    client.send(modern_request(10, "ping", json!({}))).await;
    assert_eq!(client.receive().await["error"]["code"], -32600);
    client
        .send(json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":10}}))
        .await;
    client.send(modern_request(12, "ping", json!({}))).await;
    assert_eq!(client.receive().await["id"], 12);
    service
        .call("terminal_close", json!({"id":"mcp-terminal"}))
        .await;
    client.send(modern_request(13, "ping", json!({}))).await;
    assert_eq!(client.receive().await["id"], 13);
    client.assert_no_frame(Duration::from_millis(100)).await;
    client.finish().await;
}

#[path = "support/workspace_cases.rs"]
mod workspace_cases;
