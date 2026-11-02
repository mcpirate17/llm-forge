use super::{store::Pinned, Manifest, Store, MAX_CHUNK_BYTES};
use crate::source_file::SourceRoot;
use anyhow::{ensure, Context, Result};
use conductor_native::vector_index::{Query, QueryWorkspace, Snapshot, Space, MAX_EF};
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

pub(super) struct ReuseRow {
    pub index: usize,
    pub path: String,
    pub source_sha256: String,
    pub start: usize,
    pub end: usize,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchMode {
    #[default]
    Exact,
    Hnsw,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct QueryRequest {
    pub text: String,
    pub vector: Option<Vec<f32>>,
    pub space: Option<Space>,
    #[serde(default)]
    pub mode: SearchMode,
    pub limit: usize,
    pub candidates: usize,
    pub ef: usize,
}
impl QueryRequest {
    pub fn validate(&self) -> Result<()> {
        validate_request(self)
    }
}
#[derive(Debug, Serialize)]
pub struct SearchHit {
    pub row: usize,
    pub path: String,
    pub source_sha256: String,
    pub start_byte: usize,
    pub end_byte: usize,
    pub text: String,
    pub reciprocal_rank: f64,
    pub cosine: Option<f64>,
    pub lexical_rank: Option<usize>,
}
#[derive(Debug, Serialize)]
pub struct SearchResult {
    pub generation: String,
    pub indexed_rows: usize,
    pub coverage: &'static str,
    pub mode: SearchMode,
    pub distance_evaluations: usize,
    pub candidates_considered: usize,
    pub stale_candidates: usize,
    pub unreadable_candidates: usize,
    pub read_errors: Vec<ReadError>,
    pub source_bytes_read: usize,
    pub source_read_budget_charged: usize,
    pub freshness_budget_exhausted: bool,
    pub output_truncated: bool,
    pub hits: Vec<SearchHit>,
}
#[derive(Debug, Serialize)]
pub struct ReadError {
    pub path: String,
    pub error: String,
}
pub struct Reader {
    pinned: Pinned,
    database: Connection,
    vectors: Snapshot,
    workspace: QueryWorkspace,
}
#[derive(Clone, Copy, Debug)]
pub struct ReadLimits {
    pub max_rows: usize,
    pub max_mapped_bytes: u64,
}
impl Default for ReadLimits {
    fn default() -> Self {
        Self {
            max_rows: conductor_native::vector_index::MAX_ROWS,
            max_mapped_bytes: 8 * 1024 * 1024 * 1024,
        }
    }
}
impl ReadLimits {
    pub fn validate(self) -> Result<()> {
        ensure!(
            self.max_rows <= conductor_native::vector_index::MAX_ROWS
                && (264..=8 * 1024 * 1024 * 1024).contains(&self.max_mapped_bytes),
            "invalid code-index reader limits"
        );
        Ok(())
    }
}
struct Candidate {
    row: usize,
    cosine: Option<f64>,
    lexical: Option<usize>,
    fused: f64,
}
impl Reader {
    pub fn open(store: &Store) -> Result<Self> {
        Self::open_limited(store, ReadLimits::default())
    }
    pub fn open_limited(store: &Store, limits: ReadLimits) -> Result<Self> {
        limits.validate()?;
        let pinned = store.pin(limits)?;
        let vectors = Snapshot::open(&pinned.path.join("vectors.bin"))?;
        ensure!(
            vectors.len() == pinned.manifest.rows
                && vectors.space() == &pinned.manifest.space
                && vectors.config() == pinned.manifest.config
                && vectors.source_sha256() == pinned.manifest.source_set_sha256,
            "vector and code manifest identities differ"
        );
        let database = Connection::open_with_flags(
            pinned.path.join("chunks.sqlite"),
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        database.execute_batch(
            "PRAGMA query_only=ON; PRAGMA trusted_schema=OFF; PRAGMA cache_size=-8192;",
        )?;
        let (rows, low, high): (usize, usize, usize) = database.query_row(
            "SELECT count(*),coalesce(min(rowid),0),coalesce(max(rowid),0) FROM chunks",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        ensure!(
            rows == vectors.len() && low == usize::from(rows != 0) && high == rows,
            "code and vector row identities differ"
        );
        let workspace = QueryWorkspace::new(vectors.len(), MAX_EF)?;
        Ok(Self {
            pinned,
            database,
            vectors,
            workspace,
        })
    }
    pub(super) fn reuse_metadata(
        &self,
        checkpoint: &mut dyn FnMut() -> Result<()>,
    ) -> Result<Vec<ReuseRow>> {
        let mut statement = self.database.prepare(
            "SELECT rowid,path,source_sha256,start_byte,end_byte FROM chunks ORDER BY rowid",
        )?;
        let mut rows = statement.query([])?;
        let mut out = Vec::new();
        let mut budget = 0usize;
        while let Some(row) = rows.next()? {
            checkpoint()?;
            let id: usize = row.get(0)?;
            let path: String = row.get(1)?;
            let hash: String = row.get(2)?;
            let start: usize = row.get(3)?;
            let end: usize = row.get(4)?;
            budget = budget
                .checked_add(path.len() + hash.len() + 96)
                .context("reuse metadata budget overflow")?;
            ensure!(
                budget <= 128 * 1024 * 1024,
                "reuse metadata exceeds 128 MiB"
            );
            ensure!(id == out.len() + 1, "reuse row identity is invalid");
            out.push(ReuseRow {
                index: id - 1,
                path,
                source_sha256: hash,
                start,
                end,
            });
        }
        ensure!(
            out.len() == self.vectors.len(),
            "reuse metadata row count mismatch"
        );
        Ok(out)
    }
    pub(super) fn reuse_vector(&self, row: usize, expected: &str) -> Result<Vec<f32>> {
        let text: String = self
            .database
            .prepare_cached("SELECT text FROM chunks WHERE rowid=?1")?
            .query_row([row + 1], |r| r.get(0))?;
        ensure!(
            text == expected,
            "reusable chunk content differs from verified source"
        );
        Ok(self.vectors.row(row)?.to_vec())
    }
    pub fn manifest(&self) -> &Manifest {
        &self.pinned.manifest
    }
    pub fn reserved_bytes(&self) -> usize {
        self.workspace.reserved_bytes()
    }
    pub fn search(&mut self, source: &Path, mut request: QueryRequest) -> Result<SearchResult> {
        request.validate()?;
        ensure!(
            source.canonicalize()? == self.manifest().source_root,
            "index belongs to a different source root"
        );
        let query = match (request.space.take(), request.vector.take()) {
            (Some(space), Some(vector)) => Some(Query::new(space, vector)?),
            (None, None) => None,
            _ => anyhow::bail!("query vector and space must be supplied together"),
        };
        let (candidates, evaluations) = self.candidates(&request, query.as_ref())?;
        let root = SourceRoot::open(source)?;
        self.materialize(&root, &request, candidates, evaluations)
    }
    fn candidates(
        &mut self,
        request: &QueryRequest,
        query: Option<&Query>,
    ) -> Result<(Vec<Candidate>, usize)> {
        let mut union = BTreeSet::new();
        let mut lexical = BTreeMap::new();
        let expression = lexical_expression(&request.text)?;
        if !expression.is_empty() {
            let mut statement = self.database.prepare_cached("SELECT rowid FROM chunks_fts WHERE chunks_fts MATCH ?1 ORDER BY rank,rowid LIMIT ?2")?;
            let mut rows = statement.query(rusqlite::params![expression, request.candidates])?;
            while let Some(row) = rows.next()? {
                let index = row
                    .get::<_, usize>(0)?
                    .checked_sub(1)
                    .context("invalid lexical row id")?;
                ensure!(index < self.vectors.len(), "lexical row out of bounds");
                lexical.insert(index, lexical.len() + 1);
                union.insert(index);
            }
        }
        let mut evaluations = 0;
        if let Some(query) = query {
            let found = match request.mode {
                SearchMode::Exact => {
                    self.vectors
                        .exact(query, request.candidates, &mut self.workspace)?
                }
                SearchMode::Hnsw => self.vectors.search(
                    query,
                    request.candidates,
                    request.ef,
                    &mut self.workspace,
                )?,
            };
            evaluations = found.distance_evaluations;
            union.extend(found.hits.iter().map(|h| h.row as usize));
        }
        let mut ranked: Vec<_> = union
            .into_iter()
            .map(|row| {
                Ok(Candidate {
                    row,
                    cosine: query.map(|q| self.vectors.score(q, row)).transpose()?,
                    lexical: lexical.get(&row).copied(),
                    fused: 0.0,
                })
            })
            .collect::<Result<_>>()?;
        // Re-rank the entire candidate union by exact cosine before fusion.
        ranked.sort_by(|a, b| {
            b.cosine
                .unwrap_or(0.0)
                .total_cmp(&a.cosine.unwrap_or(0.0))
                .then(a.row.cmp(&b.row))
        });
        for (index, candidate) in ranked.iter_mut().enumerate() {
            candidate.fused = candidate
                .lexical
                .map_or(0.0, |rank| 1.0 / (60 + rank) as f64)
                + if query.is_some() {
                    1.0 / (61 + index) as f64
                } else {
                    0.0
                };
        }
        if query.is_some() {
            evaluations += ranked.len();
        }
        ranked.sort_by(|a, b| b.fused.total_cmp(&a.fused).then(a.row.cmp(&b.row)));
        Ok((ranked, evaluations))
    }
    fn materialize(
        &self,
        root: &SourceRoot,
        request: &QueryRequest,
        candidates: Vec<Candidate>,
        evaluations: usize,
    ) -> Result<SearchResult> {
        let mut result = SearchResult { generation: self.manifest().generation.clone(), indexed_rows: self.vectors.len(),
            coverage: "indexed snapshot only; new paths require refresh; every returned source hash checked at read time",
            mode: request.mode, distance_evaluations: evaluations, candidates_considered: candidates.len(),
            stale_candidates: 0, unreadable_candidates: 0, read_errors: Vec::new(), source_bytes_read: 0, source_read_budget_charged: 0,
            freshness_budget_exhausted: false, output_truncated: false, hits: Vec::new() };
        let mut output_bytes = 32768;
        let mut statement = self.database.prepare_cached(
            "SELECT path,source_sha256,start_byte,end_byte FROM chunks WHERE rowid=?1",
        )?;
        for candidate in candidates {
            if result.hits.len() == request.limit {
                break;
            }
            if result.source_read_budget_charged + conductor_native::syntax::MAX_SOURCE_BYTES
                > 64 * 1024 * 1024
            {
                result.freshness_budget_exhausted = true;
                break;
            }
            let (path, hash, start, end): (String, String, usize, usize) = statement
                .query_row([candidate.row + 1], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
                })?;
            crate::harness::policy::source_path(Path::new(&path))?;
            let source = match root.read(Path::new(&path)) {
                Ok(source) => source,
                Err(error) => {
                    result.source_read_budget_charged += conductor_native::syntax::MAX_SOURCE_BYTES;
                    result.unreadable_candidates += 1;
                    if result.read_errors.len() < 4 {
                        result.read_errors.push(ReadError {
                            path: path.chars().take(512).collect(),
                            error: format!("{error:#}").chars().take(512).collect(),
                        });
                    }
                    continue;
                }
            };
            result.source_bytes_read += source.content().len();
            result.source_read_budget_charged += source.content().len();
            if source.sha256() != hash {
                result.stale_candidates += 1;
                continue;
            }
            ensure!(
                end > start && end - start <= MAX_CHUNK_BYTES,
                "invalid indexed code span"
            );
            let text = source
                .content()
                .get(start..end)
                .context("indexed span splits source")?
                .to_owned();
            let hit = SearchHit {
                row: candidate.row,
                path,
                source_sha256: hash,
                start_byte: start,
                end_byte: end,
                text,
                reciprocal_rank: candidate.fused,
                cosine: candidate.cosine,
                lexical_rank: candidate.lexical,
            };
            output_bytes += serde_json::to_vec(&hit)?.len() + 1;
            if output_bytes > 1024 * 1024 {
                result.output_truncated = true;
                break;
            }
            result.hits.push(hit);
        }
        Ok(result)
    }
}
fn validate_request(request: &QueryRequest) -> Result<()> {
    ensure!(
        request.text.len() <= 4096
            && !request.text.contains('\0')
            && (1..=100).contains(&request.limit)
            && (request.limit..=MAX_EF).contains(&request.candidates)
            && (request.candidates..=MAX_EF).contains(&request.ef),
        "invalid search text or candidate bounds"
    );
    ensure!(
        !request.text.trim().is_empty() || request.vector.is_some(),
        "search needs text or an embedding"
    );
    Ok(())
}
fn lexical_expression(text: &str) -> Result<String> {
    // Treat every term literally. User/model text never becomes an FTS operator.
    let terms: Vec<_> = text
        .split_whitespace()
        .filter(|word| word.chars().any(char::is_alphanumeric))
        .collect();
    ensure!(
        terms.len() <= 64,
        "search contains more than 64 lexical terms"
    );
    Ok(terms
        .iter()
        .map(|word| format!("\"{}\"", word.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" OR "))
}
