//! Read-only, bounded queries against a Forge or external structural index.

use anyhow::{ensure, Context, Result};
use rusqlite::{params, Connection, OpenFlags};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

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
    pub(super) conn: Option<Connection>,
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
    pub fn verify_generation(&self) -> Result<()> {
        if let (Some(conn), Some(path)) = (&self.conn, &self.database) {
            ensure!(
                database_generation(path, &read_metadata(conn)?)? == self.generation,
                "graph changed during retrieval; retry against the current generation"
            );
        }
        Ok(())
    }
    pub fn database_path(host: &Path) -> PathBuf {
        let native = host.join(DEFAULT_DB);
        if native.is_file() {
            native
        } else {
            host.join(".code-review-graph/graph.db")
        }
    }

    pub fn indexed_hash(&self, host: &Path, relative: &str) -> Result<Option<String>> {
        let Some(conn) = &self.conn else {
            return Ok(None);
        };
        let absolute = host.join(relative).to_string_lossy().to_string();
        let query = if self.has_files {
            "SELECT file_hash FROM files WHERE file_path IN (?1,?2) LIMIT 1"
        } else if self.has_hash {
            "SELECT file_hash FROM nodes WHERE file_path IN (?1,?2) LIMIT 1"
        } else {
            return Ok(None);
        };
        match conn.query_row(query, params![absolute, relative], |row| {
            row.get::<_, Option<String>>(0)
        }) {
            Ok(Some(hash)) if valid_hash(&hash) => Ok(Some(hash)),
            Ok(_) | Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(error) => Err(error).context("read indexed source hash"),
        }
    }

    pub fn file_unresolved(&self, host: &Path, relative: &str) -> Result<Option<bool>> {
        if !self.has_file_coverage {
            return Ok(None);
        }
        let Some(conn) = &self.conn else {
            return Ok(None);
        };
        let absolute = host.join(relative).to_string_lossy().to_string();
        match conn.query_row(
            "SELECT unresolved FROM files WHERE file_path IN (?1,?2) LIMIT 1",
            params![absolute, relative],
            |row| row.get::<_, i64>(0),
        ) {
            Ok(value) => Ok(Some(value != 0)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(error) => Err(error).context("read indexed file dependency coverage"),
        }
    }

    /// Validate every bounded endpoint, including deleted and dirty callers.
    pub fn validate_projection(
        &self,
        host: &Path,
        relative: &str,
        projection: &mut Projection,
    ) -> Result<()> {
        if let Some(indexed) = self.indexed_hash(host, relative)? {
            if current_hash(host, relative)?.as_deref() != Some(&indexed) {
                projection.graph_status = "stale (indexed source hash differs)".to_owned();
                projection.symbols.clear();
                projection.callers.clear();
                projection.callees.clear();
                return Ok(());
            }
        }
        let mut invalid = HashSet::new();
        for row in projection.callers.iter().chain(&projection.callees) {
            if let Some(indexed) = self.indexed_hash(host, &row.file_path)? {
                if current_hash(host, &row.file_path)?.as_deref() != Some(&indexed) {
                    invalid.insert(row.file_path.clone());
                }
            }
        }
        if !invalid.is_empty() {
            projection
                .callers
                .retain(|row| !invalid.contains(&row.file_path));
            projection
                .callees
                .retain(|row| !invalid.contains(&row.file_path));
            projection.graph_status = "stale (relationship endpoint differs)".to_owned();
            projection.relationship_limit_reached = true;
        }
        Ok(())
    }

    pub fn open(host: &Path, explicit_db: Option<&Path>) -> Result<Self> {
        let path = match explicit_db {
            Some(path) if path.is_absolute() => path.to_path_buf(),
            Some(path) => host.join(path),
            None if host.join(DEFAULT_DB).is_file() => host.join(DEFAULT_DB),
            None => host.join(".code-review-graph/graph.db"),
        };
        if !path.is_file() {
            return Ok(Self::unavailable("unavailable (graph.db missing)"));
        }
        let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
        let conn = Connection::open_with_flags(&path, flags)
            .with_context(|| format!("open graph index read-only: {}", path.display()))?;
        conn.busy_timeout(Duration::from_secs(2))?;
        conn.execute_batch("PRAGMA query_only=ON")?;
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
            "ok"
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
             FROM nodes WHERE file_path IN (?1,?2) AND (?3 IS NULL OR {name}=?3) \
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
             WHERE {owner}.file_path IN (?1,?2) AND (?3 IS NULL OR {owner}.{name}=?3) \
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
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
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
        .map(|relative| relative.to_string_lossy().replace('\\', "/"))
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
    let path = host.join(relative);
    let canonical = match path.canonicalize() {
        Ok(path) => path,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("resolve graph endpoint"),
    };
    ensure!(
        canonical.starts_with(host.canonicalize()?),
        "graph endpoint escapes host: {relative}"
    );
    let size = canonical.metadata()?.len();
    ensure!(
        size <= 1 << 20,
        "graph endpoint exceeds source byte budget: {relative}"
    );
    let bytes = std::fs::read(&canonical)?;
    ensure!(
        bytes.len() <= 1 << 20,
        "graph endpoint grew beyond source byte budget: {relative}"
    );
    Ok(Some(format!("{:x}", Sha256::digest(bytes))))
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
    let stat = path.metadata()?;
    let modified = stat.modified()?.duration_since(std::time::UNIX_EPOCH)?;
    let mut wal_path = path.as_os_str().to_owned();
    wal_path.push("-wal");
    let wal = match std::fs::metadata(wal_path) {
        Ok(stat) => Some((
            stat.len(),
            stat.modified()?
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos(),
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error).context("stat external graph WAL generation"),
    };
    let stamp = format!(
        "{}:{}:{:?}:{wal:?}",
        stat.len(),
        modified.as_nanos(),
        metadata
    );
    Ok(format!("{:x}", Sha256::digest(stamp.as_bytes())))
}
