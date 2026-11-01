//! Immutable version-2 memory snapshots. Explicit compilation converts bounded
//! JSONL vectors to f32 once; mapped queries parse only selected source rows.
//! Checksums detect corruption, not a trusted owner's malicious replacement.
//! Same-UID writers must publish atomically, never chmod/truncate mapped inodes.
#[path = "memory_index_sidecar_build.rs"]
mod build;
#[path = "memory_index_sidecar_format.rs"]
mod format;
use crate::memory_index::{compensated_dot, row_object, validate_row, NativeHit};
use crate::top_k::{Ranked, TopK};
use anyhow::{ensure, Result};
pub use format::Metadata;
use format::{Header, Stamp, HEADER_LEN, MAX_FILE};
use memmap2::Mmap;
#[cfg(feature = "python")]
use pyo3::{exceptions::PyValueError, prelude::*};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::fs::{File, OpenOptions};
use std::io::{BufReader, Read};
use std::os::unix::fs::{FileExt, MetadataExt, OpenOptionsExt};
use std::path::Path;

struct Mapped {
    bytes: Mmap,
    header: Header,
}
impl Mapped {
    fn open(path: &Path) -> Result<Self> {
        Self::open_limited(path, format::MAX_ROWS, MAX_FILE)
    }
    fn open_limited(path: &Path, max_rows: usize, max_bytes: u64) -> Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(path)?;
        let meta = file.metadata()?;
        ensure!(
            meta.is_file()
                && meta.len() >= HEADER_LEN as u64
                && meta.len() <= MAX_FILE.min(max_bytes)
                && meta.uid() == unsafe { libc::geteuid() }
                && meta.nlink() == 1
                && meta.mode() & 0o222 == 0,
            "snapshot must be a bounded read-only owned regular file without hard links"
        );
        // The immutable publication contract excludes in-place changes. The
        // mapping pins this inode across replacement of its directory entry.
        let bytes = unsafe { Mmap::map(&file)? };
        let header = Header::parse(&bytes)?;
        ensure!(
            header.rows <= max_rows,
            "memory snapshot exceeds row capability"
        );
        let mapped = Self { bytes, header };
        mapped.validate()?;
        Ok(mapped)
    }
    fn offset(&self, row: usize) -> u64 {
        let at = self.header.table + row * 8;
        u64::from_le_bytes(
            self.bytes[at..at + 8]
                .try_into()
                .expect("validated offset table"),
        )
    }
    fn vector(&self, row: usize) -> impl Iterator<Item = f64> + '_ {
        let start = HEADER_LEN + row * self.header.dims * 4;
        self.bytes[start..start + self.header.dims * 4]
            .as_chunks::<4>()
            .0
            .iter()
            .map(|cell| f32::from_le_bytes(*cell) as f64)
    }
    fn validate(&self) -> Result<()> {
        let mut previous = None;
        for row in 0..self.header.rows {
            let offset = self.offset(row);
            ensure!(
                offset < self.header.stamp.bytes() && previous.is_none_or(|old| offset > old),
                "invalid source row offsets"
            );
            ensure!(
                self.vector(row).all(f64::is_finite),
                "memory snapshot has non-finite vector"
            );
            previous = Some(offset);
        }
        Ok(())
    }
    fn rank(&self, query: &[f64], count: usize) -> Result<Vec<(f64, u64)>> {
        ensure!(
            query.len() == self.header.dims,
            "dim mismatch in memory snapshot"
        );
        ensure!(
            query.iter().all(|value| value.is_finite()),
            "query vector must be nonempty and finite"
        );
        let mut hits = TopK::new(count).map_err(anyhow::Error::msg)?;
        for row in 0..self.header.rows {
            let score = compensated_dot(query, self.vector(row));
            ensure!(score.is_finite(), "memory index score is non-finite");
            hits.push(Ranked { score, index: row }, ());
        }
        Ok(hits
            .sorted()
            .into_iter()
            .map(|(rank, ())| (rank.score, self.offset(rank.index)))
            .collect())
    }
}

/// A checked map and matching held source inode. Safe to share among concurrent
/// readers: positional source I/O never mutates a shared seek cursor.
pub struct Reader {
    mapped: Mapped,
    source: File,
}
impl Reader {
    pub fn open(source: &Path, snapshot: &Path) -> Result<Self> {
        Self::open_limited(source, snapshot, format::MAX_ROWS, MAX_FILE)
    }
    pub fn open_limited(
        source: &Path,
        snapshot: &Path,
        max_rows: usize,
        max_bytes: u64,
    ) -> Result<Self> {
        ensure!(
            max_rows > 0 && max_bytes >= HEADER_LEN as u64,
            "invalid memory reader limits"
        );
        let source = format::source(source)?;
        let mapped = Mapped::open_limited(snapshot, max_rows, max_bytes)?;
        let reader = Self { mapped, source };
        reader.fresh()?;
        Ok(reader)
    }
    fn fresh(&self) -> Result<()> {
        ensure!(
            Stamp::read(&self.source)? == self.mapped.header.stamp,
            "memory snapshot is stale; explicitly rebuild"
        );
        Ok(())
    }
    pub fn metadata(&self) -> Metadata {
        self.mapped.header.metadata()
    }
    /// Optional full source SHA verification for audit; ordinary queries use
    /// the pinned device/inode/length/mtime/ctime identity, never a prefix hash.
    pub fn verify_source_hash(&self) -> Result<()> {
        self.fresh()?;
        let mut input = AtReader {
            file: &self.source,
            offset: 0,
        }
        .take(self.mapped.header.stamp.bytes() + 1);
        let mut hash = Sha256::new();
        let mut block = [0; 65536];
        loop {
            let count = input.read(&mut block)?;
            if count == 0 {
                break;
            }
            hash.update(&block[..count]);
        }
        ensure!(
            hash.finalize().as_slice() == self.mapped.header.source_sha256,
            "memory source SHA-256 mismatch"
        );
        self.fresh()
    }
    pub fn query(&self, query: &[f64], count: usize, fingerprint: &str) -> Result<Vec<NativeHit>> {
        ensure!(
            (1..=64).contains(&count),
            "memory preview count must be 1..64"
        );
        ensure!(
            fingerprint == self.mapped.header.fingerprint,
            "memory embedding fingerprint mismatch"
        );
        self.fresh()?;
        let ranked = self.mapped.rank(query, count)?;
        let mut hits = Vec::with_capacity(ranked.len());
        for (score, offset) in ranked {
            hits.push(self.hit(score, offset)?);
        }
        self.fresh()?;
        Ok(hits)
    }
    fn hit(&self, score: f64, offset: u64) -> Result<NativeHit> {
        if offset != 0 {
            let mut previous = [0];
            self.source.read_exact_at(&mut previous, offset - 1)?;
            ensure!(previous == *b"\n", "snapshot offset is not a row boundary");
        }
        let mut reader = BufReader::new(AtReader {
            file: &self.source,
            offset,
        });
        let mut line = Vec::new();
        crate::memory_index::read_line(&mut reader, &mut line).map_err(anyhow::Error::msg)?;
        let value: Value = serde_json::from_slice(&line)?;
        let row = row_object(&value).map_err(anyhow::Error::msg)?;
        let metadata = validate_row(row).map_err(anyhow::Error::msg)?;
        ensure!(
            metadata.fingerprint == self.mapped.header.fingerprint
                && metadata.dimension == self.mapped.header.dims,
            "selected row embedding space mismatch"
        );
        crate::memory_index::preview(row, score).map_err(anyhow::Error::msg)
    }
}
struct AtReader<'a> {
    file: &'a File,
    offset: u64,
}
impl Read for AtReader<'_> {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        let count = self.file.read_at(bytes, self.offset)?;
        self.offset += count as u64;
        Ok(count)
    }
}

pub fn build_sidecar(jsonl: &Path, sidecar: &Path) -> Result<usize, String> {
    build::build(jsonl, sidecar).map_err(|error| format!("{error:#}"))
}
pub fn query_sidecar(
    sidecar: &Path,
    query: &[f64],
    candidates: usize,
) -> Result<Vec<(f64, u64)>, String> {
    Mapped::open(sidecar)
        .and_then(|mapped| mapped.rank(query, candidates))
        .map_err(|error| format!("{error:#}"))
}
pub fn header(sidecar: &Path) -> Result<(String, usize, usize), String> {
    Mapped::open(sidecar)
        .map(|mapped| {
            (
                mapped.header.fingerprint,
                mapped.header.dims,
                mapped.header.rows,
            )
        })
        .map_err(|error| format!("{error:#}"))
}
pub fn sidecar_is_fresh(jsonl: &Path, sidecar: &Path) -> Result<bool, String> {
    let source = format::source(jsonl).map_err(|error| format!("{error:#}"))?;
    // This compatibility predicate answers whether an explicit rebuild is
    // required. Reader::open is the diagnostic, fail-loud native query path.
    match Mapped::open(sidecar) {
        Ok(mapped) => {
            Ok(Stamp::read(&source).map_err(|error| error.to_string())? == mapped.header.stamp)
        }
        Err(_) => Ok(false),
    }
}

#[cfg(feature = "python")]
#[pyfunction]
fn memory_index_build_sidecar_native(jsonl_path: &str, sidecar_path: &str) -> PyResult<usize> {
    build_sidecar(Path::new(jsonl_path), Path::new(sidecar_path)).map_err(PyValueError::new_err)
}
#[cfg(feature = "python")]
#[pyfunction]
fn memory_index_sidecar_is_fresh_native(jsonl_path: &str, sidecar_path: &str) -> PyResult<bool> {
    sidecar_is_fresh(Path::new(jsonl_path), Path::new(sidecar_path)).map_err(PyValueError::new_err)
}
#[cfg(feature = "python")]
#[pyfunction]
fn memory_index_sidecar_header_native(path: &str) -> PyResult<(String, usize, usize)> {
    header(Path::new(path)).map_err(PyValueError::new_err)
}
#[cfg(feature = "python")]
#[pyfunction]
fn memory_index_query_snapshot_native(
    source: &str,
    snapshot: &str,
    query: Vec<f64>,
    count: usize,
    fingerprint: &str,
) -> PyResult<Vec<NativeHit>> {
    Reader::open(Path::new(source), Path::new(snapshot))
        .and_then(|reader| reader.query(&query, count, fingerprint))
        .map_err(|error| PyValueError::new_err(format!("{error:#}")))
}
#[cfg(feature = "python")]
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
    module.add_function(wrap_pyfunction!(
        memory_index_query_snapshot_native,
        module
    )?)?;
    Ok(())
}

#[cfg(test)]
#[path = "memory_index_sidecar_tests.rs"]
mod tests;
