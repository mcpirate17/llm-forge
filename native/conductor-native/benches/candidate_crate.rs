//! Current-only native version evaluation; no Git, filesystem or process timing.
use anyhow::{ensure, Result};
use conductor_native::candidate_crate_version::evaluate;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{hint::black_box, time::Instant};

fn fixture(crates: usize, tests_only: bool) -> Value {
    let mut manifests = Vec::new();
    let mut sources = Vec::new();
    for i in 0..crates {
        let manifest = format!("[package]\nname='crate_{i}'\nversion='1.0.0'\n");
        manifests.push(json!({"path":format!("native/crate_{i}/Cargo.toml"),"current":manifest,"base":manifest}));
        let base = "pub fn run() -> u32 { 1 }\n#[cfg(test)] mod tests { fn first() {} }\n";
        let current = if tests_only {
            "pub fn run() -> u32 { 1 }\n#[cfg(test)] mod tests { fn first() {} fn second() {} }\n"
        } else {
            "pub fn run() -> u32 { 2 }\n#[cfg(test)] mod tests { fn first() {} }\n"
        };
        sources.push(
            json!({"path":format!("native/crate_{i}/src/lib.rs"),"current":current,"base":base}),
        );
    }
    json!({"manifests":manifests,"sources":sources,"installed":[]})
}

fn measure(crates: usize, tests_only: bool) -> Result<Value> {
    let input = fixture(crates, tests_only);
    let expected = if tests_only { 0 } else { crates };
    let run = || -> Result<Value> { evaluate(black_box(&input)).map_err(anyhow::Error::msg) };
    let oracle = run()?;
    ensure!(
        oracle["findings"].as_array().unwrap().len() == expected,
        "unexpected benchmark oracle findings"
    );
    ensure!(
        oracle["metrics"]["crates_touched"] == expected,
        "unexpected benchmark oracle scope"
    );
    for _ in 0..5 {
        black_box(run()?);
    }
    let mut samples = Vec::new();
    for _ in 0..31 {
        let start = Instant::now();
        for _ in 0..10 {
            black_box(run()?);
        }
        samples.push(start.elapsed().as_secs_f64() * 1e6 / 10.0);
    }
    samples.sort_by(f64::total_cmp);
    let bytes = serde_json::to_vec(&input)?;
    Ok(
        json!({"crates":crates,"changed_sources":crates,"tests_only":tests_only,
        "findings":expected,"input_bytes":bytes.len(),"input_sha256":format!("{:x}",Sha256::digest(&bytes)),
        "samples":31,"iterations_per_sample":10,"warmup_calls":5,
        "median_sample_mean_us":samples[15],"p95_sample_mean_us":samples[29]}),
    )
}

fn main() -> Result<()> {
    let mut measurements = Vec::new();
    for crates in [1, 128] {
        for tests_only in [false, true] {
            measurements.push(measure(crates, tests_only)?);
        }
    }
    println!(
        "{}",
        json!({"schema_version":1,"scope":"current-only warm native evaluator including AST/TOML and result allocation/drop; excludes input JSON parsing, Git, file reads, hashing output, process and sandbox costs; no baseline or speedup claim","measurements":measurements})
    );
    Ok(())
}
