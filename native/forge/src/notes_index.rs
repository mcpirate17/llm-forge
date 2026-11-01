//! Bounded, transactional prose and table indexing for host notes and task docs.
mod db;
mod extract;
mod ingest;
mod tables;
#[cfg(test)]
mod tests;

use anyhow::{Context, Result};
use clap::Args;
#[cfg(test)]
use extract::extract_tables;
use regex::Regex;
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
#[cfg(test)]
use std::time::{SystemTime, UNIX_EPOCH};
pub use tables::{table_notes, TableHit, TablesArgs};

fn search_term_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\S+").unwrap())
}

/// Default vault root: `~/Documents/CodexVault`, matching Python's
/// `VAULT_ROOT = os.path.expanduser("~/Documents/CodexVault")` exactly.
/// `obsidian_sync::vault_root` joins an extra `claude/` child for the hook
/// mirror's own tree, a different shape than the notes vault, so it is not
/// reused here.
pub fn default_vault_root() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join("Documents").join("CodexVault")
}

const NOTES_FTS_DDL: &str = "CREATE VIRTUAL TABLE IF NOT EXISTS notes_fts USING fts5(
            path UNINDEXED, source UNINDEXED, title, body, mtime UNINDEXED
        )";
const NOTE_TABLES_DDL: &str = "CREATE TABLE IF NOT EXISTS note_tables (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            source TEXT NOT NULL,
            path TEXT NOT NULL,
            note TEXT NOT NULL,
            table_idx INTEGER NOT NULL,
            section_heading TEXT,
            n_cols INTEGER,
            n_rows INTEGER,
            headers_json TEXT NOT NULL,
            rows_json TEXT NOT NULL,
            ingested_at REAL NOT NULL
        )";

fn ddl(conn: &Connection) -> Result<()> {
    conn.execute_batch(NOTES_FTS_DDL)
        .context("notes_index: DDL failed (is FTS5 compiled into rusqlite?)")?;
    conn.execute_batch(NOTE_TABLES_DDL)?;
    db::validate_schema(conn)?;
    conn.execute_batch("CREATE INDEX IF NOT EXISTS idx_note_tables_note ON note_tables(note);")?;
    Ok(())
}

/// Rebuild in one SQLite transaction; errors retain all previous index rows.
pub fn rebuild(conn: &mut Connection, host: &Path, vault: Option<&Path>) -> Result<(u64, u64)> {
    rebuild_checked(conn, host, vault, || Ok(()))
}

pub(super) fn rebuild_checked(
    conn: &mut Connection,
    host: &Path,
    vault: Option<&Path>,
    before_commit: impl FnOnce() -> Result<()>,
) -> Result<(u64, u64)> {
    ingest::rebuild(conn, host, vault, ingest::Limits::default(), before_commit)
}

/// Port of `index_notes._fts_match_query`: every whitespace-separated term
/// (stripped of a surrounding `"`) quoted and doubled-quote-escaped, joined
/// with a space (FTS5's implicit AND).
fn fts_match_query(query: &str) -> String {
    search_term_re()
        .find_iter(query)
        .map(|m| m.as_str().trim_matches('"'))
        .filter(|t| !t.is_empty())
        .map(|t| format!("\"{}\"", t.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" ")
}

pub struct SearchHit {
    pub path: String,
    pub title: String,
    pub snippet: String,
}

/// Port of `index_notes.search_notes`.
pub fn search_notes(conn: &Connection, query: &str, limit: u32) -> Result<Vec<SearchHit>> {
    search_filtered(conn, query, limit, None)
}

pub(crate) fn search_filtered(
    conn: &Connection,
    query: &str,
    limit: u32,
    source: Option<&str>,
) -> Result<Vec<SearchHit>> {
    validate_search(query, limit)?;
    let match_query = fts_match_query(query);
    if match_query.is_empty() {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare(
        "SELECT substr(path,1,1024), substr(title,1,240), substr(snippet(notes_fts, 3, '[', ']', ' … ', 12),1,480) AS snip
           FROM notes_fts
          WHERE notes_fts MATCH ?1 AND (?3 IS NULL OR source=?3)
          ORDER BY rank, rowid LIMIT ?2",
    )?;
    let rows = stmt
        .query_map(rusqlite::params![match_query, limit, source], |row| {
            Ok(SearchHit {
                path: row.get(0)?,
                title: row.get(1)?,
                snippet: row.get(2)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

fn validate_search(query: &str, limit: u32) -> Result<()> {
    anyhow::ensure!((1..=1000).contains(&limit), "limit must be in 1..1000");
    anyhow::ensure!(query.len() <= 4096, "notes query exceeds 4096 bytes");
    Ok(())
}

/// Existing bounded read-only index; missing and corrupt indexes stay distinct.
pub(crate) fn open_search(path: &Path) -> Result<Option<Connection>> {
    db::open_search(path)
}

/// The prose-search index, defaulting to `HOST/research/notes.db`.
///
/// Kept byte-identical to the Python reference's `notes_db_path` default:
/// both hardcoded `runs.db` until 2026-09-16, so after the monorepo split
/// the index landed in the run database while every documented reader
/// looked in `notes.db`. A host that keeps the two together passes `--db`.
fn resolve_db_path(host: &Path, db: Option<&Path>) -> Result<PathBuf> {
    match db {
        Some(path) => Ok(path.to_owned()),
        None => conductor_native::project_paths::notes_db(host).map_err(anyhow::Error::msg),
    }
}

#[derive(Args)]
pub struct IndexArgs {
    /// Host repo root (holds `research/notes`, `tasks`, and the db).
    #[arg(long)]
    pub host: PathBuf,
    /// Obsidian vault root; defaults to `~/Documents/CodexVault`, indexed
    /// only when its `research/` child exists.
    #[arg(long)]
    pub vault: Option<PathBuf>,
    /// Overrides the default `HOST/research/notes.db`.
    #[arg(long)]
    pub db: Option<PathBuf>,
}

#[derive(Args)]
pub struct SearchArgs {
    #[arg(long)]
    pub host: PathBuf,
    pub query: Vec<String>,
    #[arg(long, default_value_t = 20)]
    pub limit: u32,
    #[arg(long)]
    pub db: Option<PathBuf>,
    #[arg(long)]
    pub json: bool,
}

/// `forge notes index`: same "notes database not found" refusal as the Python CLI
/// -- this command does not create the db file itself, only opens it.
pub fn run_index(args: &IndexArgs) -> Result<u8> {
    let db_path = resolve_db_path(&args.host, args.db.as_deref())?;
    let vault = args.vault.clone().unwrap_or_else(default_vault_root);
    let (n_files, n_tables) = db::Writer::open(&db_path)?.rebuild(&args.host, Some(&vault))?;
    println!(
        "indexed {n_files} notes, {n_tables} tables into {}",
        db_path.display()
    );
    Ok(0)
}

/// `forge notes search`.
pub fn run_search(args: &SearchArgs) -> Result<u8> {
    let db_path = resolve_db_path(&args.host, args.db.as_deref())?;
    let conn = open_search(&db_path)?
        .with_context(|| format!("notes database not found at {}", db_path.display()))?;
    let query = args.query.join(" ");
    validate_search(&query, args.limit)?;
    if fts_match_query(&query).is_empty() {
        if args.json {
            println!("[]");
        } else {
            println!("empty search query");
        }
        return Ok(0);
    }
    let hits = search_notes(&conn, &query, args.limit)?;
    if args.json {
        let payload: Vec<_> = hits
            .iter()
            .map(|h| serde_json::json!({"path": h.path, "title": h.title, "snippet": h.snippet}))
            .collect();
        println!("{}", serde_json::to_string(&payload)?);
        return Ok(0);
    }
    if hits.is_empty() {
        println!("no matches for: {query}");
        return Ok(0);
    }
    for h in hits {
        println!("\n# {}\n  {}\n  {}", h.title, h.path, h.snippet);
    }
    Ok(0)
}

#[derive(clap::Subcommand)]
pub enum NotesCommand {
    /// Rebuild `notes_fts` + `note_tables` from the host's notes/tasks and
    /// (when present) the Obsidian vault.
    Index(IndexArgs),
    /// FTS5 search over an already-built index.
    Search(SearchArgs),
    /// List structured Markdown tables in an already-built index.
    Tables(TablesArgs),
}

pub fn run(action: NotesCommand) -> Result<u8> {
    match action {
        NotesCommand::Index(args) => run_index(&args),
        NotesCommand::Search(args) => run_search(&args),
        NotesCommand::Tables(args) => tables::run(&args),
    }
}
