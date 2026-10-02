//! Hash and parser-version bound syntax facts; no reuse across parser contracts.

use anyhow::{ensure, Context, Result};
use conductor_native::graph_index::FileFacts;
use rusqlite::{Connection, OpenFlags};
use std::collections::BTreeMap;
use std::path::Path;

pub const PARSER_VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), ":syntax-facts-v5");
pub type FactCache = BTreeMap<String, (String, FileFacts)>;

pub fn load(path: &Path) -> Result<FactCache> {
    if !path.is_file() {
        return Ok(BTreeMap::new());
    }
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .context("open previous graph syntax cache")?;
    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='index_meta')",
        [],
        |row| row.get(0),
    )?;
    if !exists {
        return Ok(BTreeMap::new());
    }
    let version = conn.query_row(
        "SELECT value FROM index_meta WHERE key='parser_version'",
        [],
        |row| row.get::<_, String>(0),
    );
    match version {
        Ok(version) if version == PARSER_VERSION => {}
        Ok(_) | Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(BTreeMap::new()),
        Err(error) => return Err(error).context("read graph parser version"),
    }
    let mut stmt = conn.prepare(
        "SELECT file_path,file_hash,facts_json FROM files ORDER BY file_path LIMIT 30001",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    let mut cache = BTreeMap::new();
    for row in rows {
        let (path, hash, facts) = row?;
        ensure!(
            cache.len() < 30000 && facts.len() <= 8 << 20,
            "graph syntax cache exceeds bounds"
        );
        cache.insert(
            path,
            (
                hash,
                serde_json::from_str(&facts).context("decode cached syntax facts")?,
            ),
        );
    }
    Ok(cache)
}
