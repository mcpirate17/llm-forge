#![cfg(target_os = "linux")]
//! Opt-in real Forge report parity and separately labeled synthetic owner controls.
//! Requires managed-service delegation, explicit pins and runtime capabilities.
#[path = "support/test_report_service.rs"]
mod fixture;
#[path = "support/mcp_client.rs"]
mod mcp_client;
use fixture::{missing, Running};
use serde_json::{json, Value};
use std::{fs, path::Path, time::Duration};
use tokio::io::{AsyncWriteExt, BufReader};

fn request(id: u64, arguments: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"method":"tools/call",
        "params":{"name":"python_test_report","arguments":arguments}})
}
fn result(reply: &Value) -> &Value {
    assert_eq!(reply["result"]["isError"], false, "{reply}");
    &reply["result"]["structuredContent"]["result"]
}
fn refused(reply: &Value) {
    assert!(
        reply.get("error").is_some() || reply["result"]["isError"] == true,
        "{reply}"
    );
}
async fn call(service: &Running, client: &mut mcp_client::Mcp, id: u64, max: usize) -> Value {
    service.save(
        &format!("request-{id}"),
        request(id, json!({"max_output_bytes":max})),
    );
    client
        .send(request(id, json!({"max_output_bytes":max})))
        .await;
    let reply = client.receive().await;
    assert_eq!(reply["id"], id);
    service.save(&format!("reply-{id}"), reply.clone());
    reply
}
fn witness_processes(status: &Value, witness: &Value) -> Value {
    let mut records = Vec::new();
    assert_eq!(witness["secret_present"], false);
    for name in ["pid", "child"] {
        let pid = witness[name].as_u64().unwrap();
        if pid == 0 {
            continue;
        }
        let groups = fs::read_to_string(format!("/proc/{pid}/cgroup")).unwrap();
        let relative = groups
            .lines()
            .find_map(|line| line.strip_prefix("0::"))
            .unwrap();
        assert_eq!(
            Path::new("/sys/fs/cgroup").join(relative.trim_start_matches('/')),
            Path::new(
                status["python_test_report"]["cgroup_path"]
                    .as_str()
                    .unwrap()
            )
        );
        let stat = fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
        if name == "child" {
            let session: u64 = stat
                .rsplit_once(')')
                .unwrap()
                .1
                .split_whitespace()
                .nth(3)
                .unwrap()
                .parse()
                .unwrap();
            assert_eq!(
                session, pid,
                "fixture child did not establish its own session"
            );
            for fd in [0, 1, 2] {
                assert_eq!(
                    fs::read_link(format!("/proc/{pid}/fd/{fd}")).unwrap(),
                    Path::new("/dev/null")
                );
            }
        }
        records.push(json!({"role":name,"pid":pid,"stat":stat,"cgroup":groups}));
    }
    json!(records)
}
fn witness_absent(witness: &Value) {
    for name in ["pid", "child"] {
        let pid = witness[name].as_u64().unwrap();
        if pid != 0 {
            missing(Path::new(&format!("/proc/{pid}")));
        }
    }
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires owned managed service, FORGE_TEST_BIN + SHA256, explicit FORGE_TEST_REPORT_RUNTIME_READS"]
async fn real_forge_worker_mcp_report_matches_direct_cli_and_cleans_owned_resources() {
    tokio::time::timeout(Duration::from_secs(90), async {
        let service = Running::start(false, true, 20).await;
        let expected = service.direct();
        let mut client = service.connect().await;
        client
            .send(json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}))
            .await;
        let tools = client.receive().await;
        service.save("discovery", tools.clone());
        assert!(tools["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["name"] == "python_test_report"));
        let reply = call(&service, &mut client, 2, 262144).await;
        assert_eq!(result(&reply), &expected);
        assert_eq!(
            expected["report"]["broken"][0]["missing"],
            json!(["pkg.missing"])
        );
        let status = service.status().await;
        assert_eq!(status["python_test_report"]["launches"], 1);
        assert_eq!(status["python_test_report"]["calls"], 1);
        service.cleaned(&status).await;
        missing(&service.source.join("tasks"));
        client.finish().await;
        service.finish().await;
    })
    .await
    .expect("owned service control exceeded cooperative test bound");
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires owned managed service, pinned real Forge and explicit runtime reads"]
async fn disabled_and_invalid_report_requests_do_not_launch_a_worker() {
    tokio::time::timeout(Duration::from_secs(90), async {
        for enabled in [false, true] {
            let service = Running::start(false, enabled, 20).await;
            let mut client = service.connect().await;
            let initial_groups = service.group_inventory().await;
            client
                .send(json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}))
                .await;
            let listed = client.receive().await;
            assert_eq!(
                listed["result"]["tools"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|tool| tool["name"] == "python_test_report"),
                enabled
            );
            for (index, args) in [
                json!({}),
                json!({"max_output_bytes":4096,"unknown":true}),
                json!({"max_output_bytes":"4096"}),
            ]
            .into_iter()
            .enumerate()
            {
                client.send(request(index as u64 + 2, args)).await;
                refused(&client.receive().await);
            }
            let status = service.status().await;
            service.save("unstarted-status", status.clone());
            if enabled {
                assert_eq!(status["python_test_report"]["launches"], 0);
                assert!(status["python_test_report"]["scratch_path"].is_null());
                assert!(status["python_test_report"]["cgroup_path"].is_null());
            } else {
                assert!(status.get("python_test_report").is_none());
            }
            let final_groups = service.group_inventory().await;
            assert_eq!(initial_groups, final_groups);
            service.save(
                "no-child-groups",
                json!({"before":initial_groups,"after":final_groups}),
            );
            client.finish().await;
            service.finish().await;
        }
    })
    .await
    .expect("owned service control exceeded cooperative test bound");
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires pinned standalone synthetic worker + SHA256, owned service and explicit runtime reads"]
async fn synthetic_worker_timeout_invalid_output_budget_and_read_confinement_are_distinct() {
    tokio::time::timeout(Duration::from_secs(90), async {
        for mode in ["hang", "invalid", "large", "denied"] {
            let service = Running::start(true, true, if mode == "hang" { 1 } else { 20 }).await;
            service.mode(mode);
            let mut client = service.connect().await;
            let reply = call(&service, &mut client, 1, 4096).await;
            if mode == "denied" {
                assert_eq!(result(&reply)["report"]["synthetic_fixture"], true);
            } else {
                refused(&reply);
                let status = service.status().await;
                let reason = status["python_test_report"]["error"].as_str().unwrap();
                assert!(
                    reason.contains(match mode {
                        "hang" => "TimedOut",
                        "invalid" => "invalid report worker JSON",
                        _ => "requested output budget",
                    }),
                    "{status}"
                );
            }
            let status = service.status().await;
            assert_eq!(status["python_test_report"]["launches"], 1);
            service.cleaned(&status).await;
            client.finish().await;
            service.finish().await;
        }
    })
    .await
    .expect("owned service control exceeded cooperative test bound");
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires pinned synthetic detached-worker fixture, owned service/cgroup and explicit runtime reads"]
async fn synthetic_report_cancellation_and_adapter_eof_clean_detached_tree_before_reuse() {
    tokio::time::timeout(Duration::from_secs(90), async {
    for disconnect in [false, true] {
        let service = Running::start(true, true, 20).await;
        service.mode("descendant");
        let mut client = service.connect().await;
        client
            .send(request(2, json!({"max_output_bytes":4096})))
            .await;
        let (active, witness) = service.active().await;
        assert!(witness["child"].as_u64().unwrap() > 0);
        service.save("active-processes", witness_processes(&active, &witness));
        if disconnect {
            client.finish().await;
            service.cleaned(&active).await;
            witness_absent(&witness);
            client = service.connect().await;
        } else {
            client.send(json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":2}})).await;
            client
                .send(json!({"jsonrpc":"2.0","id":3,"method":"ping"}))
                .await;
            assert_eq!(
                client.receive().await,
                json!({"jsonrpc":"2.0","id":3,"result":{}})
            );
        }
        service.mode("normal");
        let reply = call(&service, &mut client, 4, 4096).await;
        assert_eq!(result(&reply)["report"]["synthetic_fixture"], true);
        service.cleaned(&active).await;
        witness_absent(&witness);
        let status = service.status().await;
        assert_eq!(status["python_test_report"]["launches"], 2);
        assert_eq!(status["python_test_report"]["calls"], 1);
        service.cleaned(&status).await;
        client.finish().await;
        service.finish().await;
    }
    })
    .await
    .expect("owned service control exceeded cooperative test bound");
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires pinned synthetic worker, owned service/cgroup and explicit runtime reads"]
async fn synthetic_early_halfclose_and_single_write_pipeline_clean_before_reuse() {
    tokio::time::timeout(Duration::from_secs(90), async {
        for pipeline in [false, true] {
            let service = Running::start(true, true, 20).await;
            service.mode("hang");
            let initial_groups = service.group_inventory().await;
            let mut socket = tokio::net::UnixStream::connect(service.state.join("socket")).await.unwrap();
            let mut bytes = serde_json::to_vec(&json!({"method":"call","call":{"name":"python_test_report","arguments":{"max_output_bytes":4096}}})).unwrap();
            bytes.push(b'\n');
            if pipeline { bytes.extend_from_slice(b"{\"method\":\"status\"}\n"); }
            tokio::time::timeout(Duration::from_secs(3), socket.write_all(&bytes)).await.unwrap().unwrap();
            if !pipeline { socket.shutdown().await.unwrap(); }
            let mut input = BufReader::new(socket); let mut reply = Vec::new();
            assert!(tokio::time::timeout(Duration::from_secs(5),
                forge::harness::transport::read_frame(&mut input, &mut reply, 8192)).await.unwrap().unwrap());
            let value: Value = serde_json::from_slice(&reply).unwrap();
            service.save("raw-early-reply", value.clone());
            assert_eq!(value["ok"], false, "{value}");
            assert!(value["error"].as_str().unwrap().contains(if pipeline {"pipelined"} else {"write side"}), "{value}");
            let status = service.status().await; service.save("raw-refusal-status", status.clone());
            let launches = status["python_test_report"]["launches"].as_u64().unwrap();
            assert!(launches <= 1);
            assert_eq!(status["python_test_report"]["calls"], 0);
            assert_eq!(status["python_test_report"]["fatal"], false);
            if status["python_test_report"]["cgroup_path"].is_string() { service.cleaned(&status).await; }
            else { assert!(status["python_test_report"]["scratch_path"].is_null()); }
            let after_refusal = service.group_inventory().await;
            assert_eq!(initial_groups, after_refusal);
            service.save("early-clean-groups", json!({"before":initial_groups,"after":after_refusal,"launches":launches}));
            service.mode("normal"); let mut client = service.connect().await;
            assert_eq!(result(&call(&service, &mut client, 1, 4096).await)["report"]["synthetic_fixture"], true);
            let reused = service.status().await;
            assert_eq!(reused["python_test_report"]["launches"], launches + 1);
            assert_eq!(reused["python_test_report"]["calls"], 1);
            service.cleaned(&reused).await;
            client.finish().await; service.finish().await;
        }
    }).await.expect("owned service control exceeded cooperative test bound");
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires pinned synthetic detached-worker fixture, owned service/cgroup and explicit runtime reads"]
async fn synthetic_active_report_halfclose_and_pipeline_clean_tree_before_reuse() {
    tokio::time::timeout(Duration::from_secs(90), async {
        for pipeline in [false, true] {
            let service = Running::start(true, true, 20).await;
            service.mode("descendant");
            let mut socket = tokio::net::UnixStream::connect(service.state.join("socket")).await.unwrap();
            let request = json!({"method":"call","call":{"name":"python_test_report","arguments":{"max_output_bytes":4096}}});
            service.save("raw-active-request", request.clone());
            let mut bytes = serde_json::to_vec(&request).unwrap(); bytes.push(b'\n');
            tokio::time::timeout(Duration::from_secs(3), socket.write_all(&bytes)).await.unwrap().unwrap();
            let (active, witness) = service.active().await;
            assert!(witness["child"].as_u64().unwrap() > 0);
            service.save("active-processes", witness_processes(&active, &witness));
            if pipeline {
                tokio::time::timeout(Duration::from_secs(3), socket.write_all(b"{\"method\":\"status\"}\n")).await.unwrap().unwrap();
            } else { socket.shutdown().await.unwrap(); }
            let mut input = BufReader::new(socket); let mut bytes = Vec::new();
            assert!(tokio::time::timeout(Duration::from_secs(5),
                forge::harness::transport::read_frame(&mut input, &mut bytes, 8192)).await.unwrap().unwrap());
            let reply: Value = serde_json::from_slice(&bytes).unwrap();
            service.save("raw-active-reply", reply.clone());
            assert_eq!(reply["ok"], false, "{reply}");
            assert!(reply["error"].as_str().unwrap().contains(if pipeline {"pipelined"} else {"write side"}), "{reply}");
            service.cleaned(&active).await; witness_absent(&witness);
            service.mode("normal"); let mut client = service.connect().await;
            assert_eq!(result(&call(&service, &mut client, 1, 4096).await)["report"]["synthetic_fixture"], true);
            let status = service.status().await;
            assert_eq!(status["python_test_report"]["launches"], 2);
            assert_eq!(status["python_test_report"]["calls"], 1);
            service.cleaned(&status).await; missing(&service.source.join("tasks"));
            client.finish().await; service.finish().await;
        }
    }).await.expect("owned service control exceeded cooperative test bound");
}
