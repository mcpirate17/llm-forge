use super::*;
use std::fs;
use std::io::{BufRead, Seek, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

struct ScratchDir(PathBuf);
impl ScratchDir {
    fn new(tag: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "forge-sidecar-test-{tag}-{}-{n}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
        ScratchDir(dir)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}
impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Deterministic pseudo-random value in [-1, 1) from an LCG, so the same
/// (seed, step) pair produces the same float in the writer, the sidecar
/// and the brute-force reference.
fn lcg(seed: u64, step: u64) -> f64 {
    let state = seed
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407 + step.wrapping_mul(7))
        .rotate_left(13);
    (state >> 40) as f64 / (1u64 << 24) as f64 * 2.0 - 1.0
}

fn synthetic_row(seed: u64, dims: usize) -> String {
    let vector: Vec<String> = (0..dims)
        .map(|step| format!("{:.9}", lcg(seed, step as u64)))
        .collect();
    let dims_json = dims;
    let seed_json = seed;
    format!(
        r#"{{"schema_version":3,"embedding":{{"fingerprint":"sha256:sidecar-test","dimension":{dims_json},"paid":true}},"source_sha256":"{sha:064x}","source":"notes","path":"row-{seed_json:04}.md","title":"row {seed_json}","text":"chunk {seed_json}","vector":[{vector}]}}"#,
        sha = seed,
        vector = vector.join(",")
    )
}

fn write_index(path: &Path, rows: u64, dims: usize) {
    let mut text = String::new();
    for seed in 0..rows {
        text.push_str(&synthetic_row(seed, dims));
        text.push('\n');
    }
    fs::write(path, text).unwrap();
}

fn brute_force(path: &Path, query: &[f64], top_k: usize) -> Vec<(f64, usize)> {
    let mut hits: Vec<(f64, usize)> = fs::read_to_string(path)
        .unwrap()
        .lines()
        .enumerate()
        .map(|(index, line)| {
            let row: Value = serde_json::from_str(line).unwrap();
            let vector = row["vector"].as_array().unwrap();
            let score = compensated_dot(query, vector.iter().map(|cell| cell.as_f64().unwrap()));
            (score, index)
        })
        .collect();
    hits.sort_by(|left, right| {
        right
            .0
            .partial_cmp(&left.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.1.cmp(&right.1))
    });
    hits.into_iter().take(top_k).collect()
}

fn query_vector(dims: usize) -> Vec<f64> {
    (0..dims).map(|step| lcg(9_001, step as u64)).collect()
}

#[test]
fn build_fresh_stale_cycle_follows_len_mtime_and_hash() {
    let scratch = ScratchDir::new("fresh");
    let jsonl = scratch.path().join("memory.jsonl");
    let sidecar = scratch.path().join("memory.jsonl.sidecar");
    write_index(&jsonl, 50, 4);
    assert!(!sidecar.exists());
    assert!(!sidecar_is_fresh(&jsonl, &sidecar).unwrap());
    assert_eq!(build_sidecar(&jsonl, &sidecar).unwrap(), 50);
    assert!(sidecar_is_fresh(&jsonl, &sidecar).unwrap());
    // An appended row changes len (and mtime): stale, then fresh again
    // after a rebuild.
    let mut handle = fs::OpenOptions::new().append(true).open(&jsonl).unwrap();
    handle.write_all(synthetic_row(50, 4).as_bytes()).unwrap();
    handle.write_all(b"\n").unwrap();
    drop(handle);
    assert!(!sidecar_is_fresh(&jsonl, &sidecar).unwrap());
    assert_eq!(build_sidecar(&jsonl, &sidecar).unwrap(), 51);
    assert!(sidecar_is_fresh(&jsonl, &sidecar).unwrap());
    // A rewrite with different content of identical length changes mtime
    // and ctime but not len: still stale.
    let mut text = String::new();
    for seed in 100..151 {
        text.push_str(&synthetic_row(seed, 4));
        text.push('\n');
    }
    fs::write(&jsonl, text).unwrap();
    assert!(
        !sidecar_is_fresh(&jsonl, &sidecar).unwrap(),
        "same-length rewrite must be stale via the source identity stamp"
    );
    // A corrupted magic byte is stale, not an error.
    build_sidecar(&jsonl, &sidecar).unwrap();
    let mut bytes = fs::read(&sidecar).unwrap();
    bytes[0] = b'X';
    let corrupt = scratch.path().join("corrupt");
    fs::write(&corrupt, bytes).unwrap();
    fs::set_permissions(&corrupt, fs::Permissions::from_mode(0o400)).unwrap();
    fs::rename(corrupt, &sidecar).unwrap();
    assert!(!sidecar_is_fresh(&jsonl, &sidecar).unwrap());
}

#[test]
fn top_k_matches_a_brute_force_f64_reference_within_1e_5() {
    let scratch = ScratchDir::new("topk");
    let jsonl = scratch.path().join("memory.jsonl");
    let sidecar = scratch.path().join("memory.jsonl.sidecar");
    write_index(&jsonl, 1_000, 8);
    build_sidecar(&jsonl, &sidecar).unwrap();
    let query = query_vector(8);
    let hits = query_sidecar(&sidecar, &query, 10).unwrap();
    let reference = brute_force(&jsonl, &query, 10);
    assert_eq!(hits.len(), reference.len());
    for ((score, _offset), (ref_score, _index)) in hits.iter().zip(reference.iter()) {
        assert!(
            (score - ref_score).abs() < 1e-5,
            "sidecar {score} vs f64 reference {ref_score}"
        );
    }
    // A dim-mismatched query is a loud error naming the sidecar.
    assert!(query_sidecar(&sidecar, &query[..7], 10)
        .unwrap_err()
        .contains("dim mismatch"));
}

#[test]
fn offsets_seek_to_exactly_the_ranked_rows() {
    let scratch = ScratchDir::new("offsets");
    let jsonl = scratch.path().join("memory.jsonl");
    let sidecar = scratch.path().join("memory.jsonl.sidecar");
    write_index(&jsonl, 1_000, 8);
    build_sidecar(&jsonl, &sidecar).unwrap();
    let query = query_vector(8);
    let hits = query_sidecar(&sidecar, &query, 10).unwrap();
    let reference = brute_force(&jsonl, &query, 10);
    let mut reader = BufReader::new(File::open(&jsonl).unwrap());
    for ((score, offset), (ref_score, index)) in hits.into_iter().zip(reference) {
        assert!((score - ref_score).abs() < 1e-5);
        reader.seek(std::io::SeekFrom::Start(offset)).unwrap();
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let row: Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(
            row["path"].as_str().unwrap(),
            format!("row-{:04}.md", index),
            "the offset must point at the ranked row's own line"
        );
    }
}
