//! Binary embedding sidecar for the memory index JSONL.
//!
//! `python -m conductor.memory_index query` used to parse all of
//! `research/cache/memory_index.jsonl` (406 MB of embeddings written as JSON
//! float text) on every query -- measured 4-6 s per call, and every agent
//! queries at session start. The vectors never change between queries, only
//! the ranking does, so this module writes them once into a flat binary
//! sidecar and answers queries from an mmap of it: dot products over f32 rows
//! (promoted to f64 and run through the exact compensated loop the JSON
//! scorer uses), then the JSONL is seeked only for the winning rows' payloads.
//!
//! Layout (all integers little-endian), `docs/ledger.md` "Index layout":
//!
//! ```text
//! 0     magic "FORGEIDX"            8 bytes
//! 8     sidecar format version      u32   (1)
//! 12    embedding dimension         u32
//! 16    row count                   u64
//! 24    source JSONL length         u64
//! 32    source JSONL mtime_ns       u64
//! 40    source prefix sha256       32 bytes (first 64 KB)
//! 72    embedding fingerprint     128 bytes (UTF-8, NUL padded)
//! 200   vectors                     rows x dims f32, row-major
//! 200+  row offsets                 rows x u64, byte offset of each
//!                                     row's first byte in the JSONL
//! ```
//!
//! Freshness is len + mtime_ns + prefix hash, so any rewrite of the JSONL
//! (the writer replaces it atomically) makes the next query rebuild the
//! sidecar. The sidecar itself is written to a temp file and renamed, so a
//! concurrent reader either sees the old sidecar or the new one, never a torn
//! matrix; a crashed build leaves at most a stale sidecar, which the freshness
//! check turns into a rebuild.

use crate::memory_index::{compensated_dot, row_object, validate_row};
use memmap2::Mmap;
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::fs::{File, Metadata};
use std::io::{BufRead, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

const MAGIC: [u8; 8] = *b"FORGEIDX";
const VERSION: u32 = 1;
const PREFIX_HASH_BYTES: usize = 64 * 1024;
const FINGERPRINT_BYTES: usize = 128;
const HEADER_LEN: usize = 8 + 4 + 4 + 8 + 8 + 8 + 32 + FINGERPRINT_BYTES;

struct Header {
    version: u32,
    dims: usize,
    rows: usize,
    source_len: u64,
    source_mtime_ns: u64,
    source_prefix_sha256: [u8; 32],
    fingerprint: String,
}

/// One row's vector (f32-quantised) plus the byte offset of its JSON line.
struct Collected {
    matrix: Vec<f32>,
    offsets: Vec<u64>,
    dims: usize,
    rows: usize,
    fingerprint: String,
}

// ── build ──────────────────────────────────────────────────────────────────

fn build_sidecar(jsonl: &Path, sidecar: &Path) -> Result<usize, String> {
    let meta = std::fs::metadata(jsonl)
        .map_err(|error| format!("memory index missing: {}: {error}", jsonl.display()))?;
    // Stat before reading: if the JSONL changes mid-build the recorded stats
    // no longer match the file, the next freshness check says stale, and the
    // next query rebuilds. Never the other way around.
    let (source_len, source_mtime_ns) = (meta.len(), mtime_ns(&meta));
    let source_prefix_sha256 = prefix_sha256(jsonl)?;
    let collected = collect_rows(jsonl)?;
    write_sidecar_file(
        sidecar,
        &Header {
            version: VERSION,
            dims: collected.dims,
            rows: collected.rows,
            source_len,
            source_mtime_ns,
            source_prefix_sha256,
            fingerprint: collected.fingerprint.clone(),
        },
        &collected,
    )?;
    Ok(collected.rows)
}

/// Streams the JSONL once, reusing `memory_index`'s row validation so the
/// sidecar's idea of a valid row can never drift from the JSON scorer's, and
/// quantises every vector to f32 (scores are still computed in f64 over the
/// promoted values, so only the storage is narrower, not the math).
fn collect_rows(jsonl: &Path) -> Result<Collected, String> {
    let file = File::open(jsonl)
        .map_err(|error| format!("cannot open memory index {}: {error}", jsonl.display()))?;
    let mut reader = BufReader::new(file);
    let mut collected = Collected {
        matrix: Vec::new(),
        offsets: Vec::new(),
        dims: 0,
        rows: 0,
        fingerprint: String::new(),
    };
    let mut line: Vec<u8> = Vec::new();
    let mut offset: u64 = 0;
    loop {
        line.clear();
        let read = reader
            .read_until(b'\n', &mut line)
            .map_err(|error| format!("cannot read memory index {}: {error}", jsonl.display()))?;
        if read == 0 {
            break;
        }
        let line_start = offset;
        offset += read as u64;
        if line.iter().all(|byte| byte.is_ascii_whitespace()) {
            continue;
        }
        let value = serde_json::from_slice::<Value>(&line)
            .map_err(|error| format!("invalid memory index JSON: {error}"))?;
        let row = row_object(&value)?;
        let metadata = validate_row(row)?;
        if collected.fingerprint.is_empty() {
            collected.fingerprint = metadata.fingerprint.to_owned();
        } else if collected.fingerprint != metadata.fingerprint {
            return Err("memory index contains mixed embedding fingerprints".to_owned());
        }
        if collected.dims == 0 {
            collected.dims = metadata.dimension;
        } else if collected.dims != metadata.dimension {
            return Err(format!(
                "memory index dimension disagrees across rows: {} vs {}",
                collected.dims, metadata.dimension
            ));
        }
        let vector = row["vector"].as_array().expect("validated vector");
        for cell in vector {
            let number = cell
                .as_f64()
                .ok_or_else(|| "memory index vector values must be numbers".to_owned())?;
            collected.matrix.push(number as f32);
        }
        collected.offsets.push(line_start);
        collected.rows += 1;
    }
    if collected.rows == 0 {
        return Err("memory index is empty".to_owned());
    }
    Ok(collected)
}

fn write_sidecar_file(
    sidecar: &Path,
    header: &Header,
    collected: &Collected,
) -> Result<(), String> {
    let tmp = PathBuf::from(format!("{}.tmp-{}", sidecar.display(), std::process::id()));
    let write = || -> Result<(), std::io::Error> {
        let file = File::create(&tmp)?;
        let mut out = BufWriter::new(file);
        out.write_all(&MAGIC)?;
        out.write_all(&header.version.to_le_bytes())?;
        out.write_all(&(header.dims as u32).to_le_bytes())?;
        out.write_all(&(header.rows as u64).to_le_bytes())?;
        out.write_all(&header.source_len.to_le_bytes())?;
        out.write_all(&header.source_mtime_ns.to_le_bytes())?;
        out.write_all(&header.source_prefix_sha256)?;
        let mut fingerprint = header.fingerprint.as_bytes().to_vec();
        fingerprint.resize(FINGERPRINT_BYTES, 0);
        out.write_all(&fingerprint)?;
        for cell in &collected.matrix {
            out.write_all(&cell.to_le_bytes())?;
        }
        for offset in &collected.offsets {
            out.write_all(&offset.to_le_bytes())?;
        }
        out.flush()
    };
    write().map_err(|error| format!("cannot write {}: {error}", tmp.display()))?;
    std::fs::rename(&tmp, sidecar)
        .map_err(|error| format!("cannot install sidecar {}: {error}", sidecar.display()))
}

// ── query ──────────────────────────────────────────────────────────────────

/// The `candidates` highest-scoring rows for `query`, as `(score, byte offset
/// of the row's line in the JSONL)`, ties broken by row order exactly like
/// `memory_index::rank` breaks them by ordinal.
fn query_sidecar(
    sidecar: &Path,
    query: &[f64],
    candidates: usize,
) -> Result<Vec<(f64, u64)>, String> {
    let file = File::open(sidecar).map_err(|error| {
        format!(
            "cannot open memory index sidecar {}: {error}",
            sidecar.display()
        )
    })?;
    let mmap = unsafe { Mmap::map(&file) }.map_err(|error| {
        format!(
            "cannot mmap memory index sidecar {}: {error}",
            sidecar.display()
        )
    })?;
    let header = parse_header(&mmap)?;
    if header.dims != query.len() {
        return Err(format!("dim mismatch in sidecar {}", python_path(sidecar)));
    }
    let matrix_off = HEADER_LEN;
    let offsets_off = HEADER_LEN + header.rows * header.dims * 4;
    let mut hits: Vec<(f64, usize)> = Vec::with_capacity(header.rows);
    for index in 0..header.rows {
        let row =
            &mmap[matrix_off + index * header.dims * 4..matrix_off + (index + 1) * header.dims * 4];
        let score = compensated_dot(
            query,
            row.chunks_exact(4)
                .map(|cell| f32::from_le_bytes([cell[0], cell[1], cell[2], cell[3]]) as f64),
        );
        hits.push((score, index));
    }
    hits.sort_by(|left, right| {
        right
            .0
            .partial_cmp(&left.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.1.cmp(&right.1))
    });
    Ok(hits
        .into_iter()
        .take(candidates)
        .map(|(score, index)| {
            let at = offsets_off + index * 8;
            let bytes = &mmap[at..at + 8];
            (
                score,
                u64::from_le_bytes([
                    bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
                ]),
            )
        })
        .collect())
}

// ── freshness ──────────────────────────────────────────────────────────────

fn sidecar_is_fresh(jsonl: &Path, sidecar: &Path) -> Result<bool, String> {
    let meta = std::fs::metadata(jsonl)
        .map_err(|error| format!("memory index missing: {}: {error}", jsonl.display()))?;
    let mmap = match map_existing(sidecar) {
        Some(mmap) => mmap,
        None => return Ok(false),
    };
    let header = match parse_header(&mmap) {
        Ok(header) => header,
        Err(_) => return Ok(false),
    };
    if header.version != VERSION
        || header.source_len != meta.len()
        || header.source_mtime_ns != mtime_ns(&meta)
    {
        return Ok(false);
    }
    Ok(header.source_prefix_sha256 == prefix_sha256(jsonl)?)
}

/// A missing, unreadable or unparseable sidecar is stale, not an error: the
/// caller's response to stale is rebuilding it, which fixes every one of those.
fn map_existing(sidecar: &Path) -> Option<Mmap> {
    let file = File::open(sidecar).ok()?;
    unsafe { Mmap::map(&file) }.ok()
}

// ── header plumbing ────────────────────────────────────────────────────────

fn parse_header(bytes: &[u8]) -> Result<Header, String> {
    if bytes.len() < HEADER_LEN {
        return Err("memory index sidecar is truncated".to_owned());
    }
    if bytes[..8] != MAGIC {
        return Err("not a memory index sidecar".to_owned());
    }
    let le = |at: usize, width: usize| -> u64 {
        let mut buffer = [0u8; 8];
        buffer[..width].copy_from_slice(&bytes[at..at + width]);
        u64::from_le_bytes(buffer)
    };
    let version = le(8, 4) as u32;
    if version != VERSION {
        return Err(format!(
            "memory index sidecar version {version} unsupported"
        ));
    }
    let dims = le(12, 4) as usize;
    let rows = le(16, 8) as usize;
    if dims == 0 || rows == 0 {
        return Err("memory index sidecar header has zero dims or rows".to_owned());
    }
    // u128: a crafted rows/dims pair must not overflow before the length
    // check below rejects it.
    let expected = HEADER_LEN as u128 + rows as u128 * dims as u128 * 4 + rows as u128 * 8;
    if expected != bytes.len() as u128 {
        return Err("memory index sidecar length disagrees with its header".to_owned());
    }
    let mut source_prefix_sha256 = [0u8; 32];
    source_prefix_sha256.copy_from_slice(&bytes[40..72]);
    let fingerprint_raw = &bytes[72..72 + FINGERPRINT_BYTES];
    let fingerprint_end = fingerprint_raw
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(FINGERPRINT_BYTES);
    let fingerprint = std::str::from_utf8(&fingerprint_raw[..fingerprint_end])
        .map_err(|_| "memory index sidecar fingerprint is not UTF-8".to_owned())?
        .to_owned();
    Ok(Header {
        version,
        dims,
        rows,
        source_len: le(24, 8),
        source_mtime_ns: le(32, 8),
        source_prefix_sha256,
        fingerprint,
    })
}

fn prefix_sha256(jsonl: &Path) -> Result<[u8; 32], String> {
    let mut file = File::open(jsonl)
        .map_err(|error| format!("cannot open memory index {}: {error}", jsonl.display()))?;
    let mut buffer = vec![0u8; PREFIX_HASH_BYTES];
    let mut filled = 0usize;
    while filled < buffer.len() {
        let read = file
            .read(&mut buffer[filled..])
            .map_err(|error| format!("cannot read memory index {}: {error}", jsonl.display()))?;
        if read == 0 {
            break;
        }
        filled += read;
    }
    let mut hasher = Sha256::new();
    hasher.update(&buffer[..filled]);
    Ok(hasher.finalize().into())
}

#[cfg(unix)]
fn mtime_ns(meta: &Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    (meta.mtime() as i128 * 1_000_000_000 + meta.mtime_nsec() as i128).max(0) as u64
}

#[cfg(not(unix))]
fn mtime_ns(_meta: &Metadata) -> u64 {
    0
}

fn python_path(path: &Path) -> String {
    path.display().to_string()
}

// ── PyO3 surface (mirrors memory_index's registration) ─────────────────────

#[pyfunction]
fn memory_index_build_sidecar_native(jsonl_path: &str, sidecar_path: &str) -> PyResult<usize> {
    build_sidecar(Path::new(jsonl_path), Path::new(sidecar_path)).map_err(PyValueError::new_err)
}

#[pyfunction]
fn memory_index_sidecar_is_fresh_native(jsonl_path: &str, sidecar_path: &str) -> PyResult<bool> {
    sidecar_is_fresh(Path::new(jsonl_path), Path::new(sidecar_path)).map_err(PyValueError::new_err)
}

#[pyfunction]
fn memory_index_sidecar_header_native(sidecar_path: &str) -> PyResult<(String, usize, usize)> {
    let mmap = (|| -> Result<Mmap, String> {
        let file = File::open(sidecar_path)
            .map_err(|error| format!("cannot open memory index sidecar {sidecar_path}: {error}"))?;
        unsafe { Mmap::map(&file) }
            .map_err(|error| format!("cannot mmap memory index sidecar {sidecar_path}: {error}"))
    })()
    .map_err(PyValueError::new_err)?;
    let header = parse_header(&mmap).map_err(PyValueError::new_err)?;
    Ok((header.fingerprint, header.dims, header.rows))
}

#[pyfunction]
fn memory_index_query_sidecar_native(
    sidecar_path: &str,
    query: Vec<f64>,
    candidates: usize,
) -> PyResult<Vec<(f64, u64)>> {
    query_sidecar(Path::new(sidecar_path), &query, candidates).map_err(PyValueError::new_err)
}

pub(crate) fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(memory_index_build_sidecar_native, module)?)?;
    module.add_function(wrap_pyfunction!(
        memory_index_sidecar_is_fresh_native,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(
        memory_index_sidecar_header_native,
        module
    )?)?;
    module.add_function(wrap_pyfunction!(memory_index_query_sidecar_native, module)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::{BufRead, Seek};
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
                let score =
                    compensated_dot(query, vector.iter().map(|cell| cell.as_f64().unwrap()));
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
        // and the prefix hash but not len: still stale.
        let mut text = String::new();
        for seed in 100..151 {
            text.push_str(&synthetic_row(seed, 4));
            text.push('\n');
        }
        fs::write(&jsonl, text).unwrap();
        assert!(
            !sidecar_is_fresh(&jsonl, &sidecar).unwrap(),
            "same-length rewrite must be stale via mtime/prefix hash"
        );
        // A corrupted magic byte is stale, not an error.
        build_sidecar(&jsonl, &sidecar).unwrap();
        let mut bytes = fs::read(&sidecar).unwrap();
        bytes[0] = b'X';
        fs::write(&sidecar, bytes).unwrap();
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
        for ((score, offset), (ref_score, index)) in hits.into_iter().zip(reference.into_iter()) {
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
}
