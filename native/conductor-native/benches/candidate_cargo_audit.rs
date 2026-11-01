//! Current-only synthetic RustSec API latency; no end-to-end/baseline speedup claim.
use anyhow::{ensure, Result};
use conductor_native::candidate_cargo_engine::Auditor;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    hint::black_box,
    time::{Duration, Instant},
};
const SAMPLES: usize = 31;
const ITERATIONS: usize = 20;
const WARMUP: usize = 5;
const DEADLINE: Duration = Duration::from_secs(30);
fn within(start: Instant) -> Result<()> {
    ensure!(
        start.elapsed() < DEADLINE,
        "benchmark exceeded cooperative 30-second budget"
    );
    Ok(())
}
fn sha(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn dataset() -> Result<(Vec<u8>, String, String)> {
    let registry: Vec<Value> = (0..32)
        .map(|i| {
            json!({"name":"example","version":format!("1.0.{i}"),
        "checksum":format!("{:064x}",i+1),"yanked":i==3})
        })
        .collect();
    let bundle = json!({"schema_version":1,"captured_at":123,"advisory_commit":"a".repeat(40),
        "advisories":[{"path":"crates/example/RUSTSEC-2021-0001.md","text":
            "```toml\n[advisory]\nid = \"RUSTSEC-2021-0001\"\npackage = \"example\"\ndate = \"2021-01-01\"\n[versions]\npatched = [\">= 1.0.16\"]\n```\n\n# Synthetic vulnerability 雪\n\nOwned benchmark fixture.\n"}],"registry":registry});
    let lock = |start| {
        let mut text = String::from("version = 3\n");
        for i in start..32 {
            text.push_str(&format!("\n[[package]]\nname = \"example\"\nversion = \"1.0.{i}\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\nchecksum = \"{:064x}\"\n",i+1));
        }
        text
    };
    Ok((serde_json::to_vec(&bundle)?, lock(0), lock(16)))
}
fn oracle(auditor: &Auditor, full: &str, clean: &str) -> Result<(String, String)> {
    let full = auditor.audit(full).map_err(anyhow::Error::msg)?;
    ensure!(
        full.packages_total == 32 && full.crates_io_packages == 32 && full.other_sources.is_empty(),
        "full package scope"
    );
    ensure!(
        full.matches.len() == 16 && full.yanked.len() == 1,
        "full finding counts"
    );
    let mut versions = std::collections::BTreeSet::new();
    for found in &full.matches {
        versions.insert(found.version.as_str());
        let patch = found
            .version
            .strip_prefix("1.0.")
            .unwrap()
            .parse::<usize>()?;
        ensure!(
            patch < 16 && found.advisory_id == "RUSTSEC-2021-0001" && found.package == "example",
            "advisory match oracle"
        );
    }
    ensure!(versions.len() == 16, "distinct vulnerable versions oracle");
    ensure!(full.yanked[0].version == "1.0.3", "yanked version oracle");
    let clean = auditor.audit(clean).map_err(anyhow::Error::msg)?;
    ensure!(
        clean.packages_total == 16 && clean.matches.is_empty() && clean.yanked.is_empty(),
        "negative oracle"
    );
    Ok((
        sha(&serde_json::to_vec(&full)?),
        sha(&serde_json::to_vec(&clean)?),
    ))
}
fn measure<T>(name: &str, start: Instant, mut call: impl FnMut() -> T) -> Result<Value> {
    for _ in 0..WARMUP {
        within(start)?;
        black_box(call());
    }
    let mut samples = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        within(start)?;
        let began = Instant::now();
        for _ in 0..ITERATIONS {
            black_box(call());
        }
        samples.push(began.elapsed().as_secs_f64() * 1_000_000.0 / ITERATIONS as f64);
        within(start)?;
    }
    samples.sort_by(f64::total_cmp);
    Ok(
        json!({"name":name,"samples":SAMPLES,"iterations_per_sample":ITERATIONS,"warmup_calls":WARMUP,
        "timed_calls":SAMPLES*ITERATIONS,"timed_elapsed_seconds":samples.iter().sum::<f64>()*ITERATIONS as f64/1_000_000.0,
        "mean_us_per_iteration":samples.iter().sum::<f64>()/SAMPLES as f64,
        "median_us_per_iteration":samples[SAMPLES/2],"p95_us_per_iteration":samples[29]}),
    )
}
fn main() -> Result<()> {
    let start = Instant::now();
    let (bundle, full, clean) = dataset()?;
    let auditor = Auditor::parse(&bundle).map_err(anyhow::Error::msg)?;
    ensure!(
        auditor.advisory_count() == 1 && auditor.registry_count() == 32,
        "dataset parse oracle"
    );
    let (positive_sha, negative_sha) = oracle(&auditor, &full, &clean)?;
    let measurements = vec![
        measure("bundle_parse_1_advisory_32_registry", start, || {
            Auditor::parse(black_box(&bundle)).unwrap()
        })?,
        measure(
            "warm_audit_32_versions_16_vulnerable_1_yanked",
            start,
            || auditor.audit(black_box(&full)).unwrap(),
        )?,
        measure("warm_audit_16_patched_versions", start, || {
            auditor.audit(black_box(&clean)).unwrap()
        })?,
    ];
    println!(
        "{}",
        serde_json::to_string_pretty(
            &json!({"scope":"synthetic, no end-to-end or baseline speedup claim",
        "timing":"in-process API calls incl returned object destruction; excludes semantic checks, dataset and output serialization, evidence authentication/freshness, filesystem, network, sandbox and CLI",
        "build_profile":if cfg!(debug_assertions){"debug-assertions-enabled"}else{"release-like"},
        "rustsec_version":rustsec::VERSION,"bundle_bytes":bundle.len(),"bundle_sha256":sha(&bundle),
        "lock_full_bytes":full.len(),"lock_full_sha256":sha(full.as_bytes()),"lock_clean_bytes":clean.len(),
        "lock_clean_sha256":sha(clean.as_bytes()),"positive_observation_sha256":positive_sha,
        "negative_observation_sha256":negative_sha,"measurements":measurements,"elapsed_seconds":start.elapsed().as_secs_f64(),
        "deadline_seconds":DEADLINE.as_secs()})
        )?
    );
    Ok(())
}
