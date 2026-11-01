//! Native cosine HNSW with immutable, checksummed, memory-mapped snapshots.
//!
//! Construction follows Malkov/Yashunin (arXiv:1603.09320), with deterministic
//! seeded geometric levels, diversity selection, and retained pruned connections.
//! Stored vectors are L2-normalized f32. Model and source-set SHA-256 identities
//! are mandatory; an embedding from another model is never silently accepted.
mod build;
mod distance;
mod format;
mod search;

pub use build::Builder;
pub use distance::{normalize, score_backend};
pub use format::Snapshot;
pub use search::{Hit, Query, QueryWorkspace, SearchReport};

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

pub const MAX_DIMENSION: usize = 8192;
pub const MAX_ROWS: usize = 1_000_000;
pub const MAX_VECTOR_BYTES: u64 = 4 * 1024 * 1024 * 1024;
pub const MAX_EF: usize = 4096;
pub(crate) const MAX_LEVEL: usize = 16;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Space {
    pub dimension: usize,
    #[serde(with = "crate::sha256_hex")]
    pub model_sha256: [u8; 32],
}
impl Space {
    pub fn validate(&self) -> Result<()> {
        if !(1..=MAX_DIMENSION).contains(&self.dimension) || self.model_sha256 == [0; 32] {
            bail!("vector space requires dimension 1..{MAX_DIMENSION} and a nonzero model SHA-256");
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub neighbors: usize,
    pub ef_construction: usize,
    pub seed: u64,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            neighbors: 16,
            ef_construction: 200,
            seed: 0x464f_5247_4548_4e53,
        }
    }
}
impl Config {
    pub fn validate(self) -> Result<()> {
        if !(4..=64).contains(&self.neighbors)
            || !(self.neighbors..=MAX_EF).contains(&self.ef_construction)
        {
            bail!("HNSW requires neighbors 4..64 and ef_construction neighbors..{MAX_EF}");
        }
        Ok(())
    }
}

pub(crate) trait Graph {
    fn len(&self) -> usize;
    fn vector(&self, row: usize) -> &[f32];
    fn neighbors(&self, row: usize, layer: usize) -> &[u32];
    fn entry(&self) -> usize;
    fn top(&self) -> usize;
}
