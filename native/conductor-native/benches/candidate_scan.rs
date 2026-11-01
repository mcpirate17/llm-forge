//! Identical-source native scan benchmark for separate before/after executable runs.
use anyhow::{ensure, Result};
use conductor_native::candidate_checks::evaluate;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    hint::black_box,
    io::Read,
    time::{Duration, Instant},
};
const SAMPLES: usize = 31;
const WARMUP: usize = 5;
const LINES: usize = 4000;
const DEADLINE: Duration = Duration::from_secs(30);
fn within(start: Instant) -> Result<()> {
    ensure!(
        start.elapsed() < DEADLINE,
        "scan benchmark exceeded cooperative 30-second budget"
    );
    Ok(())
}
fn sha(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn executable_sha(start: Instant) -> Result<String> {
    let mut file = std::fs::File::open(std::env::current_exe()?)?;
    ensure!(
        file.metadata()?.len() <= 256 * 1024 * 1024,
        "executable exceeds 256 MiB"
    );
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 65536];
    for _ in 0..4097 {
        within(start)?;
        let n = file.read(&mut buffer)?;
        if n == 0 {
            return Ok(format!("{:x}", hash.finalize()));
        }
        hash.update(&buffer[..n]);
    }
    anyhow::bail!("executable grew beyond 256 MiB")
}
struct Workload {
    name: &'static str,
    operation: &'static str,
    payload: Value,
    source_bytes: usize,
    dense: bool,
}
fn workload(operation: &'static str, dense: bool) -> Workload {
    let line = match (operation, dense) {
        ("secret-scan", true) => "AKIAABCDEFGHIJKLMNOP\n",
        ("native-source", true) => "strcpy(a,b);\n",
        _ => "let sample = 7;\n",
    };
    let source = line.repeat(LINES);
    let source_bytes = source.len();
    let name = match (operation, dense) {
        ("secret-scan", true) => "secret_dense_4000",
        ("secret-scan", false) => "secret_clean_4000",
        ("native-source", true) => "native_dense_4000",
        _ => "native_clean_4000",
    };
    Workload {
        name,
        operation,
        payload: json!({"files":[{"path":"fixture/source.txt","text":source}]}),
        source_bytes,
        dense,
    }
}
fn oracle(workload: &Workload) -> Result<Value> {
    let output = evaluate(workload.operation, &workload.payload).map_err(anyhow::Error::msg)?;
    let findings = output["findings"]
        .as_array()
        .expect("native evaluator findings");
    assert_eq!(findings.len(), if workload.dense { LINES } else { 0 });
    if !workload.dense {
        return Ok(output);
    }
    let (rule, message) = if workload.operation == "secret-scan" {
        (
            "aws-access-key",
            "candidate contains secret-like credential material",
        )
    } else {
        ("unsafe-native-api", "unsafe native API admitted: strcpy(")
    };
    for (index, finding) in findings.iter().enumerate() {
        assert_eq!(finding["check_id"], workload.operation);
        assert_eq!(finding["rule_id"], rule);
        assert_eq!(finding["severity"], "critical");
        assert_eq!(finding["message"], message);
        assert_eq!(finding["path"], "fixture/source.txt");
        assert_eq!(finding["line"], index + 1);
    }
    assert_eq!(findings.last().unwrap()["line"], 4000);
    if workload.operation == "secret-scan" {
        let encoded = serde_json::to_string(&output)?;
        assert!(!encoded.contains("AKIAABCDEFGHIJKLMNOP"));
        assert!(findings
            .iter()
            .all(|finding| finding.get("evidence").is_none() && finding.get("column").is_none()));
    }
    Ok(output)
}
fn measure(workload: &Workload, start: Instant) -> Result<Value> {
    let expected = oracle(workload)?;
    let input = serde_json::to_vec(&workload.payload)?;
    let encoded_output = serde_json::to_vec(&expected)?;
    let iterations = if workload.dense { 1u32 } else { 10u32 };
    for _ in 0..WARMUP {
        within(start)?;
        black_box(evaluate(workload.operation, black_box(&workload.payload)).unwrap());
    }
    let mut samples = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        within(start)?;
        let began = Instant::now();
        for _ in 0..iterations {
            black_box(evaluate(workload.operation, black_box(&workload.payload)).unwrap());
        }
        samples.push(began.elapsed().as_nanos() as f64 / f64::from(iterations));
        within(start)?;
    }
    samples.sort_by(f64::total_cmp);
    Ok(
        json!({"name":workload.name,"operation":workload.operation,"unit":"ns_per_evaluate",
        "samples":SAMPLES,"warmup_calls":WARMUP,"iterations_per_sample":iterations,
        "median_batch_mean":samples[15],"p95_batch_mean":samples[29],
        "files":1,"lines":LINES,"source_bytes":workload.source_bytes,"findings":if workload.dense{LINES}else{0},
        "input_json_bytes":input.len(),"input_sha256":sha(&input),
        "output_json_bytes":encoded_output.len(),"output_sha256":sha(&encoded_output)}),
    )
}
fn main() -> Result<()> {
    let start = Instant::now();
    let binary = executable_sha(start)?;
    let mut results = Vec::with_capacity(4);
    for operation in ["secret-scan", "native-source"] {
        for dense in [false, true] {
            results.push(measure(&workload(operation, dense), start)?);
        }
    }
    within(start)?;
    println!(
        "{}",
        json!({"schema_version":1,"binary_sha256":binary,
        "profile":if cfg!(debug_assertions){"debug"}else{"release"},"isolated_host":false,
        "comparison":"one_executable_per_run_no_speedup_claim","cooperative_deadline_seconds":30,
        "scope":"native evaluate API including output Value allocation/drop; excludes source filesystem, JSON text decode/encode, hashing and fingerprints",
        "results":results})
    );
    Ok(())
}
