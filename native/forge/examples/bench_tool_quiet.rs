//! Throwaway timing harness for the PostToolUse output-bounding exit
//! criterion: <5 ms per call for a 1 MB Bash/Read response, native Rust vs.
//! the Python hook body it replaces. `cargo run --release --example
//! bench_tool_quiet` prints median/mean over 20 runs for both shapes; the
//! Python side of the before/after comparison is measured separately, in
//! process, by `research/tmp/bench_tool_quiet.py` (not part of the crate --
//! this file only owns the "after" half).
//!
//! This crate has no lib target, so `tool_quiet.rs` is pulled in via
//! `#[path]`, the same way every test in this crate includes its module
//! under test.

#[path = "../src/tool_quiet.rs"]
mod tool_quiet;

use serde_json::json;
use std::path::PathBuf;
use std::time::Instant;
use tool_quiet::QuietConfig;

const RUNS: usize = 20;

fn percentile_ms(mut samples: Vec<f64>, p: usize) -> f64 {
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
    samples[samples.len() * p / 100]
}

fn mean_ms(samples: &[f64]) -> f64 {
    samples.iter().sum::<f64>() / samples.len() as f64
}

fn bash_case() -> serde_json::Value {
    // 1 MB of stdout, over the 8 KB default cap.
    json!({"tool_name": "Bash", "tool_response": {"stdout": "x".repeat(1_000_000)}})
}

fn read_case() -> serde_json::Value {
    // 1 MB of file content, over the 16 KB default cap.
    json!({
        "tool_name": "Read",
        "tool_response": {"type": "text", "file": {"filePath": "/big", "content": "x".repeat(1_000_000)}}
    })
}

fn run(label: &str, payload: &serde_json::Value, save_dir: &std::path::Path) {
    let cfg = QuietConfig {
        save_dir,
        repo_root: save_dir,
        now_stamp: "20260101T000000",
        output_field: "updatedToolOutput",
    };
    let mut samples = Vec::with_capacity(RUNS);
    for _ in 0..RUNS {
        let started = Instant::now();
        let out = if label.starts_with("bash") {
            tool_quiet::rewrite_envelope_bash(payload, tool_quiet::BASH_QUIET_LIMIT_DEFAULT, &cfg)
        } else {
            tool_quiet::rewrite_envelope_tool(
                payload,
                tool_quiet::TOOL_OUTPUT_QUIET_DEFAULT,
                false,
                &cfg,
            )
            .0
        };
        let elapsed = started.elapsed().as_secs_f64() * 1000.0;
        samples.push(elapsed);
        std::hint::black_box(out);
    }
    println!(
        "{label}: median={:.3}ms mean={:.3}ms max={:.3}ms",
        percentile_ms(samples.clone(), 50),
        mean_ms(&samples),
        samples.iter().cloned().fold(0.0, f64::max)
    );
}

fn main() {
    let scratch = PathBuf::from(std::env::temp_dir())
        .join(format!("forge-tool-quiet-bench-{}", std::process::id()));
    std::fs::create_dir_all(&scratch).unwrap();
    run("bash_1mb_stdout (after, native)", &bash_case(), &scratch);
    run("read_1mb_content (after, native)", &read_case(), &scratch);
    let _ = std::fs::remove_dir_all(&scratch);
}
