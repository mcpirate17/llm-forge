//! Current-only warm-cache timings: snapshots, verified graph rows and native
//! skeleton/syntactic-context dispatch (syntax evidence is not graph completeness).
//! Pre-opened workloads exclude SQLite open; dispatch workloads include their
//! own parser/store/ignore loading. Excludes setup, output serialization and process startup. No baseline
//! speedup or model-latency claim; filesystem I/O has no independent deadline.
use anyhow::{ensure, Context, Result};
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
    let mut results = vec![hash, text, projection];
    results.extend(skeleton_benchmarks()?);
    results.push(syntactic_benchmark(&fixture)?);
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "measurement":"current-only; warm filesystem cache; unisolated host",
            "forge_version":env!("CARGO_PKG_VERSION"),"toolchain":"Rust 1.98.0",
            "file_recipe":"ASCII x repeated; 4 KiB text and thirteen 2 KiB graph sources",
            "text_sha256":format!("{:x}",Sha256::digest(&payload)),
            "graph_source_sha256":format!("{:x}",Sha256::digest(vec![b'x';2048])),
            "results":results
        }))?
    );
    Ok(())
}

fn skeleton_measure(name: &str, operation: &str, source: String, count: usize) -> Result<Value> {
    let payload = json!({"source":source});
    let result = conductor_native::graph_context::dispatch(operation, &payload)
        .map_err(anyhow::Error::msg)?;
    ensure!(
        result["symbols"]
            .as_array()
            .is_some_and(|rows| rows.len() == count),
        "invalid skeleton definition count"
    );
    let skeleton = result["skeleton"]
        .as_str()
        .context("skeleton benchmark returned no text")?;
    ensure!(
        !skeleton.contains("return x +") && !skeleton.contains("{ x +"),
        "skeleton retained implementation"
    );
    let expected = if operation == "python_skeleton" {
        "source-preserving-v1"
    } else {
        "rust-signatures-v1"
    };
    ensure!(
        result["format"] == expected,
        "skeleton benchmark format mismatch"
    );
    let mut measured = measure(name, source.len(), || {
        black_box(
            conductor_native::graph_context::dispatch(operation, black_box(&payload))
                .map_err(anyhow::Error::msg)?,
        );
        Ok(())
    })?;
    measured["source_sha256"] = json!(format!("{:x}", Sha256::digest(source.as_bytes())));
    measured["physical_lines"] = json!(source.lines().count());
    measured["function_count"] = json!(count);
    measured["format"] = json!(expected);
    measured["timing_scope"]=json!("in-process dispatch, parse, admission, projection, Value allocation and internal serialization budget checks; excludes fixture setup and final output serialization");
    Ok(measured)
}
fn skeleton_benchmarks() -> Result<Vec<Value>> {
    let python = (0..256)
        .map(|n| {
            format!(
                "def f{n}(x: int) -> int:\n    \"\"\"Function {n} 界.\"\"\"\n    return x + {n}\n"
            )
        })
        .collect::<String>();
    let rust = (0..256)
        .map(|n| format!("/// Function {n} 界.\npub fn f{n}(x: usize) -> usize {{ x + {n} }}\n"))
        .collect::<String>();
    let mut one_line = "const PREFIX: &str = \"界🦀\"; ".to_owned();
    for n in 0..2048 {
        one_line.push_str(&format!("pub fn f{n}(x:usize)->usize{{ x + {n} }} "));
    }
    Ok(vec![
        skeleton_measure(
            "python_skeleton_256_functions",
            "python_skeleton",
            python,
            256,
        )?,
        skeleton_measure("rust_skeleton_256_functions", "rust_skeleton", rust, 256)?,
        skeleton_measure(
            "rust_skeleton_2048_functions_one_line_unicode_prefix",
            "rust_skeleton",
            one_line,
            2048,
        )?,
    ])
}
struct ScanFixture {
    root: PathBuf,
    visible_bytes: usize,
    target_bytes: usize,
    ignored_bytes: usize,
    manifest: Vec<Value>,
}
fn scan_fixture(fixture: &Fixture) -> Result<ScanFixture> {
    let root = fixture.0.join("syntactic");
    fs::create_dir(&root)?;
    fs::create_dir(root.join("generated"))?;
    fs::write(root.join(".gitignore"), b"generated/\n")?;
    let (mut visible_bytes, mut target_bytes, mut ignored_bytes) = (0, 0, 0);
    let mut manifest = Vec::new();
    for index in 0..32 {
        let (name, source) = if index == 0 {
            ("target.py".into(), "def target():\n    return 1\n".into())
        } else {
            (
                format!("caller_{index:02}.py"),
                format!(
                    "from target import target\n\ndef caller_{index:02}():\n    return target()\n"
                ),
            )
        };
        fs::write(root.join(&name), source.as_bytes())?;
        visible_bytes += source.len();
        if index == 0 {
            target_bytes = source.len();
        }
        manifest.push(json!({"path":name,"bytes":source.len(),"sha256":format!("{:x}",Sha256::digest(source.as_bytes())),"ignored":false}));
        let ignored_name = format!("generated/ignored_{index:02}.py");
        let ignored = format!("# generated {index}\ntarget()\n");
        fs::write(root.join(&ignored_name), ignored.as_bytes())?;
        ignored_bytes += ignored.len();
        manifest.push(json!({"path":ignored_name,"bytes":ignored.len(),"sha256":format!("{:x}",Sha256::digest(ignored.as_bytes())),"ignored":true}));
    }
    Ok(ScanFixture {
        root,
        visible_bytes,
        target_bytes,
        ignored_bytes,
        manifest,
    })
}
fn syntactic_benchmark(fixture: &Fixture) -> Result<Value> {
    let scan = scan_fixture(fixture)?;
    let payload = json!({"repo":scan.root,"file_path":"target.py","target_symbol":"target"});
    let result = conductor_native::graph_context::dispatch("relationships", &payload)
        .map_err(anyhow::Error::msg)?;
    ensure!(
        result["status"]
            .as_str()
            .is_some_and(|v| v.starts_with("unavailable")),
        "syntactic fixture unexpectedly used an index"
    );
    let callers = result["callers"]
        .as_array()
        .context("syntactic fixture returned no callers")?;
    ensure!(
        callers.len() == 31
            && callers.iter().all(|row| row["kind"] == "calls (syntactic)"
                && row["file_path"]
                    .as_str()
                    .is_some_and(|path| !path.starts_with("generated/") && path != "target.py")),
        "syntactic target/ignore/kind semantics changed"
    );
    let mut measured = measure(
        "relationships_native_syntactic_32_python_files_authored_ignore",
        scan.visible_bytes,
        || {
            black_box(
                conductor_native::graph_context::dispatch("relationships", black_box(&payload))
                    .map_err(anyhow::Error::msg)?,
            );
            Ok(())
        },
    )?;
    let manifest = serde_json::to_vec(&scan.manifest)?;
    measured["fixture_manifest_sha256"] = json!(format!("{:x}", Sha256::digest(manifest)));
    measured["source_fixtures"] = json!(scan.manifest);
    measured["visible_regular_python_files"] = json!(32);
    measured["expected_syntactic_callers"] = json!(31);
    measured["visible_python_fixture_bytes"] = json!(scan.visible_bytes);
    measured["target_bytes_scanned_but_excluded_from_results"] = json!(scan.target_bytes);
    measured["ignored_generated_python_files"] = json!(32);
    measured["ignored_generated_bytes"] = json!(scan.ignored_bytes);
    measured["authored_ignore_sha256"] = json!(format!("{:x}", Sha256::digest(b"generated/\n")));
    measured["scan_policy"]=json!("excludes requested target from results; discovery excludes hidden entries, authored ignored trees and symlinks; syntax evidence only");
    measured["timing_scope"]=json!("relationships dispatch, unavailable-store open, authored ignore loading, native file walk and scan; no rg/subprocess/model");
    ensure!(
        !scan.root.join(".forge").exists(),
        "syntactic benchmark created a graph store"
    );
    Ok(measured)
}
