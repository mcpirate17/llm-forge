//! Knowledge cards: bounded schema-2 storage and explicitly bound native recipes.
#[path = "cards_build.rs"]
mod build;
#[path = "cards_index.rs"]
mod index;
pub use build::{build, BuildReport, Settings};
pub use index::{load, score_query, search, CardIndex, Hit, SearchReport};

/// Whole cards and chunked memory use the same query-vector recipe.
pub use crate::memory_ingest::QUERY_PREFIX;
pub const MAX_INDEX_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_CARDS: usize = 4096;
pub const NATIVE_RECIPE: &str = "forge-knowledge-cards-v1";

pub fn fingerprint(config: &crate::embedding::Config) -> anyhow::Result<String> {
    crate::memory_ingest::fingerprint(config)
}
