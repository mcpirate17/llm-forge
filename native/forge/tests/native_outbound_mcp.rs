#![cfg(target_os = "linux")]
//! Opt-in raw MCP interoperability and containment evidence, not client adoption.
#[path = "support/forge_binary.rs"]
mod forge_binary;
#[path = "support/mcp_client.rs"]
mod mcp_client;
#[path = "support/outbound_observation.rs"]
mod outbound_observation;
#[path = "support/process_usage.rs"]
mod process_usage;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

fn sha(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn assert_missing(path: &Path) {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        other => panic!(
            "owned path is not proven absent: {}: {other:?}",
            path.display()
        ),
    }
}
fn schemas() -> (Value, Value) {
    (
        json!({"type":"object","properties":{"path":{"type":"string","minLength":1,
        "maxLength":4096}},"required":["path"],"additionalProperties":false}),
        json!({"type":"object","properties":{
        "pid":{"type":"integer","minimum":1,"maximum":4294967295_u64},
        "call_count":{"type":"integer","minimum":1,"maximum":32},
        "bytes":{"type":"integer","minimum":0,"maximum":4096},
        "sha256":{"type":"string","minLength":64,"maxLength":64},
        "text":{"type":"string","maxLength":4096},"secret_present":{"type":"boolean"}},
        "required":["pid","call_count","bytes","sha256","text","secret_present"],
        "additionalProperties":false}),
    )
}
fn copy_server(root: &Path) -> PathBuf {
    let supplied = PathBuf::from(
        std::env::var_os("FORGE_TEST_MCP_SERVER")
            .expect("set FORGE_TEST_MCP_SERVER to the compiled outbound_mcp_server example"),
    );
    let pin = std::env::var("FORGE_TEST_MCP_SERVER_SHA256")
        .expect("set FORGE_TEST_MCP_SERVER_SHA256 independently");
    assert!(supplied.is_absolute());
    let meta = fs::symlink_metadata(&supplied).unwrap();
    assert!(
        meta.is_file() && !meta.file_type().is_symlink() && meta.permissions().mode() & 0o111 != 0
    );
    assert_eq!(pin.len(), 64);
    assert!(pin
        .bytes()
        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)));
    let bytes = fs::read(&supplied).unwrap();
    assert_eq!(sha(&bytes), pin, "fixture binary pin differs");
    let bin = root.join("bin");
    fs::create_dir(&bin).unwrap();
    fs::set_permissions(&bin, fs::Permissions::from_mode(0o700)).unwrap();
    let copied = bin.join("server");
    fs::write(&copied, &bytes).unwrap();
    fs::set_permissions(&copied, fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(sha(&fs::read(&copied).unwrap()), pin);
    copied
}
struct Running {
    service: forge::managed_service::Service,
    state: PathBuf,
    allowed: PathBuf,
    denied: PathBuf,
    binary: PathBuf,
    binary_sha: String,
    root: Option<tempfile::TempDir>,
    verified: bool,
}
impl Drop for Running {
    fn drop(&mut self) {
        if !self.verified || std::thread::panicking() {
            if let Some(root) = self.root.take() {
                eprintln!(
                    "unverified outbound fixture retained: {}",
                    root.keep().display()
                );
            }
        }
    }
}
fn policy(root: &Path, binary: &Path, mode: Option<&str>) -> Value {
    let source = root.join("source");
    let state = root.join("state");
    fs::create_dir(&source).unwrap();
    fs::set_permissions(&source, fs::Permissions::from_mode(0o700)).unwrap();
    git2::Repository::init(&source).unwrap();
    fs::set_permissions(source.join(".git"), fs::Permissions::from_mode(0o700)).unwrap();
    fs::create_dir(&state).unwrap();
    fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
    let mut reads = vec![
        root.join("allowed.txt"),
        binary.to_path_buf(),
        Path::new("/usr").canonicalize().unwrap(),
    ];
    for path in ["/etc/ld.so.cache", "/lib", "/lib64"] {
        if Path::new(path).exists() {
            reads.push(Path::new(path).canonicalize().unwrap());
        }
    }
    if mode == Some("gated") {
        reads.push(root.join("operator-gate"));
    }
    reads.sort();
    reads.dedup();
    let all = reads.clone();
    reads.retain(|path| {
        !all.iter()
            .any(|parent| parent != path && path.starts_with(parent))
    });
    let mut document = json!({"version":1,"source_root":source,"write_paths":[],"read_only":reads,
        "programs":{},"aggregate":{"cpu_millis":1000,"memory_mib":1024,"processes":128},
        "session":{"cpu_millis":500,"memory_mib":128,"processes":32},
        "max_sessions":1,"max_retained_sessions":2,"max_tasks":4,"lifetime_seconds":40,
        "session_seconds":20,"retention_seconds":5,"replay_bytes":65536,"log_bytes":65536});
    if let Some(mode) = mode {
        let (input, output) = schemas();
        let actual_mode = if mode == "measurement" {
            "normal"
        } else {
            mode
        };
        let fixed = if mode == "gated" {
            json!([mode, root.join("operator-gate/release")])
        } else {
            json!([actual_mode])
        };
        document["outbound_mcp"] = json!({"public_name":"outbound_read_file","remote_name":"read_file",
            "executable":{"path":binary,"sha256":sha(&fs::read(binary).unwrap())},
            "fixed_arguments":fixed,"pinned_inputs":[{"path":root.join("allowed.txt"),"sha256":sha(&fs::read(root.join("allowed.txt")).unwrap())}],
            "read_only":reads,"input_schema_json":serde_json::to_string(&input).unwrap(),
            "output_schema_json":serde_json::to_string(&output).unwrap(),
            "resources":{"cpu_millis":500,"memory_mib":128,"processes":16},"max_wall_seconds":if mode=="measurement" {30} else if mode=="gated" {10} else {2}});
    }
    document
}
impl Running {
    async fn start(mode: Option<&str>) -> Self {
        let root = tempfile::tempdir().unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(root.path().join("allowed.txt"), "actual fixture λ bytes\n").unwrap();
        fs::set_permissions(
            root.path().join("allowed.txt"),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        fs::write(root.path().join("denied.txt"), "outside explicit grant\n").unwrap();
        if mode == Some("gated") {
            fs::create_dir(root.path().join("operator-gate")).unwrap();
            fs::set_permissions(
                root.path().join("operator-gate"),
                fs::Permissions::from_mode(0o700),
            )
            .unwrap();
            fs::write(root.path().join("operator-gate/release"), b"blocked\n").unwrap();
        }
        let binary = copy_server(root.path());
        let document = policy(root.path(), &binary, mode);
        let state = root.path().join("state");
        let policy: forge::harness::policy::Policy =
            serde_json::from_value(document.clone()).unwrap();
        let policy = policy.validate(&state).unwrap();
        fs::write(
            state.join("policy.json"),
            serde_json::to_vec(&document).unwrap(),
        )
        .unwrap();
        let env = BTreeMap::from([(
            "FORGE_OUTBOUND_SYNTHETIC_SECRET".into(),
            "synthetic-never-forward".into(),
        )]);
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
            &env,
        )
        .unwrap();
        let running = Self {
            service,
            state,
            allowed: root.path().join("allowed.txt"),
            denied: root.path().join("denied.txt"),
            binary_sha: sha(&fs::read(&binary).unwrap()),
            binary,
            root: Some(root),
            verified: false,
        };
        tokio::time::timeout(Duration::from_secs(8), async {
            while !running.state.join("ready.json").exists() {
                assert!(
                    !running.state.join("failure.json").exists(),
                    "service launch failed"
                );
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        running
    }
    async fn status(&self) -> Value {
        let reply = forge::harness::transport::request(
            &self.state.join("socket"),
            br#"{"method":"status"}"#,
        )
        .await
        .unwrap();
        assert_eq!(reply["ok"], true, "{reply}");
        reply["result"].clone()
    }
    async fn finish(mut self, launched: bool) -> Value {
        let status = self.status().await;
        let outbound = &status["outbound_mcp"];
        if launched {
            assert_eq!(outbound["launches"], 1, "{status}");
            assert_eq!(
                outbound["scratch_path"],
                self.state.join("outbound-scratch").to_str().unwrap()
            );
            let pid = status["pid"].as_u64().unwrap();
            let cgroups = fs::read_to_string(format!("/proc/{pid}/cgroup")).unwrap();
            let current = cgroups
                .lines()
                .find_map(|line| line.strip_prefix("0::"))
                .expect("unified service cgroup");
            let supervisor = Path::new("/sys/fs/cgroup").join(current.trim_start_matches('/'));
            assert_eq!(supervisor.file_name().unwrap(), "supervisor");
            let parent = supervisor.parent().unwrap();
            assert_eq!(
                parent.file_name().unwrap(),
                self.service.record().unit.as_str()
            );
            let owned = Path::new(outbound["cgroup_path"].as_str().unwrap());
            assert_eq!(owned.parent(), Some(parent), "{status}");
        } else if !outbound.is_null() {
            assert_eq!(outbound["state"], "not_started");
            assert_eq!(outbound["launches"], 0);
        }
        let response = forge::harness::transport::request(
            &self.state.join("socket"),
            br#"{"method":"shutdown"}"#,
        )
        .await
        .unwrap();
        assert_eq!(response["ok"], true, "{response}");
        self.service.stop().unwrap();
        let path = self.state.join("outbound-disposition.json");
        if launched {
            let value: Value = serde_json::from_slice(
                &fs::read(&path).expect("durable outbound cleanup disposition"),
            )
            .unwrap();
            assert_eq!(value["schema_version"], 1);
            assert_eq!(value["cleanup_safe"], true, "{value}");
            for field in ["cgroup_path", "scratch_path"] {
                assert_eq!(value[field], outbound[field]);
            }
            for field in ["cgroup_path", "scratch_path"] {
                let path = Path::new(value[field].as_str().expect("owned diagnostic path"));
                assert!(path.is_absolute());
                assert_missing(path);
            }
        } else {
            assert_missing(&path);
        }
        assert_eq!(sha(&fs::read(&self.binary).unwrap()), self.binary_sha);
        self.verified = true;
        if launched {
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap()
        } else {
            Value::Null
        }
    }
}
async fn connect(state: &Path) -> mcp_client::Mcp {
    let mut client = mcp_client::Mcp::new(Path::new(forge_binary::path()), &state.join("socket"));
    client.send(json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
        "protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"outbound-native-test","version":"1"}}})).await;
    let reply = client.receive().await;
    assert_eq!(reply["result"]["protocolVersion"], "2025-11-25");
    client
        .send(json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
        .await;
    client
}
async fn call(client: &mut mcp_client::Mcp, id: u64, args: Value) -> Value {
    client
        .send(
            json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{
        "name":"outbound_read_file","arguments":args}}),
        )
        .await;
    let value = client.receive().await;
    assert_eq!(value["id"], id);
    value
}
fn upstream(reply: &Value) -> &Value {
    assert_eq!(reply["result"]["isError"], false, "{reply}");
    &reply["result"]["structuredContent"]["result"]
}
fn refused(reply: &Value) -> bool {
    reply.get("error").is_some()
        || reply["result"]["isError"] == true
        || reply["result"]["structuredContent"]["result"]["isError"] == true
}
#[tokio::test(flavor = "current_thread")]
#[ignore = "requires owned service manager/cgroup and pinned FORGE_TEST_MCP_SERVER fixture"]
async fn authorized_stdio_discovery_reuses_endpoint_and_denies_ungranted_reads() {
    let service = Running::start(Some("normal")).await;
    let mut client = connect(&service.state).await;
    client
        .send(json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}))
        .await;
    let listed = client.receive().await;
    let names: Vec<_> = listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"outbound_read_file"));
    for forbidden in ["read_file", "unselected_write", "terminal_spawn"] {
        assert!(!names.contains(&forbidden));
    }
    let mut pid = Value::Null;
    for id in 3..=4 {
        let reply = call(&mut client, id, json!({"path":service.allowed})).await;
        let result = upstream(&reply);
        assert_eq!(result["isError"], false, "{reply}");
        let data = &result["structuredContent"];
        assert_eq!(data["text"], "actual fixture λ bytes\n");
        assert_eq!(data["bytes"], fs::read(&service.allowed).unwrap().len());
        assert_eq!(data["sha256"], sha(&fs::read(&service.allowed).unwrap()));
        assert_eq!(data["secret_present"], false);
        assert_eq!(data["call_count"], id - 2);
        if id == 3 {
            pid = data["pid"].clone();
        } else {
            assert_eq!(data["pid"], pid);
        }
    }
    let denied = call(&mut client, 5, json!({"path":service.denied})).await;
    assert_eq!(upstream(&denied)["isError"], true, "{denied}");
    client.finish().await;
    service.finish(true).await;
}
#[tokio::test(flavor = "current_thread")]
#[ignore = "requires owned service manager/cgroup and pinned FORGE_TEST_MCP_SERVER fixture"]
async fn missing_capability_and_invalid_input_report_unstarted_endpoint() {
    for mode in [None, Some("normal")] {
        let service = Running::start(mode).await;
        let mut client = connect(&service.state).await;
        client
            .send(json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}))
            .await;
        let listed = client.receive().await;
        let present = listed["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["name"] == "outbound_read_file");
        assert_eq!(present, mode.is_some());
        assert!(refused(
            &call(
                &mut client,
                3,
                json!({"path":service.allowed,"unknown":true})
            )
            .await
        ));
        // No-child evidence here is explicit service bookkeeping, not an
        // independent process-history attestation or a host-wide PID scan.
        let status = service.status().await;
        if mode.is_none() {
            assert!(status.get("outbound_mcp").is_none());
        } else {
            assert_eq!(status["outbound_mcp"]["state"], "not_started");
            assert_eq!(status["outbound_mcp"]["launches"], 0);
        }
        client.finish().await;
        service.finish(false).await;
    }
}
#[tokio::test(flavor = "current_thread")]
#[ignore = "requires owned service manager/cgroup and pinned FORGE_TEST_MCP_SERVER fixture"]
async fn schema_and_protocol_failures_close_endpoint_without_retry() {
    for mode in [
        "wrong_input_schema",
        "wrong_output_schema",
        "bad_output",
        "bad_version",
        "wrong_id",
        "unsolicited",
        "partial",
        "oversized",
    ] {
        let service = Running::start(Some(mode)).await;
        let mut client = connect(&service.state).await;
        assert!(
            refused(&call(&mut client, 2, json!({"path":service.allowed})).await),
            "{mode}"
        );
        assert!(
            refused(&call(&mut client, 3, json!({"path":service.allowed})).await),
            "retry after {mode}"
        );
        client.finish().await;
        service.finish(true).await;
    }
}
#[tokio::test(flavor = "current_thread")]
#[ignore = "requires owned service manager/cgroup and pinned FORGE_TEST_MCP_SERVER fixture"]
async fn idle_events_and_timeout_make_endpoint_unavailable() {
    for mode in ["idle_unsolicited", "idle_eof", "hang", "stderr_flood"] {
        let service = Running::start(Some(mode)).await;
        let mut client = connect(&service.state).await;
        let first = call(&mut client, 2, json!({"path":service.allowed})).await;
        if mode.starts_with("idle_") {
            // Either immediate poisoning or next-call refusal is safe. A successful
            // first response is untrusted; this does not prove response causality.
            if !refused(&first) {
                assert_eq!(upstream(&first)["isError"], false);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        } else {
            assert!(refused(&first), "{mode}");
        }
        assert!(
            refused(&call(&mut client, 3, json!({"path":service.allowed})).await),
            "{mode}"
        );
        client.finish().await;
        service.finish(true).await;
    }
}

async fn started_witness(service: &Running) -> Value {
    let path = service.state.join("outbound-scratch/request-started.json");
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            match fs::read(&path) {
                Ok(bytes) => {
                    // The writer may still be filling its newly created file.
                    if let Ok(value) = serde_json::from_slice::<Value>(&bytes) {
                        break value;
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => panic!("request witness read failed: {error}"),
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("external fixture did not reach the held-result gate")
}
#[tokio::test(flavor = "current_thread")]
#[ignore = "requires owned service manager/cgroup and pinned FORGE_TEST_MCP_SERVER fixture"]
async fn pinned_input_drift_after_external_read_refuses_held_result_and_reuse() {
    let service = Running::start(Some("gated")).await;
    let original = sha(&fs::read(&service.allowed).unwrap());
    let mut client = connect(&service.state).await;
    client
        .send(
            json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{
        "name":"outbound_read_file","arguments":{"path":service.allowed}}}),
        )
        .await;
    let witness = started_witness(&service).await;
    // The outbound session owns independent IDs: initialize1, list2, call3.
    assert_eq!(witness["request_id"], 3);
    assert_eq!(
        witness["response_sha256"], original,
        "witness must bind an actual held original response"
    );
    fs::write(
        &service.allowed,
        b"changed pinned input after external read\n",
    )
    .unwrap();
    assert_ne!(sha(&fs::read(&service.allowed).unwrap()), original);
    let gate = service
        .state
        .parent()
        .unwrap()
        .join("operator-gate/release");
    let next = gate.with_file_name("release-next");
    fs::write(&next, b"release\n").unwrap();
    fs::rename(next, gate).unwrap();
    let reply = client.receive().await;
    assert_eq!(reply["id"], 2);
    assert!(refused(&reply), "held result accepted after drift: {reply}");
    let status = service.status().await;
    assert_eq!(status["outbound_mcp"]["state"], "failed", "{status}");
    let reason = status["outbound_mcp"]["error"].as_str().unwrap();
    assert!(
        reason.contains("input identity changed")
            || reason.contains("input changed or digest mismatch"),
        "unexpected refusal: {status}"
    );
    assert_eq!(status["outbound_mcp"]["launches"], 1);
    assert!(refused(
        &call(&mut client, 3, json!({"path":service.allowed})).await
    ));
    client.finish().await;
    service.finish(true).await;
}
#[tokio::test(flavor = "current_thread")]
#[ignore = "requires owned service manager/cgroup and pinned FORGE_TEST_MCP_SERVER fixture"]
async fn pinned_input_drift_before_warm_call_refuses_endpoint_reuse() {
    let service = Running::start(Some("normal")).await;
    let mut client = connect(&service.state).await;
    let reply = call(&mut client, 2, json!({"path":service.allowed})).await;
    assert_eq!(upstream(&reply)["structuredContent"]["call_count"], 1);
    fs::write(&service.allowed, b"changed pinned input before warm call\n").unwrap();
    let reply = call(&mut client, 3, json!({"path":service.allowed})).await;
    assert!(refused(&reply), "warm drift accepted: {reply}");
    let status = service.status().await;
    assert_eq!(status["outbound_mcp"]["state"], "failed", "{status}");
    let reason = status["outbound_mcp"]["error"].as_str().unwrap();
    assert!(
        reason.contains("input identity changed")
            || reason.contains("input changed or digest mismatch"),
        "unexpected refusal: {status}"
    );
    assert_eq!(status["outbound_mcp"]["launches"], 1);
    assert!(refused(
        &call(&mut client, 4, json!({"path":service.allowed})).await
    ));
    client.finish().await;
    service.finish(true).await;
}

fn observation_oracle(reply: &Value, expected: &[u8], index: u64, pid: &mut Option<u32>) {
    let result = upstream(reply);
    assert_eq!(result["isError"], false, "{reply}");
    let data = &result["structuredContent"];
    assert_eq!(data["bytes"], expected.len());
    assert_eq!(data["sha256"], sha(expected));
    assert_eq!(data["text"], std::str::from_utf8(expected).unwrap());
    assert_eq!(data["secret_present"], false);
    assert_eq!(data["call_count"], index + 1);
    let observed = data["pid"]
        .as_u64()
        .and_then(|p| u32::try_from(p).ok())
        .unwrap();
    if let Some(held) = *pid {
        assert_eq!(observed, held);
    } else {
        *pid = Some(observed);
    }
}
async fn observe_roundtrips(report: &mut outbound_observation::Report) {
    use outbound_observation::{identity, resources};
    let t = report.start("service_ready");
    let service = Running::start(Some("measurement")).await;
    report.done("service_ready", t);
    report.data["cleanup"] = json!({"verified":false,"fixture_root":service.state.parent()});
    let t = report.start("stdio_initialize");
    let mut client = connect(&service.state).await;
    report.done("stdio_initialize", t);
    let t = report.start("tools_discovery");
    client
        .send(json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}))
        .await;
    let listed = client.receive().await;
    assert!(listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|t| t["name"] == "outbound_read_file"));
    report.done("tools_discovery", t);
    report.data["catalog_sha256"] = json!(sha(&serde_json::to_vec(&listed["result"]).unwrap()));
    let request = json!({"name":"outbound_read_file","arguments":{"path":service.allowed}});
    let t = report.start("input_capture");
    let before = identity(
        &service.state,
        Path::new(forge_binary::path()),
        &service.binary,
        &request,
    );
    report.done("input_capture", t);
    report.data["identity_before"] = before.clone();
    let service_pid = service.status().await["pid"].as_u64().unwrap() as u32;
    report.data["resources_before"] = resources(client.pid(), service_pid, None);
    let expected = fs::read(&service.allowed).unwrap();
    let mut endpoint = None;
    for i in 0..32 {
        let name = format!("call_{i}");
        let phase = report.start(&name);
        let started = std::time::Instant::now();
        let reply = tokio::time::timeout(Duration::from_secs(3), async {
            client
                .try_send(json!({"jsonrpc":"2.0","id":i+3,"method":"tools/call","params":request}))
                .await?;
            client.try_receive().await
        })
        .await;
        let transport_ns = started.elapsed().as_nanos() as u64;
        let reply = match reply {
            Ok(value) => value.unwrap(),
            Err(error) => {
                report.censored(&name);
                panic!("roundtrip deadline: {error}")
            }
        };
        let started = std::time::Instant::now();
        assert_eq!(reply["id"], i + 3);
        observation_oracle(&reply, &expected, i, &mut endpoint);
        let oracle_ns = started.elapsed().as_nanos() as u64;
        report.data["calls"].as_array_mut().unwrap().push(json!({"index":i,"phase":if i==0 {"first"}else{"warm"},
            "transport_ns":transport_ns,"oracle_ns":oracle_ns,"request_sha256":before["request_sha256"],
            "response_sha256":sha(&serde_json::to_vec(&reply).unwrap())}));
        report.done(&name, phase);
        if i == 0 {
            report.data["endpoint_after_first"] =
                resources(client.pid(), service_pid, endpoint)["endpoint"].clone();
        }
    }
    report.data["resources_after"] = resources(client.pid(), service_pid, endpoint);
    let t = report.start("input_recheck");
    let after = identity(
        &service.state,
        Path::new(forge_binary::path()),
        &service.binary,
        &request,
    );
    report.data["identity_after"] = after.clone();
    assert_eq!(before, after, "observed inputs changed");
    report.done("input_recheck", t);
    let t = report.start("adapter_eof");
    client.try_finish().await.unwrap();
    report.done("adapter_eof", t);
    let t = report.start("service_cleanup");
    let disposition = service.finish(true).await;
    report.done("service_cleanup", t);
    report.data["cleanup"] = json!({"verified":true,"disposition":disposition});
}
#[tokio::test(flavor = "current_thread")]
#[ignore = "explicit current-only observation; requires unique FORGE_TEST_OUTBOUND_REPORT and owned pinned fixture"]
async fn outbound_current_roundtrip_observation() {
    let mut report = outbound_observation::Report::new();
    report.data["timing_scope"] = json!({"service_ready":"fixture setup/copy/hash/policy plus managed-service launch/readiness",
        "stdio_initialize":"inbound adapter launch and MCP initialize",
        "tools_discovery":"inbound harness catalog only; remote initialize and tools/list are included in first call",
        "first":"cold lazy endpoint launch/discovery plus tools/call","warm":"same endpoint, sequential, zero retries"});
    report.data["deadline_scope"]=json!("25s cooperative async deadline; synchronous hashing/stop cannot be preempted; caller must supply an independent outer watchdog and preserve uncertain cleanup");
    let outcome =
        tokio::time::timeout(Duration::from_secs(25), observe_roundtrips(&mut report)).await;
    if outcome.is_err() {
        report.deadline();
        panic!("cooperative observation deadline exceeded; fixture retained if cleanup unverified");
    }
    report.finish();
}
