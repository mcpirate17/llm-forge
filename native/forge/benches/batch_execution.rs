//! Current-only batch scheduling overhead, with no process or sandbox launch.
use anyhow::{ensure, Result};
use forge::batch_execution;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    hint::black_box,
    io::Read,
    time::{Duration, Instant},
};

const JOBS: usize = 64;
const BATCHES: usize = 31;
const ITERATIONS: usize = 50;
const TIMEOUT: Duration = Duration::from_secs(30);
fn executable_sha() -> Result<String> {
    let mut file = std::fs::File::open(std::env::current_exe()?)?;
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
async fn batch(parallelism: usize) -> Result<Vec<Result<usize>>> {
    let jobs = (0..JOBS).collect();
    batch_execution::start(
        jobs,
        parallelism,
        |job, _, _| async move { Ok(black_box(job)) },
    )?
    .wait()
    .await
}
async fn verify(parallelism: usize) -> Result<()> {
    let results = batch(parallelism).await?;
    ensure!(results.len() == JOBS, "benchmark lost jobs");
    for (expected, actual) in results.into_iter().enumerate() {
        ensure!(actual? == expected, "benchmark result order changed");
    }
    Ok(())
}
async fn measure(parallelism: usize) -> Result<Value> {
    verify(parallelism).await?;
    for _ in 0..10 {
        let _ = black_box(batch(parallelism).await?);
    }
    let mut samples = Vec::with_capacity(BATCHES);
    for _ in 0..BATCHES {
        let start = Instant::now();
        for _ in 0..ITERATIONS {
            let _ = black_box(batch(parallelism).await?);
        }
        samples.push(start.elapsed().as_nanos() as f64 / ITERATIONS as f64);
    }
    samples.sort_by(f64::total_cmp);
    Ok(
        json!({"workload":"noop_batch_start_wait", "jobs_per_batch":JOBS,
        "parallelism":parallelism,"samples":BATCHES,"warmup_batches":10,
        "iterations_per_sample":ITERATIONS,"batch_mean_median_ns":samples[15],
        "batch_mean_p95_ns":samples[29],"job_mean_median_ns":samples[15]/JOBS as f64,
        "job_mean_p95_ns":samples[29]/JOBS as f64}),
    )
}
async fn run() -> Result<Value> {
    let start = Instant::now();
    let results = tokio::time::timeout(TIMEOUT, async {
        let mut results = Vec::new();
        for parallelism in [1, 2, 8] {
            results.push(measure(parallelism).await?);
        }
        Ok::<_, anyhow::Error>(results)
    })
    .await??;
    Ok(json!({"schema_version":1,"binary_sha256":executable_sha()?,
        "profile":if cfg!(debug_assertions) {"debug"} else {"release"},
        "isolated_host":false,"comparison":"current_only",
        "scope":"in-process no-op batch scheduling; excludes process and sandbox execution",
        "deadline_seconds":TIMEOUT.as_secs(),"elapsed_ms":start.elapsed().as_millis(),"results":results}))
}
fn main() -> Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    println!("{}", runtime.block_on(run())?);
    Ok(())
}
