//! Explicit offline semantic recall. Query vectors bind the exact task and
//! vector-space fingerprint; supplied indexes are errors if invalid or absent.
#[path = "recall_cli.rs"]
pub mod cli;
use anyhow::{ensure, Context, Result};
use conductor_native::{kb_retrieve, memory_index, memory_index_sidecar, sha256_hex};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EncodedQuery {
    schema_version: u32,
    task_sha256: String,
    embedding_fingerprint: String,
    vector: Vec<f64>,
}

/// Validated, normalized query; constructed only by the task-binding loader.
pub struct Query {
    embedding_fingerprint: String,
    vector: Vec<f64>,
}

impl Query {
    pub fn load(path: &Path, task: &str) -> Result<Self> {
        let bytes = crate::bounded_file::read(path, 1024 * 1024)?;
        Self::from_document(
            crate::tool_schema::parse_document(std::str::from_utf8(&bytes)?)?,
            task,
        )
    }

    pub fn from_document(document: serde_json::Value, task: &str) -> Result<Self> {
        ensure!(
            !task.trim().is_empty() && task.len() <= 4096,
            "invalid recall task"
        );
        let mut query: EncodedQuery = serde_json::from_value(document)?;
        ensure!(query.schema_version == 1, "unsupported recall query schema");
        let digest = format!("{:x}", Sha256::digest(task.trim().as_bytes()));
        ensure!(
            query.task_sha256 == digest,
            "query vector does not bind this task"
        );
        let hash = query
            .embedding_fingerprint
            .strip_prefix("sha256:")
            .unwrap_or("");
        sha256_hex::parse(hash).map_err(anyhow::Error::msg)?;
        ensure!(
            (1..=8192).contains(&query.vector.len()) && query.vector.iter().all(|v| v.is_finite()),
            "query vector must contain 1..8192 finite values"
        );
        let scale = query.vector.iter().map(|v| v.abs()).fold(0.0, f64::max);
        ensure!(scale > 0.0, "query vector must be nonzero");
        for value in &mut query.vector {
            *value /= scale;
        }
        query.vector = kb_retrieve::l2_normalize(&query.vector).map_err(anyhow::Error::msg)?;
        Ok(Self {
            embedding_fingerprint: query.embedding_fingerprint,
            vector: query.vector,
        })
    }

    pub fn fingerprint(&self) -> &str {
        &self.embedding_fingerprint
    }
    pub fn vector(&self) -> &[f64] {
        &self.vector
    }
}

/// Strip leading frontmatter and collapse whitespace without copying the body.
pub fn snippet(text: &str, limit: usize) -> String {
    let body = text
        .strip_prefix("---")
        .and_then(|rest| rest.find("\n---").map(|i| &rest[i + 4..]))
        .unwrap_or(text);
    let mut chars = body
        .split_whitespace()
        .enumerate()
        .flat_map(|(i, word)| (i != 0).then_some(' ').into_iter().chain(word.chars()));
    let mut result: String = chars.by_ref().take(limit).collect();
    if chars.next().is_some() && limit != 0 {
        result.pop();
        result.truncate(result.trim_end().len());
        result.push('…');
    }
    result
}

pub fn sidecar_path(source: &Path) -> PathBuf {
    let mut name = source.as_os_str().to_os_string();
    name.push(".sidecar");
    PathBuf::from(name)
}

pub fn memory_hits(
    path: &Path,
    sidecar: Option<&Path>,
    query: &Query,
    count: usize,
) -> Result<Vec<memory_index::NativeHit>> {
    ensure!(
        (1..=64).contains(&count),
        "memory preview count must be 1..64"
    );
    let snapshot = sidecar
        .map(Path::to_path_buf)
        .unwrap_or_else(|| sidecar_path(path));
    memory_index_sidecar::Reader::open(path, &snapshot)
        .with_context(|| {
            format!(
                "opening memory snapshot {}; explicitly run `forge memory build`",
                snapshot.display()
            )
        })?
        .query(&query.vector, count, &query.embedding_fingerprint)
}

pub fn memory(path: &Path, sidecar: Option<&Path>, query: &Query) -> Result<Vec<String>> {
    let hits = memory_hits(path, sidecar, query, 3)?;
    Ok(hits
        .into_iter()
        .map(|(_, source, path, title, text)| {
            format!(
                "- {}/{}: {} ({})",
                snippet(&source, 80),
                snippet(&title, 120),
                snippet(&text, 240),
                snippet(&path, 240)
            )
        })
        .collect())
}

pub fn cards(path: &Path, query: &Query) -> Result<Vec<String>> {
    let index = crate::cards::load(path)?;
    let hits = crate::cards::score_query(&index, query, 3)?;
    Ok(hits
        .into_iter()
        .map(|hit| {
            format!(
                "- {}: {} ({})",
                snippet(&hit.name, 120),
                snippet(&hit.text, 240),
                snippet(&hit.path, 240)
            )
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn snippets_are_frontmatter_free_unicode_bounded_and_whitespace_collapsed() {
        for (input, limit, expected) in [
            ("---\nkey: value\n---\n a  🦀 b", 20, "a 🦀 b"),
            ("abc🦀d", 4, "abc…"),
            ("a   b", 3, "a b"),
            ("a b c", 3, "a…"),
            ("long", 1, "…"),
            ("long", 0, ""),
            ("---unterminated", 20, "---unterminated"),
            ("  \n", 10, ""),
        ] {
            assert_eq!(snippet(input, limit), expected);
        }
    }
}
