//! Query preparation uses the operator's refresh batch limits, preserving order.
use anyhow::{ensure, Context, Result};
use forge::{
    code_index::extract::BatchPolicy,
    embedding::{Batch, Client},
};
use serde::Serialize;
use std::time::Instant;

/// Fully validated completed preparation batches, never in-flight cost.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct Progress {
    pub batch: BatchPolicy,
    pub rows: u64,
    pub batches: u64,
    pub input_tokens_total: u64,
}

pub fn queries(
    client: &Client,
    texts: &[&str],
    policy: BatchPolicy,
    deadline: Instant,
    mut observer: impl FnMut(Progress) -> Result<()>,
) -> Result<(Batch, u64)> {
    policy.validate(128)?;
    ensure!(
        texts.len() <= 1024 && texts.iter().all(|text| !text.is_empty()
            && text.len() <= 16384 && text.len() <= policy.max_bytes && !text.contains('\0')),
        "query preparation requires <=1024 nonempty texts fitting the batch byte limit, without NUL"
    );
    let mut result = Batch {
        space: client.space().clone(),
        vectors: Vec::with_capacity(texts.len()),
        input_tokens_total: 0,
    };
    let mut progress = Progress {
        batch: policy,
        rows: 0,
        batches: 0,
        input_tokens_total: 0,
    };
    observer(progress)?;
    let mut start = 0;
    while start < texts.len() {
        let (mut end, mut bytes) = (start, 0);
        while end < texts.len()
            && end - start < policy.max_inputs
            && bytes + texts[end].len() <= policy.max_bytes
        {
            bytes += texts[end].len();
            end += 1;
        }
        ensure!(end > start, "query embedding batching stalled");
        let batch = client.embed_batch(&texts[start..end], deadline)?;
        let tokens = result
            .input_tokens_total
            .checked_add(batch.input_tokens_total)
            .context("completed query input token total overflow")?;
        progress.rows += (end - start) as u64;
        progress.batches += 1;
        progress.input_tokens_total = tokens;
        observer(progress)?;
        result.vectors.extend(batch.vectors);
        result.input_tokens_total = tokens;
        start = end;
    }
    Ok((result, progress.batches))
}
