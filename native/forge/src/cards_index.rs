//! Shared typed validation and deterministic scoring for offline and native card queries.
use super::{fingerprint, MAX_CARDS, MAX_INDEX_BYTES, NATIVE_RECIPE, QUERY_PREFIX};
use crate::{embedding, recall::Query};
use anyhow::{ensure, Context, Result};
use conductor_native::{
    kb_retrieve::{self, CardRef},
    sha256_hex,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;

#[derive(Serialize, Deserialize)]
pub(super) struct Metadata {
    pub fingerprint: String,
    pub dimension: usize,
    pub paid: bool,
    pub num_gpu: Option<u32>,
    pub num_ctx: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_recipe: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runner: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub threads: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query_prefix_sha256: Option<String>,
}
#[derive(Serialize, Deserialize)]
pub(super) struct Card {
    pub name: String,
    pub path: String,
    pub text: String,
    pub vector: Vec<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_sha256: Option<String>,
}
#[derive(Serialize, Deserialize)]
pub(super) struct Provenance {
    pub notes_root: String,
    pub source_set_sha256: String,
}
#[derive(Serialize, Deserialize)]
pub struct CardIndex {
    pub(super) schema_version: u32,
    pub(super) embedding: Metadata,
    pub(super) cards: Vec<Card>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) provenance: Option<Provenance>,
}
#[derive(Serialize)]
pub struct Hit {
    pub name: String,
    pub path: String,
    pub score: f64,
    pub text: String,
}
#[derive(Serialize)]
pub struct SearchReport {
    pub schema_version: u32,
    pub embedding_fingerprint: String,
    pub input_tokens: u64,
    pub hits: Vec<Hit>,
}
impl CardIndex {
    pub fn fingerprint(&self) -> &str {
        &self.embedding.fingerprint
    }
    pub fn dimension(&self) -> usize {
        self.embedding.dimension
    }
    pub fn len(&self) -> usize {
        self.cards.len()
    }
    pub fn is_empty(&self) -> bool {
        self.cards.is_empty()
    }
    pub(super) fn validate(&self) -> Result<()> {
        ensure!(self.schema_version == 2, "unsupported card index schema");
        let hash = self
            .embedding
            .fingerprint
            .strip_prefix("sha256:")
            .context("invalid card fingerprint")?;
        sha256_hex::parse(hash).map_err(anyhow::Error::msg)?;
        ensure!(
            (1..=8192).contains(&self.embedding.dimension),
            "invalid card dimension"
        );
        ensure!(
            self.embedding.paid
                || (matches!(self.embedding.num_gpu, Some(0 | 99))
                    && self.embedding.num_ctx == Some(2048)),
            "invalid card embedding metadata"
        );
        ensure!(
            !self.cards.is_empty() && self.cards.len() <= MAX_CARDS,
            "card index must contain 1..4096 cards"
        );
        for card in &self.cards {
            ensure!(
                !card.name.is_empty()
                    && card.name.len() <= 4096
                    && !card.path.is_empty()
                    && card.path.len() <= 4096
                    && card.text.len() <= 1024 * 1024
                    && !card.text.contains('\0'),
                "invalid card text or path"
            );
            ensure!(
                card.vector.len() == self.embedding.dimension
                    && card.vector.iter().all(|v| v.is_finite())
                    && card.vector.iter().any(|v| *v != 0.0),
                "invalid card vector"
            );
            if let Some(hash) = &card.source_sha256 {
                sha256_hex::parse(hash).map_err(anyhow::Error::msg)?;
            }
        }
        if self.embedding.native_recipe.is_some() {
            self.validate_native()?;
        } else {
            ensure!(
                self.embedding.artifact_sha256.is_none()
                    && self.embedding.runner.is_none()
                    && self.embedding.threads.is_none()
                    && self.embedding.query_prefix_sha256.is_none()
                    && self.provenance.is_none()
                    && self.cards.iter().all(|card| card.source_sha256.is_none()),
                "native card metadata requires native_recipe"
            );
        }

        Ok(())
    }
    fn validate_native(&self) -> Result<()> {
        if let Some(recipe) = &self.embedding.native_recipe {
            ensure!(
                recipe == NATIVE_RECIPE && !self.embedding.paid,
                "unsupported native card recipe"
            );
            let artifact = sha256_hex::parse(
                self.embedding
                    .artifact_sha256
                    .as_deref()
                    .context("native card artifact hash missing")?,
            )
            .map_err(anyhow::Error::msg)?;
            ensure!(
                self.embedding.query_prefix_sha256.as_deref()
                    == Some(format!("{:x}", Sha256::digest(QUERY_PREFIX)).as_str()),
                "native card query prefix mismatch"
            );
            let config = embedding::Config {
                endpoint: "http://127.0.0.1:1".into(),
                model: self
                    .embedding
                    .model
                    .clone()
                    .context("native card model missing")?,
                runner: self
                    .embedding
                    .runner
                    .clone()
                    .context("native card runner missing")?,
                artifact_sha256: artifact,
                dimension: self.embedding.dimension,
                context_tokens: self
                    .embedding
                    .num_ctx
                    .context("native card context missing")?
                    as usize,
                gpu_layers: self
                    .embedding
                    .num_gpu
                    .context("native card GPU setting missing")?
                    as usize,
                threads: self
                    .embedding
                    .threads
                    .context("native card thread count missing")?,
                timeout_seconds: 1,
            };
            ensure!(
                fingerprint(&config)? == self.embedding.fingerprint,
                "native card recipe fingerprint mismatch"
            );
            self.validate_native_sources()?;
        }
        Ok(())
    }
    fn validate_native_sources(&self) -> Result<()> {
        for card in &self.cards {
            let norm: f64 = card.vector.iter().map(|value| value * value).sum();
            ensure!(
                norm.is_finite() && (norm - 1.0).abs() <= 1e-4,
                "invalid native card vector norm"
            );
            let actual = format!("{:x}", Sha256::digest(card.text.as_bytes()));
            ensure!(
                card.source_sha256.as_deref() == Some(actual.as_str()),
                "native card source hash mismatch"
            );
        }
        let provenance = self
            .provenance
            .as_ref()
            .context("native card provenance missing")?;
        let root = Path::new(&provenance.notes_root);
        ensure!(
            root.is_absolute(),
            "native card notes root must be absolute"
        );
        let mut last: Option<&str> = None;
        for card in &self.cards {
            ensure!(
                super::build::card_name(card.name.as_bytes())
                    && !card.name.contains('/')
                    && !card.name.contains('\\')
                    && last.is_none_or(|previous| previous < card.name.as_str())
                    && root.join(&card.name) == Path::new(&card.path),
                "invalid native card source ordering or path"
            );
            last = Some(&card.name);
        }
        let rows: Vec<_> = self
            .cards
            .iter()
            .map(|card| {
                (
                    &card.name,
                    card.source_sha256.as_ref().expect("validated source hash"),
                )
            })
            .collect();
        let hash = format!("{:x}", Sha256::digest(serde_json::to_vec(&rows)?));
        ensure!(
            hash == provenance.source_set_sha256,
            "native card source set hash mismatch"
        );
        Ok(())
    }
}
pub fn load(path: &Path) -> Result<CardIndex> {
    let bytes = crate::bounded_file::read(path, MAX_INDEX_BYTES)?;
    let index: CardIndex = serde_json::from_value(crate::tool_schema::parse_document(
        std::str::from_utf8(&bytes)?,
    )?)
    .context("knowledge-card index")?;
    index.validate()?;
    Ok(index)
}
fn score(index: &CardIndex, vector: &[f64], top_k: usize) -> Result<Vec<Hit>> {
    ensure!((1..=64).contains(&top_k), "card top_k must be 1..64");
    ensure!(
        vector.len() == index.dimension()
            && vector.iter().all(|v| v.is_finite())
            && vector.iter().any(|v| *v != 0.0),
        "invalid card query vector"
    );
    let cards = index.cards.iter().map(|c| CardRef {
        name: &c.name,
        path: &c.path,
        text: &c.text,
        vector: &c.vector,
    });
    let hits = kb_retrieve::score_card_refs(vector, cards, top_k).map_err(anyhow::Error::msg)?;
    ensure!(
        hits.iter().all(|h| h.score.is_finite()),
        "card score is non-finite"
    );
    Ok(hits
        .into_iter()
        .map(|h| Hit {
            name: h.name,
            path: h.path,
            score: h.score,
            text: h.text,
        })
        .collect())
}
pub fn score_query(index: &CardIndex, query: &Query, top_k: usize) -> Result<Vec<Hit>> {
    ensure!(
        index.fingerprint() == query.fingerprint(),
        "card embedding fingerprint mismatch"
    );
    score(index, query.vector(), top_k)
}
pub fn search(
    index: &CardIndex,
    config: &embedding::Config,
    task: &str,
    top_k: usize,
) -> Result<SearchReport> {
    ensure!(
        !task.trim().is_empty() && task.len() <= 4096,
        "invalid card search task"
    );
    ensure!((1..=64).contains(&top_k), "card top_k must be 1..64");
    ensure!(
        index.embedding.native_recipe.as_deref() == Some(NATIVE_RECIPE),
        "legacy card vectors require an explicit native rebuild"
    );
    ensure!(
        index.fingerprint() == fingerprint(config)?,
        "card embedding recipe mismatch"
    );
    let result =
        embedding::Client::new(config.clone())?.embed(&crate::memory_ingest::query_text(task)?)?;
    let vector: Vec<f64> = result.vector.into_iter().map(f64::from).collect();
    let hits = score(index, &vector, top_k)?;
    Ok(SearchReport {
        schema_version: 1,
        embedding_fingerprint: index.fingerprint().into(),
        input_tokens: result.input_tokens,
        hits,
    })
}
