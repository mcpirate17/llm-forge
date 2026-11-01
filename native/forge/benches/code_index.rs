//! Full local retrieval pipeline: FTS5, vector search, fusion, source reads/hash checks,
//! and JSON serialization. Synthetic vectors; no model inference or semantic quality claim.
use anyhow::{ensure, Result};
use conductor_native::{
    syntax::fingerprint,
    vector_index::{Config, Space},
};
use forge::code_index::{
    build, BuildHeader, EmbeddingRow, QueryRequest, Reader, SearchMode, SearchResult, Store,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{fs, path::PathBuf, time::Instant};

const ROWS: usize = 4096;
const DIM: usize = 64;
const QUERIES: usize = 64;

struct Fixture(PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).expect("remove owned benchmark fixture");
    }
}

fn random(state: &mut u64) -> f32 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    ((*state >> 40) as f32 / (1u32 << 24) as f32) - 0.5
}

fn fixture() -> Result<(Fixture, Vec<EmbeddingRow>, Vec<Vec<f32>>)> {
    let parent = conductor_native::state_paths::state_root()?.join("benchmarks");
    fs::create_dir_all(&parent)?;
    let path = parent.join(format!("code-index-{}", std::process::id()));
    // Create exclusively before arming cleanup.
    fs::create_dir(&path)?;
    let fixture = Fixture(path);
    fs::create_dir(fixture.0.join("source"))?;
    let mut rows = Vec::with_capacity(ROWS);
    let mut vectors = Vec::with_capacity(ROWS);
    let mut seed = 0x95da_380d_bc28_194a;
    for row in 0..ROWS {
        let family = row % 32;
        let source = format!("// family{family} synthetic retrieval fixture\npub fn function_{row}(value: u64) -> u64 {{ value.wrapping_add({row}) }}\n");
        let path = format!("file_{row:04}.rs");
        fs::write(fixture.0.join("source").join(&path), &source)?;
        let mut vector: Vec<_> = (0..DIM).map(|_| random(&mut seed) * 0.2).collect();
        vector[family] += 1.0;
        rows.push(EmbeddingRow {
            path,
            source_sha256: fingerprint(&source),
            start_byte: 0,
            end_byte: source.len(),
            vector: vector.clone(),
        });
        vectors.push(vector);
    }
    let queries = (0..QUERIES)
        .map(|q| {
            let mut v = vectors[(q * 61) % ROWS].clone();
            for item in &mut v {
                *item += random(&mut seed) * 0.03;
            }
            v
        })
        .collect();
    Ok((fixture, rows, queries))
}

fn space() -> Space {
    Space {
        dimension: DIM,
        model_sha256: Sha256::digest(b"synthetic-cluster-fixture-no-embedding-model").into(),
    }
}

fn request(q: usize, vectors: &[Vec<f32>], mode: SearchMode, lexical_only: bool) -> QueryRequest {
    QueryRequest {
        text: format!("family{}", (q * 61) % 32),
        vector: (!lexical_only).then(|| vectors[q].clone()),
        space: (!lexical_only).then(space),
        mode,
        limit: 10,
        candidates: 64,
        ef: 256,
    }
}

fn checked(result: &SearchResult) -> Result<()> {
    ensure!(
        result.hits.len() == 10
            && result.stale_candidates == 0
            && result.unreadable_candidates == 0
            && !result.freshness_budget_exhausted
            && !result.output_truncated,
        "incomplete benchmark query"
    );
    Ok(())
}

fn measure(
    reader: &mut Reader,
    root: &std::path::Path,
    vectors: &[Vec<f32>],
    mode: SearchMode,
    lexical: bool,
) -> Result<serde_json::Value> {
    for q in 0..8 {
        checked(&reader.search(root, request(q, vectors, mode, lexical))?)?;
    }
    let mut samples = Vec::new();
    let mut evaluations = 0;
    let mut bytes_read = 0;
    let mut serialized = 0;
    for q in 0..QUERIES {
        let start = Instant::now();
        let result = reader.search(root, request(q, vectors, mode, lexical))?;
        let output = serde_json::to_vec(&result)?;
        samples.push(start.elapsed().as_secs_f64() * 1e6);
        checked(&result)?;
        evaluations += result.distance_evaluations;
        bytes_read += result.source_bytes_read;
        serialized += output.len();
        std::hint::black_box(output);
    }
    samples.sort_by(f64::total_cmp);
    Ok(
        json!({"mode":if lexical {"bm25"} else {match mode {SearchMode::Exact=>"hybrid_exact", SearchMode::Hnsw=>"hybrid_hnsw"}},
        "completed":QUERIES,"failed":0,"latency_us_p50":samples[QUERIES/2],"latency_us_p95":samples[60],
        "mean_distance_evaluations":evaluations as f64/QUERIES as f64,
        "mean_source_bytes_read":bytes_read as f64/QUERIES as f64,"mean_json_bytes":serialized as f64/QUERIES as f64}),
    )
}

fn main() -> Result<()> {
    let setup = Instant::now();
    let (fixture, rows, vectors) = fixture()?;
    let source = fixture.0.join("source");
    let store = Store::create(&fixture.0.join("index"))?;
    let fixture_ms = setup.elapsed().as_secs_f64() * 1e3;
    let start = Instant::now();
    let manifest = build(
        &store,
        &source,
        BuildHeader {
            version: 1,
            rows: ROWS,
            space: space(),
            config: Config::default(),
        },
        rows.into_iter().map(Ok),
    )?;
    let build_ms = start.elapsed().as_secs_f64() * 1e3;
    let start = Instant::now();
    let mut reader = Reader::open(&store)?;
    let first_open_ms = start.elapsed().as_secs_f64() * 1e3;
    let mut overlap = 0;
    for q in 0..QUERIES {
        let exact = reader.search(&source, request(q, &vectors, SearchMode::Exact, false))?;
        let approximate = reader.search(&source, request(q, &vectors, SearchMode::Hnsw, false))?;
        checked(&exact)?;
        checked(&approximate)?;
        overlap += approximate
            .hits
            .iter()
            .filter(|hit| exact.hits.iter().any(|other| other.row == hit.row))
            .count();
    }
    let measurements = [
        measure(&mut reader, &source, &vectors, SearchMode::Exact, true)?,
        measure(&mut reader, &source, &vectors, SearchMode::Exact, false)?,
        measure(&mut reader, &source, &vectors, SearchMode::Hnsw, false)?,
    ];
    let binary = fs::read(std::env::current_exe()?)?;
    println!(
        "{}",
        json!({"workload":"local_code_retrieval_pipeline","rows":ROWS,"dimension":DIM,"clusters":32,
        "queries":QUERIES,"top_k":10,"candidates":64,"ef":256,"generation":manifest.generation,
        "source_set_sha256":manifest.source_set_sha256.iter().map(|b| format!("{b:02x}")).collect::<String>(),"benchmark_binary_sha256":format!("{:x}",Sha256::digest(binary)),
        "fixture_ms":fixture_ms,"build_ms":build_ms,"first_open_ms":first_open_ms,
        "hybrid_top10_overlap_with_exact":overlap as f64/(QUERIES*10) as f64,
        "measurements":measurements,"scope":"synthetic clustered vectors; warm shared host; includes FTS5, cosine, fusion, source validation and serialization; excludes model inference and transport; no semantic quality claim"})
    );
    drop(reader);
    Ok(())
}
