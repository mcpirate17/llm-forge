//! Immutable code snapshots: native embeddings, HNSW/exact cosine and SQLite BM25.
mod build;
pub mod cli;
pub mod extract;
mod search;
mod store;
pub use build::{build, BuildHeader, EmbeddingRow};
pub use search::{QueryRequest, ReadLimits, Reader, SearchMode, SearchResult};
pub use store::{Manifest, Store};

use anyhow::{ensure, Result};
use sha2::{Digest, Sha256};
use std::fs::{File, OpenOptions};
use std::io::Read;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;

const MAX_CHUNK_BYTES: usize = 16 * 1024;
const MAX_COMPONENT_BYTES: u64 = 8 * 1024 * 1024 * 1024;

fn validate_hash(hash: &str) -> Result<()> {
    conductor_native::sha256_hex::parse(hash).map_err(anyhow::Error::msg)?;
    Ok(())
}
fn file_hash(path: &Path) -> Result<String> {
    let mut file = immutable_file(path)?;
    let mut hash = Sha256::new();
    let mut block = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut block)?;
        if n == 0 {
            break;
        }
        hash.update(&block[..n]);
    }
    Ok(format!("{:x}", hash.finalize()))
}
fn immutable_file(path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file()
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.nlink() == 1
            && metadata.mode() & 0o222 == 0
            && metadata.len() <= MAX_COMPONENT_BYTES,
        "index component must be an owned, read-only, bounded regular file without hard links"
    );
    Ok(file)
}
