//! Current-only lifecycle CLI timings, including process startup and durable I/O.
#[path = "../tests/support/forge_binary.rs"]
mod forge_binary;
#[path = "support/memory_fixture.rs"]
mod memory_fixture;
use anyhow::{ensure, Result};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::Read;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

struct Fixture {
    root: PathBuf,
    binary: PathBuf,
    _worker: File,
}
impl Fixture {
    fn create() -> Result<Self> {
        let binary = PathBuf::from(forge_binary::path());
        let root = std::env::temp_dir().join(forge::resource_id("forge-lifecycle-bench")?);
        fs::DirBuilder::new().mode(0o700).create(&root)?;
        git2::Repository::init(&root)?;
        let state = root.join(".code-review-graph");
        fs::DirBuilder::new().mode(0o700).create(&state)?;
        let worker = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(state.join("refresh.lock"))?;
        ensure!(
            unsafe { libc::flock(worker.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0,
            "hold fixture worker lock"
        );
        for index in 0..128 {
            fs::write(root.join(format!("module_{index}.rs")),format!("fn source_{index}() -> usize {{ {index} }}\nfn caller_{index}() -> usize {{ source_{index}() }}\n"))?;
        }
        Ok(Self {
            root,
            binary,
            _worker: worker,
        })
    }
    fn call(&self, args: &[&str]) -> Result<Vec<u8>> {
        let mut command = Command::new(&self.binary);
        command
            .current_dir(&self.root)
            .args(args)
            .env("PATH", "")
            .env_remove("CRG_DATA_DIR")
            .env_remove("A2A_AGENT_NAME")
            .env_remove("CLAUDE_PROJECT_DIR")
            .env_remove("CONDUCTOR_NOTES_DB")
            .env_remove("CONDUCTOR_HOST_ROOT");
        let output = forge::process_probe::run(&mut command, Duration::from_secs(10), 65536)?;
        ensure!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(output.stdout)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_dir_all(&self.root) {
            eprintln!("lifecycle benchmark cleanup: {error}");
        }
    }
}

fn hash(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn measure(fixture: &Fixture, name: &str, args: &[&str]) -> Result<Value> {
    let mut samples = Vec::new();
    let mut last = Vec::new();
    for index in 0..34 {
        let start = Instant::now();
        last = fixture.call(args)?;
        if index >= 3 {
            samples.push(start.elapsed().as_nanos());
        }
    }
    samples.sort_unstable();
    Ok(
        json!({"operation":name,"samples":31,"median_ns":samples[15],"p95_ns":samples[29],
        "sample_ns":samples,"last_output_bytes":last.len()}),
    )
}

fn main() -> Result<()> {
    let fixture = Fixture::create()?;
    let stage = [
        "handoff",
        "stage",
        "--identity",
        "bench",
        "--task",
        "Continue native implementation",
        "--context",
        "Preserve the native interfaces and measured behavior.",
    ];
    fixture.call(&stage)?;
    let start = Instant::now();
    let first: Value = serde_json::from_slice(&fixture.call(&["graph", "index"])?)?;
    let first_ns = start.elapsed().as_nanos();
    ensure!(first["files"] == 128, "wrong graph fixture size");
    let mut measurements = Vec::new();
    for (name, args) in [
        ("native CLI version startup", vec!["--version"]),
        (
            "native session brief with policy and absent optional indexes",
            vec!["session", "brief", "--task", "native tools", "--json"],
        ),
        (
            "durable graph queue with worker already holding lock",
            vec!["graph", "refresh"],
        ),
        (
            "handoff stage with parent lineage and fsync",
            stage.to_vec(),
        ),
        (
            "unchanged 128-file graph index with cached syntax facts",
            vec!["graph", "index"],
        ),
    ] {
        measurements.push(measure(&fixture, name, &args)?);
    }
    fixture.call(&["handoff", "hook", "--identity", "bench"])?;
    measurements.push(measure(
        &fixture,
        "already-active handoff hook",
        &["handoff", "hook", "--identity", "bench"],
    )?);
    let memory_bytes = memory_fixture::memory_fixture(&fixture.root)?;
    let compiled = fixture.root.join("memory.jsonl.sidecar");
    let start = Instant::now();
    conductor_native::memory_index_sidecar::build_sidecar(
        &fixture.root.join("memory.jsonl"),
        &compiled,
    )
    .map_err(anyhow::Error::msg)?;
    let memory_build_ns = start.elapsed().as_nanos();
    measurements.push(measure(
        &fixture,
        "native brief with validated mmap memory (4096 rows x 64 dimensions)",
        &[
            "session",
            "brief",
            "--task",
            "native tools",
            "--query",
            "query.json",
            "--memory-index",
            "memory.jsonl",
            "--json",
        ],
    )?);
    println!(
        "{}",
        json!({"measurement":"current-only, shared host, warm process/filesystem repetition; includes child startup and durable I/O",
        "forge_binary":fixture.binary,"binary_sha256":hash(&fixture.binary)?,
        "first_graph_index_ns":first_ns,"first_graph_report":first,"memory_fixture_bytes":memory_bytes,"memory_build_ns":memory_build_ns,"measurements":measurements})
    );
    Ok(())
}
