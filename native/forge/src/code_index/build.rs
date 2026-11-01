use super::{file_hash, store::seal, validate_hash, Manifest, Store, MAX_CHUNK_BYTES};
use crate::source_file::{Snapshot as Source, SourceRoot};
use anyhow::{ensure, Context, Result};
use conductor_native::vector_index::{Builder, Config, Space, MAX_ROWS, MAX_VECTOR_BYTES};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildHeader {
    pub version: u32,
    pub rows: usize,
    pub space: Space,
    #[serde(default)]
    pub config: Config,
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EmbeddingRow {
    pub path: String,
    pub source_sha256: String,
    pub start_byte: usize,
    pub end_byte: usize,
    pub vector: Vec<f32>,
}

/// Build and atomically publish from an ordered stream of source-bound embeddings.
/// No supplied text is trusted: searchable text is read through SourceRoot.
pub fn build(
    store: &Store,
    source: &Path,
    header: BuildHeader,
    rows: impl IntoIterator<Item = Result<EmbeddingRow>>,
) -> Result<Manifest> {
    build_checked(store, source, header, rows, || Ok(()))
}

pub(crate) fn build_checked(
    store: &Store,
    source: &Path,
    header: BuildHeader,
    rows: impl IntoIterator<Item = Result<EmbeddingRow>>,
    mut checkpoint: impl FnMut() -> Result<()>,
) -> Result<Manifest> {
    checkpoint()?;
    header.space.validate()?;
    header.config.validate()?;
    ensure!(
        header.version == 1 && header.rows <= MAX_ROWS,
        "invalid embedding stream header"
    );
    let cells = header
        .rows
        .checked_mul(header.space.dimension)
        .context("matrix shape overflow")?;
    ensure!(
        (cells as u64) * 4 <= MAX_VECTOR_BYTES,
        "embedding matrix exceeds 4 GiB"
    );
    let source = source.canonicalize()?;
    ensure!(
        !source.starts_with(store.root()) && !store.root().starts_with(&source),
        "index state and source root must be disjoint"
    );
    let root = SourceRoot::open(&source)?;
    let staging = store.begin()?;
    let database = staging.path.join("chunks.sqlite");
    let mut connection = create_database(&database)?;
    let mut vectors = Vec::new();
    vectors.try_reserve_exact(cells)?;
    let source_set_sha256 = ingest(
        &mut connection,
        &root,
        &header,
        rows,
        &mut vectors,
        &mut checkpoint,
    )?;
    checkpoint()?;
    recheck_sources(&connection, &root, &mut checkpoint)?;
    connection
        .execute_batch("INSERT INTO chunks_fts(chunks_fts) VALUES('optimize'); PRAGMA optimize;")?;
    connection.close().map_err(|(_, e)| e)?;
    seal(&database)?;
    let vectors_path = staging.path.join("vectors.bin");
    let builder = Builder::new_checked(
        header.space.clone(),
        vectors,
        header.config,
        &mut checkpoint,
    )?;
    builder.write(&vectors_path, source_set_sha256)?;
    let manifest = Manifest {
        version: 1,
        generation: staging.id.clone(),
        source_root: source,
        rows: header.rows,
        source_set_sha256,
        space: header.space,
        config: header.config,
        database_sha256: file_hash(&database)?,
        vectors_sha256: file_hash(&vectors_path)?,
    };
    checkpoint()?;
    staging.publish(&manifest)
}

fn create_database(path: &Path) -> Result<Connection> {
    let connection = Connection::open(path)?;
    connection.execute_batch("PRAGMA journal_mode=DELETE; PRAGMA synchronous=FULL; PRAGMA page_size=4096;
        PRAGMA max_page_count=1048576; PRAGMA cache_size=-16384; PRAGMA trusted_schema=OFF;
        CREATE TABLE chunks(rowid INTEGER PRIMARY KEY, path TEXT NOT NULL, source_sha256 TEXT NOT NULL,
          start_byte INTEGER NOT NULL, end_byte INTEGER NOT NULL, text TEXT NOT NULL) STRICT;
        CREATE VIRTUAL TABLE chunks_fts USING fts5(path, text, content='chunks', content_rowid='rowid', tokenize='unicode61');")?;
    Ok(connection)
}

fn ingest(
    connection: &mut Connection,
    root: &SourceRoot,
    header: &BuildHeader,
    rows: impl IntoIterator<Item = Result<EmbeddingRow>>,
    vectors: &mut Vec<f32>,
    checkpoint: &mut impl FnMut() -> Result<()>,
) -> Result<[u8; 32]> {
    let transaction = connection.transaction()?;
    let mut insert = transaction.prepare("INSERT INTO chunks VALUES(?1,?2,?3,?4,?5,?6)")?;
    let mut index =
        transaction.prepare("INSERT INTO chunks_fts(rowid,path,text) VALUES(?1,?2,?3)")?;
    let mut cached: Option<Source> = None;
    let mut previous: Option<(String, usize, usize)> = None;
    let mut hash = Sha256::new();
    let mut count = 0;
    for row in rows {
        checkpoint()?;
        let row = row?;
        ensure!(
            count < header.rows,
            "embedding stream has more rows than declared"
        );
        ensure!(
            row.vector.len() == header.space.dimension,
            "embedding row dimension mismatch"
        );
        ensure!(
            row.vector.iter().all(|v| v.is_finite()) && row.vector.iter().any(|&v| v != 0.0),
            "embedding row must have a finite nonzero cosine direction"
        );
        validate_hash(&row.source_sha256)?;
        let path = PathBuf::from(&row.path);
        crate::harness::policy::source_path(&path)?;
        ensure!(
            row.path.len() <= 4096
                && row
                    .path
                    .split('/')
                    .all(|part| !part.is_empty() && part != "." && part != ".."),
            "invalid source path"
        );
        let key = (row.path.clone(), row.start_byte, row.end_byte);
        ensure!(
            previous.as_ref().is_none_or(|last| &key > last),
            "embedding rows must be unique and sorted by path/start/end"
        );
        if cached.as_ref().is_none_or(|s| s.path() != path) {
            cached = Some(root.read(&path)?);
        }
        let source = cached.as_ref().context("missing source cache")?;
        ensure!(
            source.sha256() == row.source_sha256,
            "embedding source hash mismatch for {}",
            row.path
        );
        ensure!(
            row.end_byte > row.start_byte && row.end_byte - row.start_byte <= MAX_CHUNK_BYTES,
            "invalid code chunk span"
        );
        let text = source
            .content()
            .get(row.start_byte..row.end_byte)
            .context("chunk span is outside source or splits UTF-8")?;
        insert.execute(params![
            count + 1,
            row.path,
            row.source_sha256,
            row.start_byte,
            row.end_byte,
            text
        ])?;
        index.execute(params![count + 1, row.path, text])?;
        let binding = serde_json::to_vec(&(&key, &row.source_sha256))?;
        hash.update((binding.len() as u64).to_le_bytes());
        hash.update(binding);
        vectors.extend(row.vector);
        previous = Some(key);
        count += 1;
    }
    ensure!(
        count == header.rows,
        "embedding stream row count does not match header"
    );
    drop(insert);
    drop(index);
    transaction.commit()?;
    Ok(hash.finalize().into())
}

fn recheck_sources(
    connection: &Connection,
    root: &SourceRoot,
    checkpoint: &mut impl FnMut() -> Result<()>,
) -> Result<()> {
    let mut statement =
        connection.prepare("SELECT DISTINCT path,source_sha256 FROM chunks ORDER BY path")?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        checkpoint()?;
        let path: String = row.get(0)?;
        let expected: String = row.get(1)?;
        ensure!(
            root.read(Path::new(&path))?.sha256() == expected,
            "source changed during index construction: {path}"
        );
    }
    Ok(())
}
