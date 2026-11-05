//! Opt-in, current-only roundtrip measurements against an owned native service.
#[path = "../tests/support/forge_binary.rs"]
mod forge_binary;
#[path = "support/mcp_measurement.rs"]
mod mcp_measurement;
#[path = "support/memory_fixture.rs"]
mod memory_fixture;
#[path = "../tests/support/tokenizer_fixture.rs"]
mod tokenizer_fixture;
use anyhow::{ensure, Context, Result};
use forge::harness::{policy::Policy, transport};
use forge::managed_service::Service;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
const SOURCE: &str = "fn answer() -> u32 { 42 }\n";

struct Directory {
    path: PathBuf,
    preserve: bool,
}
impl Directory {
    fn remove(&mut self) -> Result<()> {
        self.preserve = false;
        fs::remove_dir_all(&self.path)?;
        ensure!(!self.path.exists(), "benchmark fixture survived cleanup");
        Ok(())
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        if self.path.exists() {
            if self.preserve {
                eprintln!(
                    "benchmark fixture preserved at {}: process cleanup not verified",
                    self.path.display()
                );
            } else if let Err(error) = fs::remove_dir_all(&self.path) {
                eprintln!("benchmark fixture cleanup: {error}");
            }
        }
    }
}

fn prepare(base: PathBuf) -> Result<(Directory, PathBuf, Service, Instant)> {
    ensure!(
        std::env::var("FORGE_BENCH_SERVICE").as_deref() == Ok("1"),
        "set FORGE_BENCH_SERVICE=1 to authorize this bounded transient benchmark service"
    );
    fs::create_dir(&base)?;
    let owned = Directory {
        path: base.clone(),
        preserve: true,
    };
    fs::set_permissions(&base, fs::Permissions::from_mode(0o700))?;
    let source = base.join("source");
    let state = base.join("state");
    fs::create_dir(&source)?;
    fs::create_dir(&state)?;
    fs::set_permissions(&state, fs::Permissions::from_mode(0o700))?;
    fs::write(source.join("main.rs"), SOURCE)?;
    let tokenizer = tokenizer_fixture::Fixture::new();
    let mut tokenizer_config = tokenizer.config.clone();
    tokenizer_config.path = base.join("tokenizer.json");
    fs::copy(&tokenizer.config.path, &tokenizer_config.path)?;
    memory_fixture::memory_fixture(&base)?;
    conductor_native::memory_index_sidecar::build_sidecar(
        &base.join("memory.jsonl"),
        &base.join("memory.mapped"),
    )
    .map_err(anyhow::Error::msg)?;
    let policy = json!({"version":1,"source_root":source,"write_paths":[],"read_only":[],"programs":{},
        "workspace":{"identity":null,"max_chars":3500},
        "context":{"tokenizer":tokenizer_config,"max_input_bytes":16384,"max_tokens":16384,"recovery_bytes":1048576},
        "memory_indexes":{"notes":{"source":base.join("memory.jsonl"),"snapshot":base.join("memory.mapped"),
            "fingerprint":format!("sha256:{}","b".repeat(64)),"dimension":64,"max_rows":4096,"max_mapped_mib":2}},
        "aggregate":{"cpu_millis":1000,"memory_mib":512,"processes":64},"session":{"cpu_millis":100,"memory_mib":32,"processes":8},
        "max_sessions":1,"max_retained_sessions":1,"max_tasks":1,"lifetime_seconds":120,"session_seconds":5,
        "retention_seconds":1,"replay_bytes":1024,"log_bytes":1024});
    let parsed: Policy = serde_json::from_value(policy.clone())?;
    parsed.clone().validate(&state)?;
    fs::write(state.join("policy.json"), serde_json::to_vec(&policy)?)?;
    let forge = Path::new(forge_binary::path());
    let launch_started = Instant::now();
    let service = Service::launch(
        forge,
        &[
            "harness".into(),
            "serve".into(),
            "--state".into(),
            state.as_os_str().into(),
        ],
        parsed.aggregate,
        parsed.lifetime_seconds,
        &BTreeMap::new(),
    )?;
    Ok((owned, state, service, launch_started))
}

async fn diagnostic_reply(
    write: &mut tokio::net::unix::OwnedWriteHalf,
    read: &mut BufReader<tokio::net::unix::OwnedReadHalf>,
    request: &[u8],
    row: &mut Value,
    deadline: Instant,
) -> Result<u128> {
    let duration = mcp_measurement::limit(deadline)?;
    mcp_measurement::start(&mut row["accounting"]);
    let started = Instant::now();
    let result = tokio::time::timeout(duration, async {
        let mut reply = Vec::new();
        write.write_all(request).await?;
        ensure!(
            transport::read_frame(read, &mut reply, 1048576).await?,
            "Unix diagnostic EOF"
        );
        let response: Value = serde_json::from_slice(&reply)?;
        ensure!(response["ok"] == true, "Unix diagnostic failed: {response}");
        Ok::<_, anyhow::Error>(())
    })
    .await
    .context("Unix diagnostic deadline exceeded")
    .and_then(|result| result);
    mcp_measurement::record(&mut row["accounting"], &result);
    result?;
    Ok(started.elapsed().as_nanos())
}

async fn measure_call(state: &Path, call: Value, row: &mut Value, deadline: Instant) -> Result<()> {
    let stream = tokio::time::timeout(
        mcp_measurement::limit(deadline)?,
        UnixStream::connect(state.join("socket")),
    )
    .await??;
    let (read, mut write) = stream.into_split();
    let mut read = BufReader::new(read);
    let mut request = serde_json::to_vec(&json!({"method":"call","call":call}))?;
    request.push(b'\n');
    row["first_call_ns"] =
        json!(diagnostic_reply(&mut write, &mut read, &request, row, deadline).await?);
    row["operations_per_batch"] = json!(mcp_measurement::OPERATIONS);
    row["warmups"] = json!(mcp_measurement::OPERATIONS);
    row["sample_batch_means_ns"] = json!([]);
    let mut samples = Vec::new();
    for batch in 0..=mcp_measurement::BATCHES {
        let started = Instant::now();
        for _ in 0..mcp_measurement::OPERATIONS {
            diagnostic_reply(&mut write, &mut read, &request, row, deadline).await?;
        }
        if batch > 0 {
            let mean = started.elapsed().as_nanos() / mcp_measurement::OPERATIONS as u128;
            samples.push(mean);
            row["sample_batch_means_ns"]
                .as_array_mut()
                .unwrap()
                .push(json!(mean));
        }
    }
    samples.sort_unstable();
    row["median_batch_mean_ns"] = json!(samples[15]);
    row["p95_batch_mean_ns"] = json!(samples[29]);
    Ok(())
}

async fn ready(state: &Path) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(8), async {
        while !state.join("ready.json").exists() {
            ensure!(
                !state.join("failure.json").exists(),
                "owned service failed to start"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        Ok::<_, anyhow::Error>(())
    })
    .await??;
    Ok(())
}

async fn unix_diagnostic(state: &Path, rows: &mut Value, deadline: Instant) -> Result<()> {
    let query: Value = serde_json::from_slice(&fs::read(
        state.parent().context("state parent")?.join("query.json"),
    )?)?;
    for (index,call) in [
        json!({"name":"code_fold","arguments":{"path":"main.rs"}}),
        json!({"name":"session_brief","arguments":{"task":"native tools"}}),
        json!({"name":"workspace_recall","arguments":{"index":"notes","task":"native tools","query":query,"limit":3}}),
    ].into_iter().enumerate() {measure_call(state,call,&mut rows[index],deadline).await?;}
    Ok(())
}

fn execute(measurement: &mut mcp_measurement::Measurement) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let setup = Instant::now();
    let base = std::env::temp_dir().join(format!("forge-hbench-{}", std::process::id()));
    measurement.report["fixture_path"] = json!(base);
    let (mut owned, state, mut service, launch_started) = prepare(base)?;
    measurement.report["fixture_prepare_ns"] =
        json!(launch_started.duration_since(setup).as_nanos());
    let readiness = runtime.block_on(ready(&state));
    measurement.report["service_launch_ready_ns"] = json!(launch_started.elapsed().as_nanos());
    measurement.report["service_startup_samples"] = json!(1);
    let binary = Path::new(forge_binary::path());
    let before = mcp_measurement::input_identity(&state, binary);
    measurement.report["inputs_before"] = before
        .as_ref()
        .map_or_else(|error| json!({"error":format!("{error:#}")}), Clone::clone);
    let deadline = measurement.deadline;
    let measured = match (&readiness, &before) {
        (Err(error), _) => Err(anyhow::anyhow!("service readiness failed: {error:#}")),
        (_, Err(error)) => Err(anyhow::anyhow!(
            "initial identity capture failed: {error:#}"
        )),
        (Ok(_), Ok(_)) => runtime.block_on(async {
            tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), async {
                measurement.run(&state, binary).await?;
                unix_diagnostic(&state, &mut measurement.report["unix_diagnostic"], deadline).await
            })
            .await
            .context("cooperative measurement deadline exceeded")?
        }),
    };
    // Always perform both cleanup paths, even after a failed/dropped measurement.
    let adapter = runtime.block_on(measurement.finish_adapter());
    let stopped = service.stop();
    measurement.report["cleanup"]["service"] = match &stopped {
        Ok(()) => json!("verified_stopped"),
        Err(error) => json!({"failed":format!("{error:#}")}),
    };
    let after = mcp_measurement::input_identity(&state, binary);
    measurement.report["inputs_after"] = after
        .as_ref()
        .map_or_else(|error| json!({"error":format!("{error:#}")}), Clone::clone);
    let unchanged = match (&before, &after) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    };
    measurement.report["inputs_reverified"] = json!(unchanged);
    let processes_stopped = adapter.is_ok() && stopped.is_ok();
    let removed = if processes_stopped {
        owned.remove()
    } else {
        Err(anyhow::anyhow!(
            "fixture preserved because process cleanup was not verified"
        ))
    };
    measurement.report["cleanup"]["fixture"] = match &removed {
        Ok(()) => json!("verified_removed"),
        Err(error) => json!({"failed":format!("{error:#}"),"path":owned.path,
            "preserved_due_to_unverified_process_cleanup":!processes_stopped}),
    };
    let mut errors = Vec::new();
    for (stage, result) in [
        ("measurement", measured),
        ("adapter cleanup", adapter),
        ("service cleanup", stopped),
        ("fixture cleanup", removed),
    ] {
        if let Err(error) = result {
            errors.push(format!("{stage}: {error:#}"));
        }
    }
    if !unchanged {
        errors.push("source/build/binary/request/policy inputs changed or unavailable".into());
    }
    ensure!(errors.is_empty(), "{}", errors.join("; "));
    Ok(())
}

fn main() -> Result<()> {
    let mut measurement = mcp_measurement::Measurement::new();
    let started = Instant::now();
    let result = execute(&mut measurement);
    let error = result.as_ref().err().map(|error| format!("{error:#}"));
    measurement.finish_report(error.as_deref());
    measurement.report["elapsed_ns"] = json!(started.elapsed().as_nanos());
    // Validity requires EOF/exit, service stop, point-in-time hash recheck and fixture cleanup.
    println!("{}", serde_json::to_string(&measurement.report)?);
    result?;
    ensure!(
        measurement.report["valid"] == true,
        "benchmark accounting is incomplete"
    );
    Ok(())
}
