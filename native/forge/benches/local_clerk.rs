//! Current-only native clerk inventory timings. Warm filesystem cache; no model,
//! network, subprocess, Python, or baseline-speedup claim. Includes full reads,
//! SHA-256, capability/path checks, retained snippets and inventory Value creation.
//! Excludes fixture/Settings setup, output JSON serialization and process startup.
use anyhow::{ensure, Result};
use forge::local_clerk::{inventory, Settings, MAX_RETAINED_BYTES, MAX_TEXT_CHARS};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs,
    hint::black_box,
    path::{Path, PathBuf},
    time::Instant,
};

const BATCHES: usize = 31;
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Result<Self> {
        let path = std::env::temp_dir().join(forge::resource_id("forge-clerk-bench")?);
        // Arm cleanup only after exclusive ownership has been established.
        fs::create_dir(&path)?;
        Ok(Self(path))
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).expect("remove owned clerk benchmark fixture");
    }
}
struct Workload {
    name: &'static str,
    paths: Vec<PathBuf>,
    manifest: Vec<Value>,
    bytes: usize,
    warmup: usize,
    iterations: usize,
}
impl Workload {
    fn new(name: &'static str, warmup: usize, iterations: usize) -> Self {
        Self {
            name,
            paths: Vec::new(),
            manifest: Vec::new(),
            bytes: 0,
            warmup,
            iterations,
        }
    }
    fn add(&mut self, root: &Path, filename: &str, bytes: &[u8], recipe: &str) -> Result<()> {
        let path = root.join(filename);
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        use std::io::Write;
        file.write_all(bytes)?;
        file.sync_all()?;
        self.paths.push(path);
        self.bytes += bytes.len();
        self.manifest
            .push(json!({"name":filename,"size_bytes":bytes.len(),
            "sha256":format!("{:x}",Sha256::digest(bytes)),"byte_recipe":recipe}));
        Ok(())
    }
}
fn workloads(root: &Path) -> Result<[Workload; 2]> {
    let mut small = Workload::new("inventory_three_2048_byte_sources", 100, 100);
    for index in 0..3 {
        let mut bytes = format!("# Small fixture {index}\n").into_bytes();
        bytes.resize(2048, b'a' + index);
        small.add(
            root,
            &format!("small-{index}.md"),
            &bytes,
            "UTF-8 '# Small fixture N\\n', padded to 2048 bytes with ASCII 'a'+N",
        )?;
    }
    let mut large = Workload::new("inventory_full_hash_1_mib_bounded_prefix", 10, 10);
    let mut bytes = b"# Large fixture\n".to_vec();
    bytes.resize(1024 * 1024, b'x');
    large.add(
        root,
        "large.md",
        &bytes,
        "UTF-8 '# Large fixture\\n', padded to exactly 1048576 bytes with ASCII 'x'",
    )?;
    Ok([small, large])
}
fn verify(settings: &Settings, workload: &Workload) -> Result<()> {
    let value = inventory(settings, &workload.paths)?;
    let sources = value["sources"]
        .as_array()
        .expect("inventory sources array");
    ensure!(
        sources.len() == workload.manifest.len(),
        "benchmark source-count mismatch"
    );
    for (source, expected) in sources.iter().zip(&workload.manifest) {
        ensure!(
            source["sha256"] == expected["sha256"],
            "benchmark full-source SHA mismatch"
        );
        ensure!(
            source["size_bytes"] == expected["size_bytes"],
            "benchmark source-size mismatch"
        );
    }
    if workload.bytes == 1024 * 1024 {
        ensure!(
            sources[0]["truncated_for_prompt"] == true,
            "large-source prefix was not bounded"
        );
    }
    Ok(())
}
fn measure(settings: &Settings, workload: &Workload) -> Result<()> {
    verify(settings, workload)?;
    for _ in 0..workload.warmup {
        black_box(inventory(black_box(settings), black_box(&workload.paths))?);
    }
    let mut samples = Vec::with_capacity(BATCHES);
    for _ in 0..BATCHES {
        let start = Instant::now();
        for _ in 0..workload.iterations {
            black_box(inventory(black_box(settings), black_box(&workload.paths))?);
        }
        samples.push(start.elapsed().as_nanos() as f64 / workload.iterations as f64);
    }
    samples.sort_by(f64::total_cmp);
    let (p50, p95) = (samples[15], samples[29]);
    ensure!(
        p50 > 0.0 && p95 > 0.0,
        "benchmark timer resolution is insufficient"
    );
    let mib = workload.bytes as f64 / (1024.0 * 1024.0);
    println!(
        "{}",
        json!({"comparison":"current-only","workload":workload.name,
        "cache":"warm filesystem cache","source_fixtures":workload.manifest,
        "source_bytes_per_operation":workload.bytes,"retained_prefix_bytes_per_source":MAX_RETAINED_BYTES,
        "retained_text_character_limit":MAX_TEXT_CHARS,"batches":BATCHES,
        "operations_per_batch":workload.iterations,"warmup_operations":workload.warmup,
        "timed_operations":BATCHES*workload.iterations,
        "p50_batch_mean_ns":p50,"p95_batch_mean_ns":p95,
        "mib_per_second_at_p50_batch_mean":mib*1e9/p50,
        "mib_per_second_at_p95_batch_mean":mib*1e9/p95})
    );
    Ok(())
}
fn main() -> Result<()> {
    let fixture = Fixture::new()?;
    let workloads = workloads(&fixture.0)?;
    let settings = Settings::new(&fixture.0, &[])?;
    for workload in &workloads {
        measure(&settings, workload)?;
    }
    Ok(())
}
