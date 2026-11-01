//! Matched, deterministic CPU search benchmark. No models or GPUs are launched.
#[path = "../tests/support/vector_fixture.rs"]
mod support;
use anyhow::Result;
use conductor_native::vector_index::{
    score_backend, Builder, Config, Hit, Query, QueryWorkspace, Snapshot,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::fs;
use std::hint::black_box;
use std::time::Instant;
use support::{dataset, space, Fixture};

fn timings(
    index: &Snapshot,
    queries: &[Query],
    scratch: &mut QueryWorkspace,
    ef: Option<usize>,
) -> Result<Vec<u128>> {
    let mut times = Vec::new();
    for round in 0..12 {
        let start = Instant::now();
        for query in queries {
            black_box(match ef {
                Some(ef) => index.search(query, 10, ef, scratch)?,
                None => index.exact(query, 10, scratch)?,
            });
        }
        if round >= 2 {
            times.push(start.elapsed().as_nanos() / queries.len() as u128);
        }
    }
    times.sort_unstable();
    Ok(times)
}

fn evaluate(clusters: usize) -> Result<()> {
    const ROWS: usize = 32768;
    const DIMENSION: usize = 384;
    let fixture = Fixture::new();
    let path = fixture.path("vectors.bin");
    let (matrix, raw) = dataset(ROWS, DIMENSION, clusters, 721716);
    let started = Instant::now();
    let builder = Builder::new(space(DIMENSION), matrix, Config::default())?;
    let build_ms = started.elapsed().as_millis();
    builder.write(&path, [0x17; 32])?;
    drop(builder);
    let started = Instant::now();
    let snapshot = Snapshot::open(&path)?;
    let open_ms = started.elapsed().as_millis();
    let queries: Vec<_> = raw
        .into_iter()
        .map(|raw| Query::new(space(DIMENSION), raw))
        .collect::<Result<_>>()?;
    let mut scratch = QueryWorkspace::new(ROWS, 400)?;
    let truth: Vec<Vec<Hit>> = queries
        .iter()
        .map(|q| snapshot.exact(q, 10, &mut scratch).map(|r| r.hits.to_vec()))
        .collect::<Result<_>>()?;
    let exact_ns = timings(&snapshot, &queries, &mut scratch, None)?;
    let mut results = Vec::new();
    for ef in [64, 160, 400] {
        let mut matched = 0;
        let mut distances = 0;
        for (query, exact) in queries.iter().zip(&truth) {
            let result = snapshot.search(query, 10, ef, &mut scratch)?;
            matched += result
                .hits
                .iter()
                .filter(|h| exact.iter().any(|e| e.row == h.row))
                .count();
            distances += result.distance_evaluations;
        }
        let ns = timings(&snapshot, &queries, &mut scratch, Some(ef))?;
        results.push(
            json!({"ef":ef,"recall_at_10":matched as f64 / (queries.len()*10) as f64,
            "median_batch_mean_ns":ns[ns.len()/2],"p95_batch_mean_ns":ns[ns.len()-1],
            "mean_distance_evaluations":distances as f64 / queries.len() as f64}),
        );
    }
    let status = fs::read_to_string("/proc/self/status")?;
    let memory: Vec<_> = status
        .lines()
        .filter(|line| line.starts_with("VmRSS:") || line.starts_with("VmHWM:"))
        .collect();
    println!(
        "{}",
        json!({"fixture":"seeded unseen normalized uniform or clustered queries; no learned embeddings",
        "seed":721716,"rows":ROWS,"dimension":DIMENSION,"clusters":clusters,"queries":queries.len(),
        "warmup_batches":2,"measured_batches":10,"backend":score_backend(),"config":snapshot.config(),
        "build_ms":build_ms,"verified_open_ms":open_ms,"mapped_bytes":snapshot.mapped_bytes(),
        "query_workspace_bytes":scratch.reserved_bytes(),"memory_includes_build_peak":memory,
        "snapshot_sha256":format!("{:x}", Sha256::digest(fs::read(&path)?)),
        "exact":{"median_batch_mean_ns":exact_ns[exact_ns.len()/2],"p95_batch_mean_ns":exact_ns[exact_ns.len()-1]},
        "hnsw":results})
    );
    Ok(())
}

fn main() -> Result<()> {
    evaluate(0)?;
    evaluate(32)
}
