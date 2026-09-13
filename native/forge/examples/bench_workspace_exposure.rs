//! Throwaway timing harness for the SessionStart EXPOSED-line exit criterion:
//! < 20 ms per computation, native Rust vs. the Python
//! `conductor.workspace_hygiene.exposure_line` it replaces. `cargo run
//! --release --example bench_workspace_exposure -- [repo...]` prints
//! median/mean/min/max over 20 runs per repo (default: this checkout and a
//! small fixture repo if given); the Python side of the before/after
//! comparison is measured separately by the PR body's recorded runs -- this
//! file only owns the "after" half, like `bench_tool_quiet.rs`.
//!
//! This crate has no lib target, so `workspace_hygiene.rs` is pulled in via
//! `#[path]`, the same way every test in this crate includes its module.

#[path = "../src/workspace_hygiene.rs"]
mod workspace_hygiene;

use std::time::Instant;

const RUNS: usize = 20;

fn mean_ms(samples: &[f64]) -> f64 {
    samples.iter().sum::<f64>() / samples.len() as f64
}

fn run(repo: &std::path::Path) {
    let mut samples = Vec::with_capacity(RUNS);
    for _ in 0..RUNS {
        let start = Instant::now();
        let line = workspace_hygiene::exposure_line(repo);
        samples.push(start.elapsed().as_secs_f64() * 1000.0);
        assert!(line.starts_with("EXPOSED:"), "{line}");
    }
    let mut sorted = samples.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    println!(
        "{}: n={} mean={:.1}ms median={:.1}ms min={:.1}ms max={:.1}ms",
        repo.display(),
        RUNS,
        mean_ms(&samples),
        sorted[RUNS / 2],
        sorted[0],
        sorted[RUNS - 1],
    );
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("usage: bench_workspace_exposure <repo> [<repo>...]");
        std::process::exit(2);
    }
    for repo in &args {
        run(std::path::Path::new(repo));
    }
}
