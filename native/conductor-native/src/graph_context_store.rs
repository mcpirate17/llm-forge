//! Read-only, bounded queries against a Forge or external structural index.

use anyhow::{ensure, Context, Result};
use rusqlite::{params, Connection};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use super::database::{QueryScope as Operation, Reader};
use super::files::SourceRoot;
#[path = "graph_context_evidence.rs"]
mod evidence;

pub const DEFAULT_DB: &str = ".forge/graph.db";
const REQUIRED_NODES: &[&str] = &["qualified_name", "file_path"];
const REQUIRED_EDGES: &[&str] = &["source_qualified", "target_qualified"];

#[derive(Clone, Debug, serde::Deserialize, Serialize)]
pub struct Symbol {
    pub qualified_name: String,
    pub name: String,
    pub kind: String,
    pub line_start: i64,
    pub line_end: i64,
    pub signature: Option<String>,
}

#[derive(Clone, Debug, serde::Deserialize, Serialize)]
pub struct Relationship {
    pub qualified_name: String,
    pub kind: String,
    pub file_path: String,
    pub line: i64,
}

#[derive(Debug, Serialize)]
pub struct Projection {
    pub graph_status: String,
    pub symbols: Vec<Symbol>,
    pub callers: Vec<Relationship>,
    pub callees: Vec<Relationship>,
    pub indexed_hashes: Vec<String>,
    pub relationship_limit_reached: bool,
}

pub struct GraphStore {
    pub(super) conn: Option<Reader>,
    pub generation: String,
    pub metadata: std::collections::BTreeMap<String, String>,
    pub database: Option<PathBuf>,
    has_kind: bool,
    has_name: bool,
    has_edge_kind: bool,
    has_lines: bool,
    has_test: bool,
    pub has_file_coverage: bool,
    unavailable: Option<String>,
    has_signature: bool,
    has_hash: bool,
    has_edge_line: bool,
    has_files: bool,
}

struct QueryScope<'a> {
    host: &'a Path,
    absolute: &'a str,
    relative: &'a str,
    symbol: Option<&'a str>,
}

impl GraphStore {
    /// Nested queries share the same absolute deadline and VM budget.
    pub fn operation(&self) -> Result<Option<Operation<'_>>> {
        self.conn.as_ref().map(Reader::operation).transpose()
    }

    pub fn verify_generation(&self) -> Result<()> {
        let _operation = self.operation()?;
        if let (Some(conn), Some(path)) = (&self.conn, &self.database) {
            conn.verify()?;
            ensure!(
                database_generation(path, &read_metadata(conn)?)? == self.generation,
                "graph changed during retrieval; retry against the current generation"
            );
            conn.verify()?;
        }
        Ok(())
    }
    pub fn database_path(host: &Path) -> PathBuf {
        let native = host.join(DEFAULT_DB);
        if std::fs::symlink_metadata(&native).map_or_else(
            |error| error.kind() != std::io::ErrorKind::NotFound,
            |_| true,
        ) {
            native
        } else {
            host.join(".code-review-graph/graph.db")
        }
    }

    pub fn file_unresolved(&self, host: &Path, relative: &str) -> Result<Option<bool>> {
        let _operation = self.operation()?;
        let sources = SourceRoot::open(host)?;
        let path = sources.normalize(relative)?;
        let relative = path.to_str().context("graph path must be UTF-8")?;
        if !self.has_file_coverage {
            return Ok(None);
        }
        let Some(conn) = &self.conn else {
            return Ok(None);
        };
        let absolute = sources.host().join(relative);
        // All absolute/relative aliases must explicitly store integer zero.
        // MAX over no rows is NULL; NULL/text/blob/nonzero rows are unknown.
        // The shared operation guard bounds scanning adversarial alias counts.
        let value=conn.query_row(
            "SELECT max(CASE WHEN typeof(unresolved)='integer' AND unresolved=0 THEN 0 ELSE 1 END) FROM files WHERE file_path IN (?1,?2)",
            params![absolute.to_str().context("graph path must be UTF-8")?, relative],
            |row| row.get::<_, Option<i64>>(0),
        ).context("read indexed file dependency coverage")?;
        Ok(value.map(|value| value != 0))
    }

    pub fn open(host: &Path, explicit_db: Option<&Path>) -> Result<Self> {
        let path = match explicit_db {
            Some(path) if path.is_absolute() => path.to_path_buf(),
            Some(path) => host.join(path),
            None => Self::database_path(host),
        };
        let Some(conn) = Reader::open(&path)? else {
            return Ok(Self::unavailable("unavailable (graph.db missing)"));
        };
        let path = conn.path().to_owned();
        let operation = conn.operation()?;
        let nodes = columns(&conn, "nodes")?;
        let edges = columns(&conn, "edges")?;
        let files = columns(&conn, "files")?;
        let missing_nodes = missing(&nodes, REQUIRED_NODES);
        let missing_edges = missing(&edges, REQUIRED_EDGES);
        if !missing_nodes.is_empty() || !missing_edges.is_empty() {
            return Ok(Self::unavailable(format!(
                "unavailable (graph schema missing nodes: {}; edges: {})",
                missing_nodes.join(","),
                missing_edges.join(",")
            )));
        }
        let metadata = read_metadata(&conn)?;
        let generation = database_generation(&path, &metadata)?;
        conn.verify()?;
        drop(operation);
        Ok(Self {
            generation,
            metadata,
            database: Some(path),
            has_kind: nodes.contains("kind"),
            has_name: nodes.contains("name"),
            has_edge_kind: edges.contains("kind"),
            has_lines: nodes.contains("line_start") && nodes.contains("line_end"),
            has_test: nodes.contains("is_test"),
            has_file_coverage: files.contains("file_path") && files.contains("unresolved"),
            conn: Some(conn),
            unavailable: None,
            has_signature: nodes.contains("signature"),
            has_hash: nodes.contains("file_hash"),
            has_edge_line: edges.contains("line"),
            has_files: files.contains("file_path") && files.contains("file_hash"),
        })
    }

    fn unavailable(status: impl Into<String>) -> Self {
        Self {
            conn: None,
            generation: "unavailable".to_owned(),
            metadata: Default::default(),
            database: None,
            has_kind: false,
            has_name: false,
            has_edge_kind: false,
            has_lines: false,
            has_test: false,
            has_file_coverage: false,
            unavailable: Some(status.into()),
            has_signature: false,
            has_hash: false,
            has_edge_line: false,
            has_files: false,
        }
    }

    pub fn project(
        &self,
        host: &Path,
        relative: &str,
        symbol: Option<&str>,
        max_edges: usize,
    ) -> Result<Projection> {
        ensure!(
            (1..=100).contains(&max_edges),
            "graph edge limit must be 1..100"
        );
        ensure!(
            symbol.is_none_or(|s| s.len() <= 512 && !s.contains('\0')),
            "invalid graph symbol"
        );
        let _operation = self.operation()?;
        let sources = SourceRoot::open(host)?;
        let host = sources.host();
        let relative_path = sources.normalize(relative)?;
        let relative = relative_path.to_str().context("graph path must be UTF-8")?;
        let Some(conn) = &self.conn else {
            return Ok(Projection {
                graph_status: self.unavailable.clone().unwrap_or_default(),
                symbols: Vec::new(),
                callers: Vec::new(),
                callees: Vec::new(),
                indexed_hashes: Vec::new(),
                relationship_limit_reached: false,
            });
        };
        let absolute = host.join(relative).to_string_lossy().to_string();
        let scope = QueryScope {
            host,
            absolute: &absolute,
            relative,
            symbol,
        };
        // The indexer can refresh nodes and edges concurrently. A read
        // transaction keeps all three SELECTs on one SQLite snapshot.
        let transaction = conn
            .unchecked_transaction()
            .context("begin read-only graph snapshot")?;
        let (symbols, mut indexed_hashes) = self.symbols(&transaction, &scope)?;
        if self.has_files && indexed_hashes.is_empty() {
            let file_hash = transaction.query_row(
                "SELECT file_hash FROM files WHERE file_path IN (?1,?2) LIMIT 1",
                params![scope.absolute, scope.relative],
                |row| row.get::<_, String>(0),
            );
            match file_hash {
                Ok(hash) => indexed_hashes.push(hash),
                Err(rusqlite::Error::QueryReturnedNoRows) => {}
                Err(error) => return Err(error).context("query indexed file hash"),
            }
        }
        let graph_status = if symbols.is_empty() && self.has_files && !indexed_hashes.is_empty() {
            "indexed (no matching symbols)"
        } else if symbols.is_empty() {
            "unindexed (file or symbol absent from graph)"
        } else {
            "unverified (source hashes not checked)"
        };
        if symbols.is_empty() {
            transaction.commit().context("finish graph snapshot")?;
            return Ok(Projection {
                graph_status: graph_status.to_string(),
                symbols,
                callers: Vec::new(),
                callees: Vec::new(),
                indexed_hashes,
                relationship_limit_reached: false,
            });
        }
        let (callers, more_callers) = self.relationships(&transaction, &scope, max_edges, true)?;
        let (callees, more_callees) = self.relationships(&transaction, &scope, max_edges, false)?;
        transaction.commit().context("finish graph snapshot")?;
        Ok(Projection {
            graph_status: graph_status.to_string(),
            symbols,
            callers,
            callees,
            indexed_hashes,
            relationship_limit_reached: more_callers || more_callees,
        })
    }

    fn symbol_sql(&self) -> String {
        let signature = if self.has_signature {
            "substr(signature,1,513)"
        } else {
            "NULL"
        };
        let hash = if self.has_hash {
            "substr(file_hash,1,129)"
        } else {
            "NULL"
        };
        let signature_nul = if self.has_signature {
            "OR instr(signature,char(0))>0"
        } else {
            ""
        };
        let hash_nul = if self.has_hash {
            "OR instr(file_hash,char(0))>0"
        } else {
            ""
        };
        let signature_long = if self.has_signature {
            "OR length(CAST(signature AS BLOB))>512"
        } else {
            ""
        };
        let hash_long = if self.has_hash {
            "OR length(CAST(file_hash AS BLOB))>128"
        } else {
            ""
        };
        let kind = if self.has_kind {
            "COALESCE(kind,'Function')"
        } else {
            "'Function'"
        };
        let name = if self.has_name {
            "name"
        } else {
            "qualified_name"
        };
        let start = if self.has_lines {
            "COALESCE(line_start,1)"
        } else {
            "1"
        };
        let end = if self.has_lines {
            "COALESCE(line_end,1)"
        } else {
            "1"
        };
        format!(
            "SELECT substr(qualified_name,1,513), substr({name},1,257), \
             substr({kind},1,65), {start}, {end}, {signature}, {hash}, \
             CASE WHEN instr(qualified_name,char(0))>0 \
               OR instr({name},char(0))>0 OR instr({kind},char(0))>0 \
               {signature_nul} {hash_nul} THEN 1 ELSE 0 END, \
             CASE WHEN length(CAST(qualified_name AS BLOB))>512 \
               OR length(CAST({name} AS BLOB))>256 \
               OR length(CAST({kind} AS BLOB))>64 \
               {signature_long} {hash_long} THEN 1 ELSE 0 END \
             FROM nodes WHERE file_path IN (?1,?2) AND (?3 IS NULL OR {name}=?3 \
               OR qualified_name=?3 OR qualified_name=file_path||'::'||?3) \
             AND lower({kind}) != 'file' \
             ORDER BY {start}, qualified_name LIMIT 41"
        )
    }

    fn symbols(
        &self,
        conn: &Connection,
        scope: &QueryScope<'_>,
    ) -> Result<(Vec<Symbol>, Vec<String>)> {
        let sql = self.symbol_sql();
        let mut stmt = conn.prepare(&sql).context("query graph symbols")?;
        let rows = stmt.query_map(
            params![scope.absolute, scope.relative, scope.symbol],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<i64>>(3)?.unwrap_or(0),
                    row.get::<_, Option<i64>>(4)?.unwrap_or(0),
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, i64>(8)?,
                ))
            },
        )?;
        let mut symbols = Vec::new();
        let mut hashes = Vec::new();
        for row in rows {
            let (
                qualified_name,
                name,
                kind,
                line_start,
                line_end,
                signature,
                hash,
                has_nul,
                too_long,
            ) = row?;
            ensure!(has_nul == 0, "graph index node metadata contains NUL bytes");
            ensure!(
                too_long == 0,
                "graph index node metadata exceeds field byte limit"
            );
            graph_text("qualified_name", &qualified_name)?;
            graph_text("name", &name)?;
            graph_text("kind", &kind)?;
            if let Some(value) = &signature {
                graph_text("signature", value)?;
            }
            if let Some(hash) = hash {
                graph_text("file_hash", &hash)?;
                if hashes.len() < 41 && !hashes.contains(&hash) {
                    hashes.push(hash);
                }
            }
            symbols.push(Symbol {
                qualified_name: short_path(scope.host, &qualified_name),
                name,
                kind,
                line_start,
                line_end,
                signature,
            });
        }
        Ok((symbols, hashes))
    }

    fn relationships(
        &self,
        conn: &Connection,
        scope: &QueryScope<'_>,
        max_edges: usize,
        inbound: bool,
    ) -> Result<(Vec<Relationship>, bool)> {
        let (owner, peer, owner_qualified, peer_qualified) = if inbound {
            ("target", "source", "target_qualified", "source_qualified")
        } else {
            ("source", "target", "source_qualified", "target_qualified")
        };
        let line = if !self.has_lines {
            "0"
        } else if inbound && self.has_edge_line {
            "COALESCE(NULLIF(edge.line,0), source.line_start, 0)"
        } else if inbound {
            "COALESCE(source.line_start, 0)"
        } else {
            "COALESCE(target.line_start, 0)"
        };
        let test_priority = if self.has_test {
            format!("COALESCE({peer}.is_test,0)")
        } else {
            "(0+0)".to_owned()
        };
        let kind = if self.has_edge_kind {
            "COALESCE(edge.kind,'UNKNOWN')"
        } else {
            "'CALLS'"
        };
        let name = if self.has_name {
            "name"
        } else {
            "qualified_name"
        };
        let sql = format!(
            "SELECT DISTINCT substr({peer}.qualified_name,1,513), \
             substr({kind},1,65), substr({peer}.file_path,1,513), {line}, \
             CASE WHEN instr({peer}.qualified_name,char(0))>0 \
               OR instr({kind},char(0))>0 \
               OR instr({peer}.file_path,char(0))>0 THEN 1 ELSE 0 END, \
             CASE WHEN length(CAST({peer}.qualified_name AS BLOB))>512 \
               OR length(CAST({kind} AS BLOB))>64 \
               OR length(CAST({peer}.file_path AS BLOB))>512 THEN 1 ELSE 0 END \
             FROM nodes AS {owner} \
             JOIN edges AS edge ON edge.{owner_qualified}={owner}.qualified_name \
             JOIN nodes AS {peer} ON {peer}.qualified_name=edge.{peer_qualified} \
             WHERE {owner}.file_path IN (?1,?2) AND (?3 IS NULL OR {owner}.{name}=?3 \
               OR {owner}.qualified_name=?3 OR {owner}.qualified_name={owner}.file_path||'::'||?3) \
             AND lower({kind}) != 'contains' \
             ORDER BY {test_priority} DESC, {peer}.qualified_name, {kind}, {peer}.file_path \
             LIMIT ?4"
        );
        let mut stmt = conn.prepare(&sql).context("query graph relationships")?;
        let rows = stmt.query_map(
            params![
                scope.absolute,
                scope.relative,
                scope.symbol,
                (max_edges + 1) as i64
            ],
            |row| {
                Ok((
                    Relationship {
                        qualified_name: short_path(scope.host, &row.get::<_, String>(0)?),
                        kind: row.get(1)?,
                        file_path: short_path(scope.host, &row.get::<_, String>(2)?),
                        line: row.get::<_, Option<i64>>(3)?.unwrap_or(0),
                    },
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            },
        )?;
        let mut rows = rows
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("read graph relationships")?;
        for (row, has_nul, too_long) in &rows {
            ensure!(
                *has_nul == 0,
                "graph index edge metadata contains NUL bytes"
            );
            ensure!(
                *too_long == 0,
                "graph index edge metadata exceeds field byte limit"
            );
            graph_text("edge qualified_name", &row.qualified_name)?;
            graph_text("edge kind", &row.kind)?;
            graph_text("edge file_path", &row.file_path)?;
        }
        let limited = rows.len() > max_edges;
        rows.truncate(max_edges);
        Ok((rows.into_iter().map(|(row, _, _)| row).collect(), limited))
    }
}

pub(super) fn columns(conn: &Connection, table: &str) -> Result<HashSet<String>> {
    let sql = match table {
        "nodes" => "PRAGMA table_info(nodes)",
        "edges" => "PRAGMA table_info(edges)",
        "files" => "PRAGMA table_info(files)",
        "metadata" => "PRAGMA table_info(metadata)",
        "index_meta" => "PRAGMA table_info(index_meta)",
        _ => unreachable!(),
    };
    let mut schema = conn.prepare(
        "SELECT type,upper(substr(ltrim(sql),1,13)) FROM sqlite_schema WHERE name=?1 COLLATE NOCASE LIMIT 1",
    )?;
    let kind = schema.query_row([table], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
    });
    match kind {
        Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(HashSet::new()),
        Ok((kind, Some(prefix))) if kind == "table" && prefix == "CREATE TABLE " => {}
        Ok(_) => anyhow::bail!("graph {table} must be an ordinary table"),
        Err(error) => return Err(error).context("inspect graph table kind"),
    }
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map([], |row| {
        let name = row.get_ref(1)?.as_str()?;
        if name.len() > 128 || name.contains('\0') {
            return Err(rusqlite::Error::InvalidQuery);
        }
        Ok(name.to_owned())
    })?;
    rows.collect::<rusqlite::Result<HashSet<_>>>()
        .with_context(|| format!("read {table} schema"))
}

fn missing(columns: &HashSet<String>, required: &[&str]) -> Vec<String> {
    required
        .iter()
        .filter(|name| !columns.contains(**name))
        .map(|name| (*name).to_string())
        .collect()
}

fn short_path(host: &Path, value: &str) -> String {
    let absolute = PathBuf::from(value);
    absolute
        .strip_prefix(host)
        .map(|relative| relative.to_string_lossy().into_owned())
        .unwrap_or_else(|_| value.to_string())
}

fn graph_text(field: &str, value: &str) -> Result<()> {
    ensure!(
        !value.contains('\0'),
        "graph index {field} contains NUL bytes"
    );
    Ok(())
}

pub fn valid_hash(hash: &str) -> bool {
    hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
}

pub fn current_hash(host: &Path, relative: &str) -> Result<Option<String>> {
    SourceRoot::open(host)?.hash(relative)
}

fn read_metadata(conn: &Connection) -> Result<BTreeMap<String, String>> {
    let mut metadata = BTreeMap::new();
    for table in ["metadata", "index_meta"] {
        let fields = columns(conn, table)?;
        if fields.contains("key") && fields.contains("value") {
            let mut stmt = conn.prepare(&format!("SELECT key,value FROM {table} LIMIT 64"))?;
            for row in stmt.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })? {
                let (key, value) = row?;
                ensure!(
                    key.len() <= 128 && value.len() <= 4096,
                    "graph metadata exceeds byte budget"
                );
                metadata.insert(key, value);
            }
        }
    }
    Ok(metadata)
}

fn database_generation(path: &Path, metadata: &BTreeMap<String, String>) -> Result<String> {
    let stat = std::fs::symlink_metadata(path)?;
    ensure!(
        stat.is_file(),
        "graph database generation is not a regular file"
    );
    let modified = stat.modified()?.duration_since(std::time::UNIX_EPOCH)?;
    let mut wal_path = path.as_os_str().to_owned();
    wal_path.push("-wal");
    let wal = match std::fs::symlink_metadata(wal_path) {
        Ok(stat) => {
            ensure!(stat.is_file(), "graph WAL generation is not a regular file");
            Some((
                stat.dev(),
                stat.ino(),
                stat.len(),
                stat.mtime(),
                stat.mtime_nsec(),
                stat.ctime(),
                stat.ctime_nsec(),
            ))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error).context("stat external graph WAL generation"),
    };
    let stamp = format!(
        "{}:{}:{}:{}:{}:{}:{:?}:{wal:?}",
        stat.dev(),
        stat.ino(),
        stat.ctime(),
        stat.ctime_nsec(),
        stat.len(),
        modified.as_nanos(),
        metadata
    );
    Ok(format!("{:x}", Sha256::digest(stamp.as_bytes())))
}
