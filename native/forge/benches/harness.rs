//! Opt-in, current-only roundtrip measurements against an owned native service.
#[path = "../tests/support/forge_binary.rs"]
mod forge_binary;
#[path = "support/memory_fixture.rs"]
mod memory_fixture;
use anyhow::{ensure, Context, Result};
use forge::harness::{policy::Policy, transport};
use forge::managed_service::Service;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
const SOURCE: &str = "fn answer() -> u32 { 42 }\n";

struct Directory(PathBuf);
impl Drop for Directory {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_dir_all(&self.0) {
            eprintln!("benchmark fixture cleanup: {error}");
        }
    }
}

fn prepare() -> Result<(Directory, PathBuf, Service)> {
    ensure!(
        std::env::var("FORGE_BENCH_SERVICE").as_deref() == Ok("1"),
        "set FORGE_BENCH_SERVICE=1 to authorize this bounded transient benchmark service"
    );
    let base = std::env::temp_dir().join(format!("forge-hbench-{}", std::process::id()));
    fs::create_dir(&base)?;
    fs::set_permissions(&base, fs::Permissions::from_mode(0o700))?;
    let owned = Directory(base.clone());
    let source = base.join("source");
    let state = base.join("state");
    fs::create_dir(&source)?;
    fs::create_dir(&state)?;
    fs::set_permissions(&state, fs::Permissions::from_mode(0o700))?;
    fs::write(source.join("main.rs"), SOURCE)?;
    memory_fixture::memory_fixture(&base)?;
    conductor_native::memory_index_sidecar::build_sidecar(
        &base.join("memory.jsonl"),
        &base.join("memory.mapped"),
    )
    .map_err(anyhow::Error::msg)?;
    let policy = json!({"version":1,"source_root":source,"write_paths":[],"read_only":[],"programs":{},
        "workspace":{"identity":null,"max_chars":3500},
        "memory_indexes":{"notes":{"source":base.join("memory.jsonl"),"snapshot":base.join("memory.mapped"),
            "fingerprint":format!("sha256:{}","b".repeat(64)),"dimension":64,"max_rows":4096,"max_mapped_mib":2}},
        "aggregate":{"cpu_millis":1000,"memory_mib":512,"processes":64},"session":{"cpu_millis":100,"memory_mib":32,"processes":8},
        "max_sessions":1,"max_retained_sessions":1,"max_tasks":1,"lifetime_seconds":60,"session_seconds":5,
        "retention_seconds":1,"replay_bytes":1024,"log_bytes":1024});
    let parsed: Policy = serde_json::from_value(policy.clone())?;
    parsed.clone().validate(&state)?;
    fs::write(state.join("policy.json"), serde_json::to_vec(&policy)?)?;
    let forge = Path::new(forge_binary::path());
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
    Ok((owned, state, service))
}

async fn measure_call(state: &Path, name: &str, call: Value) -> Result<Value> {
    let stream = UnixStream::connect(state.join("socket")).await?;
    let (read, mut write) = stream.into_split();
    let mut read = BufReader::new(read);
    let mut reply = Vec::new();
    let mut request = serde_json::to_vec(&json!({"method":"call","call":call}))?;
    request.push(b'\n');
    let mut samples = Vec::new();
    let first = Instant::now();
    write.write_all(&request).await?;
    ensure!(
        transport::read_frame(&mut read, &mut reply, 1048576).await?,
        "benchmark socket EOF"
    );
    let first: u128 = first.elapsed().as_nanos();
    ensure!(
        serde_json::from_slice::<Value>(&reply)?["ok"] == true,
        "first benchmark call failed"
    );
    for batch in 0..32 {
        let start = Instant::now();
        for _ in 0..100 {
            write.write_all(&request).await?;
            ensure!(
                transport::read_frame(&mut read, &mut reply, 1048576).await?,
                "benchmark socket EOF"
            );
            let result: Value = serde_json::from_slice(&reply)?;
            ensure!(result["ok"] == true, "benchmark call failed: {result}");
        }
        if batch > 0 {
            samples.push(start.elapsed().as_nanos() / 100);
        }
    }
    samples.sort_unstable();
    Ok(
        json!({"name":name,"first_call_ns":first,"batches":31,"operations_per_batch":100,
        "median_batch_mean_ns":samples[15],"p95_batch_mean_ns":samples[29],"sample_batch_means_ns":samples}),
    )
}

async fn measure(state: &Path) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(8), async {
        while !state.join("ready.json").exists() {
            if state.join("failure.json").exists() {
                anyhow::bail!("{}", fs::read_to_string(state.join("failure.json"))?);
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        Ok::<_, anyhow::Error>(())
    })
    .await??;
    let query: Value = serde_json::from_slice(&fs::read(
        state.parent().context("state parent")?.join("query.json"),
    )?)?;
    let mut measurements = Vec::new();
    for (name, call) in [
        (
            "code_fold",
            json!({"name":"code_fold","arguments":{"path":"main.rs"}}),
        ),
        (
            "session_brief",
            json!({"name":"session_brief","arguments":{"task":"native tools"}}),
        ),
        (
            "mapped workspace recall, 4096 rows x 64 dimensions",
            json!({"name":"workspace_recall","arguments":{"index":"notes","task":"native tools","query":query,"limit":3}}),
        ),
    ] {
        measurements.push(measure_call(state, name, call).await?);
    }
    let ready: Value = serde_json::from_slice(&fs::read(state.join("ready.json"))?)?;
    let status = fs::read_to_string(format!(
        "/proc/{}/status",
        ready["pid"].as_u64().context("missing service PID")?
    ))?;
    let rss = status
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))
        .map(str::trim);
    println!(
        "{}",
        json!({"workload":"validated calls over persistent Unix socket, including client parse",
        "measurement":"current-only; shared host; one-CPU service quota","source_bytes":SOURCE.len(),
        "binary_sha256":format!("{:x}",Sha256::digest(fs::read(Path::new(forge_binary::path()))?)),
        "service_rss":rss,"measurements":measurements})
    );
    Ok(())
}

fn main() -> Result<()> {
    let (_owned, state, mut service) = prepare()?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(measure(&state))?;
    service.stop()?;
    Ok(())
}
