//! Warm native evaluator timings, including bounded parsing and result creation.
use anyhow::{ensure, Result};
use conductor_native::{
    candidate_checks, candidate_duplicate_policy, candidate_import_declaration, candidate_structure,
};
use forge::candidate_config::{self, ConfigInput};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    hint::black_box,
    time::{Duration, Instant},
};

fn measure(
    name: &str,
    input: &Value,
    expected: usize,
    start: Instant,
    mut operation: impl FnMut() -> Result<Value>,
) -> Result<Value> {
    let observed = operation()?;
    ensure!(
        observed["findings"]
            .as_array()
            .is_some_and(|rows| rows.len() == expected),
        "benchmark oracle differs: {name}: {observed}"
    );
    for _ in 0..5 {
        black_box(operation()?);
    }
    let mut samples = Vec::new();
    for _ in 0..31 {
        ensure!(
            start.elapsed() < Duration::from_secs(30),
            "benchmark exceeds 30 seconds"
        );
        let began = Instant::now();
        for _ in 0..10 {
            black_box(operation()?);
        }
        samples.push(began.elapsed().as_secs_f64() * 1e6 / 10.0);
    }
    samples.sort_by(f64::total_cmp);
    let bytes = serde_json::to_vec(input)?;
    Ok(
        json!({"name":name, "findings":expected, "input_bytes":bytes.len(),
        "input_sha256":format!("{:x}", Sha256::digest(&bytes)), "samples":31,
        "iterations_per_sample":10, "warmup_calls":5, "median_sample_mean_us":samples[15],
        "p95_sample_mean_us":samples[29]}),
    )
}

fn function(name: &str, value: usize) -> String {
    format!("def {name}():\n    a = {value}\n    b = 2\n    c = 3\n    d = 4\n    e = 5\n    f = 6\n    g = 7\n    h = 8\n    return a\n")
}
fn duplicate(files: usize) -> Value {
    let mut records: Vec<_> = (0..files)
        .map(|i| {
            json!({"path":format!("file{i}.py"),
        "text":function("candidate", i), "changed_lines":if i==0 {vec![2]} else {vec![]}})
        })
        .collect();
    records.push(json!({"path":"peer.py", "text":function("peer", 0), "changed_lines":[]}));
    json!({"files":records, "changed":["file0.py"]})
}
fn imports(files: usize) -> Value {
    json!({"files":(0..files).map(|i| json!({"path":format!("conductor/file{i}.py"),
        "text":"import requests\n", "manifest":"pyproject.toml"})).collect::<Vec<_>>(),
        "manifests":[{"path":"pyproject.toml", "text":"[project]\nname='benchmark'\ndependencies=[]\n"}],
        "strict_trees":["conductor"], "providers":{"stdlib":["sys"],
        "modules":[{"name":"requests", "distributions":["requests"]}],
        "provenance":{"schema_version":1,"implementation":"cpython","version":"3.12",
            "platform":"linux","sha256":"0".repeat(64)}}})
}
fn structure(files: usize) -> Value {
    let paths: Vec<_> = (0..files).map(|i| format!("file{i}.py")).collect();
    json!({"files":paths.iter().map(|path| json!({"path":path,
        "text":"_CACHE = {}\ndef lookup(key):\n    _CACHE[key] = key\n    return key\n"})).collect::<Vec<_>>(),
        "changed":paths})
}

fn main() -> Result<()> {
    let start = Instant::now();
    let mut measurements = Vec::new();
    for count in [1, 128] {
        let input = json!({"path":"module.py", "text":"value = 1\n".repeat(count),
            "changed_lines":[], "classes":["python"], "hot":false});
        measurements.push(measure(
            &format!("python_ast_{count}_statements"),
            &input,
            0,
            start,
            || {
                candidate_checks::evaluate("python-ast", black_box(&input))
                    .map_err(anyhow::Error::msg)
            },
        )?);
        let input = duplicate(count);
        measurements.push(measure(
            &format!("duplicate_{count}_candidates_plus_peer"),
            &input,
            1,
            start,
            || candidate_duplicate_policy::evaluate(black_box(&input)).map_err(anyhow::Error::msg),
        )?);
        let input = imports(count);
        measurements.push(measure(
            &format!("imports_{count}_files"),
            &input,
            count,
            start,
            || {
                candidate_import_declaration::evaluate(black_box(&input))
                    .map_err(anyhow::Error::msg)
            },
        )?);
        let input = structure(count);
        measurements.push(measure(
            &format!("structure_{count}_files"),
            &input,
            count,
            start,
            || candidate_structure::evaluate(black_box(&input)).map_err(anyhow::Error::msg),
        )?);
        let records: Vec<_> = (0..count)
            .map(|i| ConfigInput {
                path: format!("config{i}.toml"),
                text: "[project]\nname='benchmark'\nversion='1.0.0'\n".into(),
                notebook: false,
            })
            .collect();
        let input = json!(records
            .iter()
            .map(|r| json!({"path":r.path, "text":r.text, "notebook":r.notebook}))
            .collect::<Vec<_>>());
        measurements.push(measure(
            &format!("config_{count}_files"),
            &input,
            0,
            start,
            || candidate_config::evaluate(black_box(&records)),
        )?);
    }
    println!(
        "{}",
        json!({"schema_version":1,
        "scope":"current-only warm native evaluators including parsing, allocation and result drop; excludes input JSON decoding, filesystem/Git capture, process, sandbox and end-to-end dispatch costs; no baseline or speedup claim",
        "measurements":measurements})
    );
    Ok(())
}
