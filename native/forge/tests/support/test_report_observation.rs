//! Opt-in as-configured API observation, not matched resource/isolation policy.
#[path = "process_usage.rs"]
mod process_usage;
use super::{fixture, mcp_client};
use anyhow::{ensure, Context, Result};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::PathBuf,
    time::{Duration, Instant},
};
const PAIRS: usize = 31;
const MAX: usize = 262144;

struct Observation {
    file: fs::File,
    data: Value,
}
impl Observation {
    fn new() -> Self {
        let path = PathBuf::from(
            std::env::var_os("FORGE_TEST_REPORT_OBSERVATION")
                .expect("set unique absolute FORGE_TEST_REPORT_OBSERVATION output path"),
        );
        assert!(path.is_absolute());
        let file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .unwrap();
        let mut phases: Vec<_> = [
            "setup_ready",
            "stdio_initialize",
            "discovery",
            "capture_before",
            "first_cli",
            "first_mcp",
            "warmup_cli",
            "warmup_mcp",
        ]
        .into_iter()
        .map(|name| json!({"name":name,"state":"unstarted"}))
        .collect();
        for pair in 0..PAIRS {
            for arm in ["cli", "mcp"] {
                phases.push(json!({"name":format!("pair_{pair}_{arm}"),"state":"unstarted"}));
            }
        }
        for name in ["capture_after", "adapter_eof", "service_cleanup"] {
            phases.push(json!({"name":name,"state":"unstarted"}));
        }
        let mut result = Self {
            file,
            data: json!({"schema":"forge.test-report-observation.v1","valid":false,
            "output":path,"phases":phases,"calls":[],"cleanup":{"verified":false},"limits":{"cooperative_observation_seconds":90,"service_lifetime_seconds":60,"worker_timeout_seconds":20,"cli_timeout_seconds":20},
            "scope":"same authored committed fixture and release executable; 1 first +1 unscored warmup +31 alternating single-call pairs; no retries",
            "limitations":["as-configured comparison: MCP worker500mCPU/128MiB; directCLI has no equivalent worker cgroup cap",
                "MCP warm reuses service/adapter, but every report launches a fresh confined worker; filesystem caches uncontrolled",
                "CLI interval includes native process spawn/capture/reap; MCP includes framing/socket/worker/JSON conversion; oracle and retained-log writes measured separately",
                "service setup includes private executable copy, fixture creation, hashing, policy validation, launch and readiness",
                "stdio setup includes adapter spawn, initialize reply and initialized notification write; notification has no processing acknowledgment",
                "process CPU is coarse kernel ticks; RSS/HWM point samples are not allocation counts or aggregate tree peaks",
                "short-lived CLI/worker CPU and RSS unavailable; outer wait4 receipts exclude separately managed service/workers",
                "cooperative90second observation timeout cannot preempt synchronous hashing or service stop; cleanup accounted independently",
                "no cold-host, clean-install, task benefit, agent/model/token or speedup claim",
                "process_probe errors may not expose partial CLI stream bytes; such missing streams remain unavailable and invalidate the observation"]}),
        };
        result.persist().unwrap();
        result
    }
    fn phase(&mut self, name: &str) -> &mut Value {
        self.data["phases"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|p| p["name"] == name)
            .unwrap()
    }
    fn start(&mut self, name: &str) -> Instant {
        assert_eq!(self.phase(name)["state"], "unstarted");
        self.phase(name)["state"] = json!("running");
        self.persist().unwrap();
        Instant::now()
    }
    fn done(&mut self, name: &str, began: Instant) {
        let phase = self.phase(name);
        phase["state"] = json!("completed");
        phase["elapsed_ns"] = json!(began.elapsed().as_nanos() as u64);
        self.persist().unwrap();
    }
    fn record<T>(&mut self, name: &str, began: Instant, result: &Result<T>) {
        let phase = self.phase(name);
        phase["state"] = json!(match result {
            Ok(_) => "completed",
            Err(error)
                if error.is::<forge::process_probe::TimedOut>()
                    || error.is::<tokio::time::error::Elapsed>() =>
                "censored",
            Err(_) => "failed",
        });
        phase["elapsed_ns"] = json!(began.elapsed().as_nanos() as u64);
        self.persist().unwrap();
    }
    fn interrupted(&mut self, censored: bool) {
        self.data["valid"] = json!(false);
        for phase in self.data["phases"].as_array_mut().unwrap() {
            if phase["state"] == "running" {
                phase["state"] = json!(if censored { "censored" } else { "failed" });
            }
        }
        self.persist().unwrap();
    }
    fn persist(&mut self) -> std::io::Result<()> {
        let phases = self.data["phases"].as_array().unwrap();
        let mut counts = json!({"planned":phases.len(),"completed":0,"failed":0,"censored":0,"running":0,"unstarted":0});
        for phase in phases {
            let key = phase["state"].as_str().unwrap();
            counts[key] = json!(counts[key].as_u64().unwrap() + 1);
        }
        self.data["accounting"] = counts;
        let mut calls =
            json!({"planned":66,"completed":0,"failed":0,"censored":0,"running":0,"unstarted":0});
        for phase in self.data["phases"].as_array().unwrap() {
            let name = phase["name"].as_str().unwrap();
            if ["first_", "warmup_", "pair_"]
                .iter()
                .any(|prefix| name.starts_with(prefix))
            {
                let key = phase["state"].as_str().unwrap();
                calls[key] = json!(calls[key].as_u64().unwrap() + 1);
            }
        }
        self.data["call_accounting"] = calls;
        self.file.set_len(0)?;
        use std::io::Seek;
        self.file.rewind()?;
        self.file
            .write_all(&serde_json::to_vec_pretty(&self.data)?)?;
        self.file.sync_all()
    }
}
impl Drop for Observation {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.data["valid"] = json!(false);
        }
        for phase in self.data["phases"].as_array_mut().unwrap() {
            if phase["state"] == "running" {
                phase["state"] = json!(if std::thread::panicking() {
                    "failed"
                } else {
                    "censored"
                });
            }
        }
        if let Err(error) = self.persist() {
            eprintln!("observation evidence write failed: {error}");
        }
    }
}
fn digest(value: &Value) -> String {
    format!("{:x}", Sha256::digest(serde_json::to_vec(value).unwrap()))
}
fn identity(service: &fixture::Running) -> Value {
    let mut paths = vec![service.source.clone()];
    let mut hashes = serde_json::Map::new();
    while let Some(directory) = paths.pop() {
        for entry in fs::read_dir(directory).unwrap() {
            let entry = entry.unwrap();
            let kind = entry.file_type().unwrap();
            if kind.is_dir() {
                paths.push(entry.path());
            } else {
                assert!(kind.is_file());
                hashes.insert(
                    entry
                        .path()
                        .strip_prefix(&service.source)
                        .unwrap()
                        .to_str()
                        .unwrap()
                        .into(),
                    json!(fixture::sha(&entry.path())),
                );
            }
            assert!(hashes.len() + paths.len() <= 512);
        }
    }
    let policy: Value =
        serde_json::from_slice(&fs::read(service.state.join("policy.json")).unwrap()).unwrap();
    assert_eq!(
        policy["test_report"]["executable"]["path"],
        json!(service.binary)
    );
    assert_eq!(
        policy["test_report"]["executable"]["sha256"],
        fixture::sha(&service.binary)
    );
    json!({"source_files":hashes,"policy":policy,"binary_sha256":fixture::sha(&service.binary),
        "test_executable_sha256":fixture::sha(&std::env::current_exe().unwrap()),
        "observation_source_sha256":format!("{:x}",Sha256::digest(include_bytes!("test_report_observation.rs"))),
        "fixture_helper_sha256":format!("{:x}",Sha256::digest(include_bytes!("test_report_service.rs")))})
}
fn resources(client: &mcp_client::Mcp, service_pid: u32) -> Value {
    json!({"driver":process_usage::process(std::process::id()),"adapter":process_usage::process(client.pid()),
        "service":process_usage::process(service_pid),"worker":{"available":false},"direct_cli":{"available":false}})
}
fn resource_delta(before: &Value, after: &Value) -> Value {
    let mut rows = serde_json::Map::new();
    for name in ["driver", "adapter", "service"] {
        let (a, b) = (&before[name], &after[name]);
        let same = a["available"] == true
            && b["available"] == true
            && a["pid"] == b["pid"]
            && a["start_ticks"] == b["start_ticks"];
        let ticks = if same {
            b["cpu_ticks"]
                .as_u64()
                .unwrap()
                .checked_sub(a["cpu_ticks"].as_u64().unwrap())
        } else {
            None
        };
        rows.insert(
            name.into(),
            json!({"same_process_identity":same,"cpu_ticks_delta":ticks}),
        );
    }
    json!({"scope":"before first call through post-call identity hashing/logs/oracles; not per-call attribution", "components":rows})
}
fn summary(calls: &[Value], arm: &str) -> Value {
    let mut times: Vec<_> = calls
        .iter()
        .filter(|row| row["scored"] == true && row["arm"] == arm)
        .map(|row| row["roundtrip_ns"].as_u64().unwrap())
        .collect();
    times.sort();
    assert_eq!(times.len(), PAIRS);
    // Same nearest-rank definition used by native performance receipts; their
    // Sample also requires CPU/RSS, which must not be fabricated for this path.
    json!({"samples":times.len(),"p50_ns":times[15],"p95_ns":times[29]})
}
fn cli(service: &fixture::Running, label: &str) -> Result<(Value, Value, u64)> {
    let command = service.direct_command();
    let argv: Vec<_> = std::iter::once(command.get_program())
        .chain(command.get_args())
        .map(|s| s.to_str().unwrap())
        .collect();
    let began = Instant::now();
    let result = service.direct_output(Duration::from_secs(20));
    let output = result?;
    let value = serde_json::from_slice::<Value>(&output.stdout);
    let elapsed = began.elapsed().as_nanos() as u64;
    let stdout = service.state.join(format!("observation-{label}.stdout"));
    let stderr = service.state.join(format!("observation-{label}.stderr"));
    for (path, bytes) in [(&stdout, &output.stdout), (&stderr, &output.stderr)] {
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)?
            .write_all(bytes)?;
    }
    let raw = json!({"argv":argv,"exit_code":output.status.code(),"stdout":stdout,"stderr":stderr,
        "stdout_sha256":fixture::sha(&stdout),"stderr_sha256":fixture::sha(&stderr)});
    ensure!(output.status.success(), "CLI failed: {raw}");
    Ok((value?, raw, elapsed))
}
async fn mcp(
    service: &fixture::Running,
    label: &str,
    client: &mut mcp_client::Mcp,
    id: u64,
) -> Result<(Value, Value, u64)> {
    let began = Instant::now();
    client
        .try_send(super::request(id, json!({"max_output_bytes":MAX})))
        .await?;
    let reply = client.try_receive().await?;
    let elapsed = began.elapsed().as_nanos() as u64;
    service.save(
        &format!("observation-{label}-parsed-mcp-reply"),
        reply.clone(),
    );
    ensure!(
        reply["id"] == id && reply["result"]["isError"] == false,
        "MCP failed: {reply}"
    );
    let structured = &reply["result"]["structuredContent"];
    let text = reply["result"]["content"][0]["text"]
        .as_str()
        .context("MCP text missing")?;
    ensure!(
        serde_json::from_str::<Value>(text)? == *structured,
        "MCP text/structured disagreement"
    );
    Ok((structured["result"].clone(), reply, elapsed))
}
struct Call {
    name: String,
    arm: &'static str,
    id: u64,
    scored: bool,
}
async fn operation(
    observation: &mut Observation,
    service: &fixture::Running,
    client: &mut mcp_client::Mcp,
    call: Call,
    expected: Option<&Value>,
) -> Result<Value> {
    let started = observation.start(&call.name);
    observation.phase(&call.name)["semantic_request"] =
        super::request(call.id, json!({"max_output_bytes":MAX}));
    observation.phase(&call.name)["raw_scope"] = json!("CLI unique stdout/stderr when Output returned; MCP parsed JSON reply retained; raw transport bytes unavailable");
    if call.arm == "cli" {
        let command = service.direct_command();
        let argv: Vec<_> = std::iter::once(command.get_program())
            .chain(command.get_args())
            .map(|value| value.to_str().unwrap())
            .collect();
        observation.phase(&call.name)["argv"] = json!(argv);
        observation.phase(&call.name)["raw_stdout"] = json!(service
            .state
            .join(format!("observation-{}.stdout", call.name)));
        observation.phase(&call.name)["raw_stderr"] = json!(service
            .state
            .join(format!("observation-{}.stderr", call.name)));
    } else {
        observation.phase(&call.name)["rpc_request"] =
            super::request(call.id, json!({"max_output_bytes":MAX}));
    }
    observation.persist()?;
    let result = async {
        let (value, raw, elapsed) = if call.arm == "cli" { cli(service,&call.name)? }
            else { mcp(service,&call.name,client,call.id).await? };
        let oracle = Instant::now();
        let matches = expected.is_none_or(|expected| value == *expected);
        let complete = value["schema_version"] == 1 && value["advisory"] == true
            && value["root"] == json!(service.source) && value["report"]["tests_scanned"] == 1
            && value["report"]["modules_scanned"] == 2;
        let oracle_ns = oracle.elapsed().as_nanos() as u64;
        let row = json!({"name":call.name,"arm":call.arm,"scored":call.scored,"roundtrip_ns":elapsed,
            "oracle_ns":oracle_ns,"oracle_matches":matches,"complete_fixture_classification":complete,
            "output_sha256":digest(&value),"raw":raw});
        service.save(&format!("observation-{}",call.name),row.clone());
        observation.data["calls"].as_array_mut().unwrap().push(row);
        ensure!(matches && complete,"full report parity/completeness mismatch: {}",call.name);
        Ok(value)
    }.await;
    observation.record(&call.name, started, &result);
    result
}
async fn measure_calls(
    observation: &mut Observation,
    service: &fixture::Running,
    client: &mut mcp_client::Mcp,
) -> Result<()> {
    let first = Call {
        name: "first_cli".into(),
        arm: "cli",
        id: 2,
        scored: false,
    };
    let expected = operation(observation, service, client, first, None).await?;
    let first = Call {
        name: "first_mcp".into(),
        arm: "mcp",
        id: 2,
        scored: false,
    };
    operation(observation, service, client, first, Some(&expected)).await?;
    for arm in ["cli", "mcp"] {
        let call = Call {
            name: format!("warmup_{arm}"),
            arm,
            id: 3,
            scored: false,
        };
        operation(observation, service, client, call, Some(&expected)).await?;
    }
    for pair in 0..PAIRS {
        let order = if pair.is_multiple_of(2) {
            ["cli", "mcp"]
        } else {
            ["mcp", "cli"]
        };
        for arm in order {
            let call = Call {
                name: format!("pair_{pair}_{arm}"),
                arm,
                id: pair as u64 + 4,
                scored: true,
            };
            operation(observation, service, client, call, Some(&expected)).await?;
        }
    }
    Ok(())
}
async fn observe(observation: &mut Observation) -> Result<()> {
    let began = observation.start("setup_ready");
    let service = fixture::Running::start(false, true, 20).await;
    observation.data["state"] = json!(service.state);
    observation.done("setup_ready", began);
    let began = observation.start("stdio_initialize");
    let mut client = service.connect().await;
    observation.done("stdio_initialize", began);
    let began = observation.start("discovery");
    client
        .try_send(json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}))
        .await?;
    let catalog = client.try_receive().await?;
    ensure!(
        catalog["id"] == 1 && catalog["result"]["tools"].is_array(),
        "discovery failed"
    );
    let request = json!({"name":"python_test_report","arguments":{"max_output_bytes":MAX}});
    forge::tool_schema::ToolRegistry::from_json(&catalog["result"].to_string())?
        .validate_call(&request.to_string(), false)?;
    observation.data["catalog_sha256"] = json!(digest(&catalog["result"]));
    service.save("observation-discovery", catalog);
    observation.done("discovery", began);
    let began = observation.start("capture_before");
    let before = identity(&service);
    observation.data["inputs_before"] = before.clone();
    let service_pid = service.status().await["pid"].as_u64().unwrap() as u32;
    observation.data["resources_before"] = resources(&client, service_pid);
    observation.done("capture_before", began);
    measure_calls(observation, &service, &mut client).await?;
    let began = observation.start("capture_after");
    let after = identity(&service);
    observation.data["inputs_after"] = after.clone();
    observation.data["resources_after"] = resources(&client, service_pid);
    observation.data["resources_delta"] = resource_delta(
        &observation.data["resources_before"],
        &observation.data["resources_after"],
    );
    let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    observation.data["clock_ticks_per_second"] = json!(if ticks > 0 { Some(ticks) } else { None });
    ensure!(before == after, "source/executable/policy drift");
    observation.done("capture_after", began);
    let calls = observation.data["calls"].as_array().unwrap();
    observation.data["metrics"] = json!({"cli":summary(calls,"cli"),"mcp":summary(calls,"mcp")});
    let began = observation.start("adapter_eof");
    client.try_finish().await?;
    observation.done("adapter_eof", began);
    service.save("observation-before-cleanup", observation.data.clone());
    let state = service.state.clone();
    let began = observation.start("service_cleanup");
    service.finish().await;
    observation.done("service_cleanup", began);
    observation.data["cleanup"] = json!({"verified":true,"state":state,"record_scope":"see separately retained service-cleanup/cleanup-observation records"});
    ensure!(
        observation.data["phases"]
            .as_array()
            .unwrap()
            .iter()
            .all(|phase| phase["state"] == "completed"),
        "observation has incomplete phases"
    );
    observation.data["valid"] = json!(true);
    observation.persist()?;
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(state.join("observation-final.json"))?
        .write_all(&serde_json::to_vec_pretty(&observation.data)?)?;
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "explicit as-configured observation; requires unique FORGE_TEST_REPORT_OBSERVATION, pinned Forge and runtime grants"]
async fn matched_cli_mcp_python_test_report_observation() {
    let mut observation = Observation::new();
    match tokio::time::timeout(Duration::from_secs(90), observe(&mut observation)).await {
        Ok(Ok(())) => (),
        Ok(Err(error)) => {
            observation.data["error"] = json!(format!("{error:#}"));
            observation.interrupted(false);
            panic!("report observation failed: {error:#}");
        }
        Err(error) => {
            observation.data["error"] = json!(error.to_string());
            observation.interrupted(true);
            panic!("report observation cooperative deadline exceeded");
        }
    }
}
