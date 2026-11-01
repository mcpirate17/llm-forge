//! Current-only native changed-line range cost on a fixed Git-object fixture.
//! Fixture creation, candidate resolution and cleanup are outside timing.
use anyhow::{ensure, Result};
use forge::candidate_source::{
    lines::{LineRange, RangeScope},
    Candidate, Kind, Source,
};
use git2::{Repository, Signature};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    hint::black_box,
    path::{Path, PathBuf},
    time::Instant,
};

const FILES: usize = 16;
const LINES: usize = 100;
const CHANGED_LINE: u32 = 51;
const WARMUPS: usize = 5;
const SAMPLES: usize = 31;
const ITERATIONS: usize = 20;

struct Fixture(PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn file_path(index: usize) -> String {
    format!("src/bench_{index:02}.c")
}

fn source_text(index: usize, changed: bool) -> String {
    (1..=LINES)
        .map(|line| {
            let value = if changed && line == CHANGED_LINE as usize {
                10_000 + index
            } else {
                line
            };
            format!("int bench_{index:02}_line_{line:03} = {value};\n")
        })
        .collect()
}

fn fixture() -> Result<(Fixture, Source, Candidate, Vec<String>, serde_json::Value)> {
    let root = std::env::temp_dir().join(forge::resource_id("forge-lines-bench")?);
    std::fs::create_dir(&root)?;
    let fixture = Fixture(root);
    let repo_root = fixture.0.join("repo");
    let repository = Repository::init(&repo_root)?;
    std::fs::create_dir(repo_root.join("src"))?;

    let paths: Vec<_> = (0..FILES).map(file_path).collect();
    let mut source_hashes = Vec::with_capacity(FILES);
    for (index, path) in paths.iter().enumerate() {
        let original = source_text(index, false);
        let changed = source_text(index, true);
        std::fs::write(repo_root.join(path), &original)?;
        source_hashes.push(json!({
            "path": path,
            "base_sha256": digest(original.as_bytes()),
            "candidate_sha256": digest(changed.as_bytes())
        }));
    }
    let mut index = repository.index()?;
    for path in &paths {
        index.add_path(Path::new(path))?;
    }
    index.write()?;
    let base_tree_oid = index.write_tree()?;
    let base_tree = repository.find_tree(base_tree_oid)?;
    let signature = Signature::now("native-lines-bench", "native-lines-bench@example.invalid")?;
    repository.commit(
        Some("HEAD"),
        &signature,
        &signature,
        "baseline",
        &base_tree,
        &[],
    )?;
    drop(base_tree);

    for (index, path) in paths.iter().enumerate() {
        std::fs::write(repo_root.join(path), source_text(index, true))?;
    }
    let mut index = repository.index()?;
    for path in &paths {
        index.add_path(Path::new(path))?;
    }
    index.write()?;

    let source = Source::open(&repo_root)?;
    let candidate = source.resolve(Kind::Index, "HEAD", None)?;
    let selected: Vec<&str> = paths.iter().map(String::as_str).collect();
    let first_plan = source.changed_ranges(&candidate, &selected)?;
    ensure!(
        first_plan.files.len() == FILES,
        "range plan file count differs"
    );
    for (file, path) in first_plan.files.iter().zip(&paths) {
        ensure!(file.path == *path, "range plan path/order differs");
        ensure!(file.scope == RangeScope::Changed, "expected changed scope");
        ensure!(
            file.ranges.as_slice()
                == [LineRange {
                    start: CHANGED_LINE,
                    end: CHANGED_LINE
                }],
            "range plan differs from single-line fixture"
        );
    }
    let output_bytes = serde_json::to_vec(&first_plan)?;
    let output_sha256 = digest(&output_bytes);
    let workload = json!({
        "files": FILES,
        "lines_per_file": LINES,
        "changed_lines_per_file": 1,
        "changed_line": CHANGED_LINE,
        "selected_paths": &paths,
        "base_tree_oid": &candidate.base_tree_oid,
        "candidate_tree_oid": &candidate.tree_oid,
        "source_hashes": source_hashes,
        "validated_output_sha256": output_sha256,
        "validated_output_bytes": output_bytes.len()
    });
    Ok((fixture, source, candidate, paths, workload))
}

fn measure(source: &Source, candidate: &Candidate, paths: &[String]) -> Result<Vec<f64>> {
    let selected: Vec<&str> = paths.iter().map(String::as_str).collect();
    for _ in 0..WARMUPS {
        black_box(source.changed_ranges(black_box(candidate), black_box(&selected))?);
    }
    let mut sample_means = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        let started = Instant::now();
        for _ in 0..ITERATIONS {
            black_box(source.changed_ranges(black_box(candidate), black_box(&selected))?);
        }
        sample_means.push(started.elapsed().as_nanos() as f64 / ITERATIONS as f64);
    }
    Ok(sample_means)
}

fn main() -> Result<()> {
    let (_fixture, source, candidate, paths, workload) = fixture()?;
    let samples = measure(&source, &candidate, &paths)?;
    let mut ordered = samples.clone();
    ordered.sort_by(f64::total_cmp);
    let report = json!({
        "benchmark": "candidate_source_changed_ranges",
        "measurement": "current_only_no_speedup_claim",
        "unit": "ns_per_operation",
        "warmups": WARMUPS,
        "samples": SAMPLES,
        "iterations_per_sample": ITERATIONS,
        "sample_means_ns": samples,
        "median_sample_mean_ns": ordered[15],
        "p95_sample_mean_ns": ordered[29],
        "timed_region": "Source::changed_ranges plus RangePlan destruction",
        "excluded": ["Git fixture creation", "candidate resolution", "fixture cleanup"],
        "workload": workload
    });
    println!("{}", serde_json::to_string(&report)?);
    Ok(())
}
