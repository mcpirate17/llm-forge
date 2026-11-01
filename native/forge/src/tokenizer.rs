//! Offline, artifact-pinned deterministic BPE. Counts the exact supplied text;
//! chat templates and external message envelopes are the caller's responsibility.
use anyhow::{ensure, Context, Result};
use conductor_native::sha256_hex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::sync::Mutex;
use tokenizers::{models::ModelWrapper, Tokenizer};

pub const MAX_TEXT_BYTES: usize = 1024 * 1024;
pub const MAX_ARTIFACT_BYTES: usize = 128 * 1024 * 1024;
const IMPLEMENTATION: &str = "hf-rust-tokenizers-0.23.2/fancy-regex/deterministic-bpe/v1";

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub path: PathBuf,
    #[serde(with = "sha256_hex")]
    pub tokenizer_sha256: [u8; 32],
    #[serde(with = "sha256_hex")]
    pub model_sha256: [u8; 32],
    /// Run the artifact's postprocessor (e.g. BOS/EOS), exactly once per text.
    pub add_special_tokens: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct Identity {
    pub implementation: &'static str,
    #[serde(with = "sha256_hex")]
    pub tokenizer_sha256: [u8; 32],
    #[serde(with = "sha256_hex")]
    pub model_sha256: [u8; 32],
    #[serde(with = "sha256_hex")]
    pub recipe_sha256: [u8; 32],
    pub add_special_tokens: bool,
}

pub struct Counter {
    tokenizer: Tokenizer,
    identity: Identity,
    counts: Mutex<[Option<CountEntry>; 256]>,
}

#[derive(Clone, Copy)]
struct CountEntry {
    hash: [u8; 32],
    tokens: usize,
}

impl Config {
    pub fn validate(&self) -> Result<()> {
        ensure!(self.path.is_absolute(), "tokenizer path must be absolute");
        ensure!(
            self.tokenizer_sha256 != [0; 32] && self.model_sha256 != [0; 32],
            "tokenizer and model identities must be nonzero SHA-256 values"
        );
        Ok(())
    }
}

impl Counter {
    pub fn load(config: &Config) -> Result<Self> {
        config.validate()?;
        let bytes = crate::harness::state::read(&config.path, MAX_ARTIFACT_BYTES)?;
        ensure!(
            <[u8; 32]>::from(Sha256::digest(&bytes)) == config.tokenizer_sha256,
            "tokenizer artifact SHA-256 mismatch"
        );
        let mut tokenizer = Tokenizer::from_bytes(&bytes)
            .map_err(|error| anyhow::anyhow!("invalid tokenizer artifact: {error}"))?;
        let ModelWrapper::BPE(mut bpe) = tokenizer.get_model().clone() else {
            anyhow::bail!("only BPE tokenizer artifacts are supported");
        };
        ensure!(
            bpe.dropout.is_none_or(|p| p == 0.0),
            "BPE dropout is nondeterministic"
        );
        // Disable content retention in worker-local BPE caches. Batch/rayon APIs
        // are deliberately unused; one admitted caller uses one native thread.
        bpe.resize_cache(0);
        tokenizer.with_model(bpe);
        tokenizer.with_padding(None);
        tokenizer
            .with_truncation(None)
            .map_err(|e| anyhow::anyhow!("disable truncation: {e}"))?;
        let mut hash = Sha256::new();
        hash.update(IMPLEMENTATION.as_bytes());
        hash.update(config.tokenizer_sha256);
        hash.update(config.model_sha256);
        hash.update([u8::from(config.add_special_tokens)]);
        Ok(Self {
            tokenizer,
            counts: Mutex::new([None; 256]),
            identity: Identity {
                implementation: IMPLEMENTATION,
                tokenizer_sha256: config.tokenizer_sha256,
                model_sha256: config.model_sha256,
                recipe_sha256: hash.finalize().into(),
                add_special_tokens: config.add_special_tokens,
            },
        })
    }
    pub fn identity(&self) -> &Identity {
        &self.identity
    }

    pub fn encode(&self, text: &str) -> Result<tokenizers::Encoding> {
        self.encode_internal(text, true)
    }
    fn encode_internal(&self, text: &str, offsets: bool) -> Result<tokenizers::Encoding> {
        ensure!(text.len() <= MAX_TEXT_BYTES, "tokenizer text exceeds 1 MiB");
        let encoding = if offsets {
            self.tokenizer
                .encode(text, self.identity.add_special_tokens)
        } else {
            self.tokenizer
                .encode_fast(text, self.identity.add_special_tokens)
        }
        .map_err(|error| anyhow::anyhow!("BPE encoding failed: {error}"))?;
        ensure!(
            encoding.get_overflowing().is_empty(),
            "unexpected token truncation"
        );
        // Also bounds expanding normalizers in operator-supplied artifacts.
        ensure!(
            encoding.len() <= 4 * MAX_TEXT_BYTES,
            "encoded token limit exceeded"
        );
        Ok(encoding)
    }
    pub fn count(&self, text: &str) -> Result<usize> {
        ensure!(text.len() <= MAX_TEXT_BYTES, "tokenizer text exceeds 1 MiB");
        let hash: [u8; 32] = Sha256::digest(text.as_bytes()).into();
        let slot = usize::from(hash[0]);
        if let Some(entry) = self
            .counts
            .lock()
            .map_err(|_| anyhow::anyhow!("token count cache poisoned"))?[slot]
        {
            if entry.hash == hash {
                return Ok(entry.tokens);
            }
        }
        let tokens = self.count_uncached(text)?;
        // Fixed direct-mapped metadata cache: collisions evict, never alias.
        // No source text/tokens are retained and no lock is held during BPE.
        self.counts
            .lock()
            .map_err(|_| anyhow::anyhow!("token count cache poisoned"))?[slot] =
            Some(CountEntry { hash, tokens });
        Ok(tokens)
    }
    /// Exact offset-free control without the bounded hash/count cache.
    pub fn count_uncached(&self, text: &str) -> Result<usize> {
        Ok(self.encode_internal(text, false)?.len())
    }
    pub fn decode(&self, ids: &[u32]) -> Result<String> {
        ensure!(
            ids.len() <= 4 * MAX_TEXT_BYTES,
            "decoded token limit exceeded"
        );
        ensure!(
            ids.iter()
                .all(|&id| self.tokenizer.id_to_token(id).is_some()),
            "unknown token ID"
        );
        self.tokenizer
            .decode(ids, false)
            .map_err(|e| anyhow::anyhow!("decode failed: {e}"))
            .context("decode pinned BPE tokens")
    }
}
