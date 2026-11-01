//! Current-only warm-cache timings: native source snapshots and verified graph rows.
//! Excludes setup, SQLite open, JSON serialization and process startup. No baseline
//! speedup or model-latency claim; filesystem I/O has no independent deadline.
use anyhow::{ensure, Result};
use conductor_native::graph_context::{files::SourceRoot, store::GraphStore};
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{fs, hint::black_box, path::PathBuf, time::Instant};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Result<Self> {
        let root = std::env::temp_dir().join(forge::resource_id("forge-graph-bench")?);
        fs::create_dir(&root)?;
        Ok(Self(root))
    }
    fn graph(&self) -> Result<GraphStore> {
        fs::create_dir(self.0.join(".forge"))?;
        let connection = Connection::open(self.0.join(".forge/graph.db"))?;
        connection.execute_batch(
            "CREATE TABLE nodes(qualified_name TEXT, name TEXT, file_path TEXT, file_hash TEXT);
            CREATE TABLE edges(source_qualified TEXT, target_qualified TEXT, kind TEXT);
            CREATE INDEX nodes_file ON nodes(file_path);
            CREATE INDEX edges_target ON edges(target_qualified);
            BEGIN",
        )?;
        let bytes = vec![b'x'; 2048];
        let digest = format!("{:x}", Sha256::digest(&bytes));
        for index in 0..13 {
            let name = format!("node{index}");
            let path = format!("node{index}.rs");
            fs::write(self.0.join(&path), &bytes)?;
            connection.execute(
                "INSERT INTO nodes VALUES (?1,?1,?2,?3)",
                params![name, path, digest],
            )?;
            if index > 0 {
                connection.execute("INSERT INTO edges VALUES (?1,'node0','CALLS')", [&name])?;
            }
        }
        connection.execute_batch("COMMIT")?;
        drop(connection);
        GraphStore::open(&self.0, None)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).expect("remove owned graph benchmark fixture");
    }
}

fn measure(
    name: &str,
    source_bytes: usize,
    mut operation: impl FnMut() -> Result<()>,
) -> Result<Value> {
    for _ in 0..10 {
        operation()?;
    }
    let mut samples = Vec::with_capacity(31);
    for _ in 0..31 {
        let start = Instant::now();
        for _ in 0..20 {
            operation()?;
        }
        samples.push(start.elapsed().as_secs_f64() * 1e6 / 20.0);
    }
    let batch_means = samples.clone();
    samples.sort_by(f64::total_cmp);
    Ok(
        json!({"workload":name,"source_bytes":source_bytes,"warmup_operations":10,
        "batches":31,"operations_per_batch":20,"median_us":samples[15],
        "p95_us":samples[29],"batch_means_us":batch_means}),
    )
}

fn main() -> Result<()> {
    let fixture = Fixture::new()?;
    let store = fixture.graph()?;
    let sources = SourceRoot::open(&fixture.0)?;
    let payload = vec![b'x'; 4096];
    fs::write(fixture.0.join("text.rs"), &payload)?;
    let hash = measure("confined_hash_4k", 4096, || {
        black_box(sources.hash("text.rs")?);
        Ok(())
    })?;
    let text = measure("confined_text_and_hash_4k", 4096, || {
        black_box(sources.read_text("text.rs", 4096)?);
        Ok(())
    })?;
    let projection = measure("verified_projection_13_files_12_edges", 13 * 2048, || {
        let _operation = store.operation()?;
        let mut result = store.project(&fixture.0, "node0.rs", None, 50)?;
        store.validate_projection(&fixture.0, "node0.rs", &mut result)?;
        ensure!(
            result.graph_status == "ok" && result.callers.len() == 12,
            "invalid benchmark projection"
        );
        black_box(result);
        Ok(())
    })?;
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "measurement":"current-only; warm filesystem cache; unisolated host",
            "forge_version":env!("CARGO_PKG_VERSION"),"toolchain":"Rust 1.98.0",
            "file_recipe":"ASCII x repeated; 4 KiB text and thirteen 2 KiB graph sources",
            "text_sha256":format!("{:x}",Sha256::digest(&payload)),
            "graph_source_sha256":format!("{:x}",Sha256::digest(vec![b'x';2048])),
            "results":[hash,text,projection]
        }))?
    );
    Ok(())
}
