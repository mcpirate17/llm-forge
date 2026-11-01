//! Current-only bounded roster parsing and filesystem-backed scope planning.
use anyhow::{ensure, Result};
use cap_std::fs::Dir;
use conductor_native::candidate_cargo_lint::{plan, Mode, Roster};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs,
    hint::black_box,
    path::PathBuf,
    time::{Duration, Instant},
};

struct Fixture(PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).expect("remove owned benchmark fixture");
    }
}
fn sha(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn measure<T>(name: &str, start: Instant, mut call: impl FnMut() -> T) -> Result<Value> {
    for _ in 0..5 {
        black_box(call());
    }
    let mut times = Vec::with_capacity(31);
    for _ in 0..31 {
        ensure!(
            start.elapsed() < Duration::from_secs(30),
            "cooperative benchmark deadline"
        );
        let began = Instant::now();
        for _ in 0..20 {
            black_box(call());
        }
        times.push(began.elapsed().as_secs_f64() * 1_000_000.0 / 20.0);
    }
    ensure!(
        start.elapsed() < Duration::from_secs(30),
        "cooperative benchmark deadline"
    );
    times.sort_by(f64::total_cmp);
    Ok(
        json!({"name":name,"warmup_calls":5,"samples":31,"iterations_per_sample":20,
        "timed_calls":620,"median_sample_mean_us":times[15],"p95_sample_mean_us":times[29],
        "mean_us":times.iter().sum::<f64>()/31.0}),
    )
}
fn main() -> Result<()> {
    let started = Instant::now();
    let fixture =
        Fixture(std::env::temp_dir().join(
            conductor_native::native_id::new("cargo-lint-bench").map_err(anyhow::Error::msg)?,
        ));
    fs::create_dir(&fixture.0)?;
    let names: Vec<String> = (0..16).map(|n| format!("pkg{n:02}")).collect();
    let roster_bytes = format!(
        "[crates]\ntested={names:?}\nlinted={names:?}\n[globs]\nmanifests=['*/Cargo.toml']\n"
    )
    .into_bytes();
    let mut sources = Vec::new();
    let files: Vec<String> = names
        .iter()
        .map(|name| format!("{name}/src/lib.rs"))
        .collect();
    for name in &names {
        fs::create_dir_all(fixture.0.join(name).join("src"))?;
        let text = format!("[package]\nname='{name}'\nversion='0.1.0'\nedition='2021'\n");
        fs::write(fixture.0.join(name).join("Cargo.toml"), &text)?;
        fs::write(
            fixture.0.join(name).join("src/lib.rs"),
            "pub fn answer() -> u32 { 42 }\n",
        )?;
        sources.push((format!("{name}/Cargo.toml"), text));
    }
    let root = Dir::open_ambient_dir(&fixture.0, cap_std::ambient_authority())?;
    let selected: Vec<&str> = files.iter().map(String::as_str).collect();
    let roster = Roster::parse(&roster_bytes).map_err(anyhow::Error::msg)?;
    let oracle = plan(&root, &roster, &selected, Mode::Fmt).map_err(anyhow::Error::msg)?;
    let expected: Vec<String> = names
        .iter()
        .map(|name| format!("{name}/Cargo.toml"))
        .collect();
    ensure!(
        oracle.manifests == expected && oracle.inventory == expected,
        "manifest oracle"
    );
    ensure!(
        oracle.input_owners.len() == 16 && oracle.skips.is_empty(),
        "selection oracle"
    );
    for owner in &oracle.input_owners {
        ensure!(
            owner.input == format!("{}/src/lib.rs", owner.crate_path),
            "owner oracle"
        );
    }
    let measurements = vec![
        measure("parse_16_crate_roster", started, || {
            Roster::parse(black_box(&roster_bytes)).unwrap()
        })?,
        measure("plan_16_crates_with_filesystem_reads", started, || {
            plan(&root, &roster, black_box(&selected), Mode::Fmt).unwrap()
        })?,
    ];
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "scope":"current-only synthetic; no baseline improvement or end-to-end claim",
            "timing":"API including object destruction; plan includes filesystem enumeration and reads; excludes fixture/oracle/toolchain/cache/Cargo/process/sandbox/CLI costs",
            "roster_sha256":sha(&roster_bytes),"manifests_sha256":sha(&serde_json::to_vec(&sources)?),
            "selected_sha256":sha(&serde_json::to_vec(&files)?),"plan_sha256":sha(&serde_json::to_vec(&oracle)?),
            "build_profile":if cfg!(debug_assertions){"debug"}else{"release-like"},
            "measurements":measurements,"elapsed_seconds":started.elapsed().as_secs_f64(),"deadline_seconds":30
        }))?
    );
    Ok(())
}
