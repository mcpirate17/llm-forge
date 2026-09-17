//! `forge notes index|search`: a native port of `conductor.index_notes`'s
//! FTS5 note index. Byte-identical schema and extraction rules to the
//! Python (see `src/conductor/index_notes.py`), which stays the reference
//! implementation. Sources: the host's `research/notes/**.md` and
//! `tasks/**.md` always, plus the Obsidian vault's `research/`,
//! `dashboards/`, `runbooks/` trees additionally when present -- neither
//! tree is a superset of the other, so both are always indexed when both
//! exist (the bug this whole slice exists to fix).

use anyhow::{bail, Context, Result};
use clap::Args;
use regex::Regex;
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

const EXCLUDED_REL_PREFIXES: &[&str] = &["tasks/audit/"];

fn sep_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\s*\|?[\s:|-]*-[\s:|-]*\|?\s*$").unwrap())
}

fn heading_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\s{0,3}(#{1,6})\s+(.*?)\s*#*\s*$").unwrap())
}

fn search_term_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\S+").unwrap())
}

/// A `serde_json::ser::Formatter` matching Python's `json.dumps` default
/// separators (`", "` between array/object items, `": "` after an object
/// key) instead of serde_json's compact `","`/`":"` -- `headers_json` and
/// `rows_json` must be byte-identical to the Python reference, and a bare
/// `serde_json::to_string` disagrees on every comma.
struct PySeparatorFormatter;

impl serde_json::ser::Formatter for PySeparatorFormatter {
    fn begin_array_value<W: ?Sized + std::io::Write>(
        &mut self,
        writer: &mut W,
        first: bool,
    ) -> std::io::Result<()> {
        if first {
            Ok(())
        } else {
            writer.write_all(b", ")
        }
    }

    fn begin_object_value<W: ?Sized + std::io::Write>(
        &mut self,
        writer: &mut W,
    ) -> std::io::Result<()> {
        writer.write_all(b": ")
    }
}

fn to_python_json<T: serde::Serialize>(value: &T) -> Result<String> {
    let mut buf = Vec::new();
    let mut ser = serde_json::Serializer::with_formatter(&mut buf, PySeparatorFormatter);
    value.serialize(&mut ser)?;
    Ok(String::from_utf8(buf)?)
}

/// One markdown table extracted from a note, `serde_json`-encoded the same
/// shape the Python `note_tables.headers_json`/`rows_json` columns hold.
struct Table {
    section: String,
    table_idx: i64,
    headers: Vec<String>,
    rows: Vec<Vec<String>>,
}

fn clean_cell(cell: &str) -> String {
    cell.trim()
        .trim_matches('`')
        .replace("**", "")
        .trim()
        .to_string()
}

fn split_row(line: &str) -> Vec<String> {
    let mut s = line.trim();
    if let Some(rest) = s.strip_prefix('|') {
        s = rest;
    }
    if let Some(rest) = s.strip_suffix('|') {
        s = rest;
    }
    s.split('|').map(clean_cell).collect()
}

fn is_table_row(line: &str) -> bool {
    line.trim_start().starts_with('|')
}

/// Port of `index_notes.extract_tables`: header row, separator row (with a
/// literal `-`), then zero or more data rows, tracking the last heading seen
/// as `section`.
fn extract_tables(text: &str) -> Vec<Table> {
    let lines: Vec<&str> = text.lines().collect();
    let mut tables = Vec::new();
    let mut section = String::new();
    let mut i = 0usize;
    let mut tidx = 0i64;
    while i < lines.len() {
        if let Some(caps) = heading_re().captures(lines[i]) {
            section = caps.get(2).unwrap().as_str().trim().to_string();
            i += 1;
            continue;
        }
        if is_table_row(lines[i])
            && i + 1 < lines.len()
            && sep_re().is_match(lines[i + 1])
            && lines[i + 1].contains('-')
        {
            let headers = split_row(lines[i]);
            let mut j = i + 2;
            let mut rows = Vec::new();
            while j < lines.len() && is_table_row(lines[j]) {
                rows.push(split_row(lines[j]));
                j += 1;
            }
            tables.push(Table {
                section: section.clone(),
                table_idx: tidx,
                headers,
                rows,
            });
            tidx += 1;
            i = j;
            continue;
        }
        i += 1;
    }
    tables
}

fn title_of(text: &str, path: &Path) -> String {
    for line in text.lines() {
        if let Some(caps) = heading_re().captures(line) {
            return caps.get(2).unwrap().as_str().trim().to_string();
        }
    }
    path.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default()
}

fn should_index_path(rel: &str) -> bool {
    let normalized = rel.replace('\\', "/");
    !EXCLUDED_REL_PREFIXES
        .iter()
        .any(|p| normalized.starts_with(p))
}

/// One `(source tag, root dir)` pair `rebuild` walks for `**/*.md`.
struct Source {
    tag: &'static str,
    root: PathBuf,
}

/// Mirrors `index_notes._source_roots()`: the host's own notes + tasks
/// always, the vault trees additionally when `vault/research` is a dir.
fn source_roots(host: &Path, vault: Option<&Path>) -> Vec<Source> {
    let mut roots = vec![
        Source {
            tag: "notes",
            root: host.join("research").join("notes"),
        },
        Source {
            tag: "tasks",
            root: host.join("tasks"),
        },
    ];
    if let Some(vault) = vault {
        let vault_research = vault.join("research");
        if vault_research.is_dir() {
            roots.push(Source {
                tag: "vault_research",
                root: vault_research,
            });
            roots.push(Source {
                tag: "vault_dashboards",
                root: vault.join("dashboards"),
            });
            roots.push(Source {
                tag: "vault_runbooks",
                root: vault.join("runbooks"),
            });
        }
    }
    roots
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

fn ddl(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE VIRTUAL TABLE IF NOT EXISTS notes_fts USING fts5(
            path UNINDEXED, source UNINDEXED, title, body, mtime UNINDEXED
        );
        CREATE TABLE IF NOT EXISTS note_tables (
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
        );
        CREATE INDEX IF NOT EXISTS idx_note_tables_note ON note_tables(note);",
    )
    .context("notes_index: DDL failed (is FTS5 compiled into rusqlite?)")?;
    Ok(())
}

/// Walks `sources`, rel-pathed against `host` (repo-rooted sources) or
/// `vault` (vault-rooted sources) exactly like the Python
/// `os.path.relpath(path, REPO) if path.startswith(REPO) else
/// os.path.relpath(path, VAULT_ROOT)`. Full idempotent rebuild: `DELETE`
/// both tables, then re-insert inside one transaction.
pub fn rebuild(conn: &mut Connection, host: &Path, vault: Option<&Path>) -> Result<(u64, u64)> {
    ddl(conn)?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs_f64();
    let tx = conn.transaction()?;
    tx.execute("DELETE FROM notes_fts", [])?;
    tx.execute("DELETE FROM note_tables", [])?;
    let mut n_files = 0u64;
    let mut n_tables = 0u64;
    for source in source_roots(host, vault) {
        if !source.root.is_dir() {
            continue;
        }
        let mut paths: Vec<PathBuf> = walkdir::WalkDir::new(&source.root)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "md"))
            .map(|e| e.path().to_path_buf())
            .collect();
        paths.sort();
        for path in paths {
            let rel = if path.starts_with(host) {
                path.strip_prefix(host).unwrap().to_path_buf()
            } else if let Some(vault) = vault {
                path.strip_prefix(vault).unwrap_or(&path).to_path_buf()
            } else {
                path.clone()
            };
            let rel_str = rel.to_string_lossy().to_string();
            if !should_index_path(&rel_str) {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            let note = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            let mtime = std::fs::metadata(&path)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs_f64())
                .unwrap_or(0.0);
            tx.execute(
                "INSERT INTO notes_fts (path, source, title, body, mtime) VALUES (?1,?2,?3,?4,?5)",
                rusqlite::params![rel_str, source.tag, title_of(&text, &path), text, mtime],
            )?;
            n_files += 1;
            for t in extract_tables(&text) {
                tx.execute(
                    "INSERT INTO note_tables
                       (source, path, note, table_idx, section_heading, n_cols,
                        n_rows, headers_json, rows_json, ingested_at)
                       VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
                    rusqlite::params![
                        source.tag,
                        rel_str,
                        note,
                        t.table_idx,
                        t.section,
                        t.headers.len() as i64,
                        t.rows.len() as i64,
                        to_python_json(&t.headers)?,
                        to_python_json(&t.rows)?,
                        now,
                    ],
                )?;
                n_tables += 1;
            }
        }
    }
    tx.commit()?;
    Ok((n_files, n_tables))
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
    if limit < 1 {
        bail!("limit must be positive");
    }
    let match_query = fts_match_query(query);
    if match_query.is_empty() {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare(
        "SELECT path, title, snippet(notes_fts, 3, '[', ']', ' … ', 12) AS snip
           FROM notes_fts
          WHERE notes_fts MATCH ?1
          ORDER BY rank LIMIT ?2",
    )?;
    let rows = stmt
        .query_map(rusqlite::params![match_query, limit], |row| {
            Ok(SearchHit {
                path: row.get(0)?,
                title: row.get(1)?,
                snippet: row.get(2)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// The prose-search index, defaulting to `HOST/research/notes.db`.
///
/// Kept byte-identical to the Python reference's `notes_db_path` default:
/// both hardcoded `runs.db` until 2026-09-16, so after the monorepo split
/// the index landed in the run database while every documented reader
/// looked in `notes.db`. A host that keeps the two together passes `--db`.
fn resolve_db_path(host: &Path, db: Option<&Path>) -> PathBuf {
    db.map(PathBuf::from)
        .unwrap_or_else(|| host.join("research").join("notes.db"))
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
    let db_path = resolve_db_path(&args.host, args.db.as_deref());
    if !db_path.exists() {
        bail!("notes database not found at {}", db_path.display());
    }
    let vault = args.vault.clone().unwrap_or_else(default_vault_root);
    let mut conn =
        Connection::open(&db_path).with_context(|| format!("opening {}", db_path.display()))?;
    let (n_files, n_tables) = rebuild(&mut conn, &args.host, Some(&vault))?;
    println!(
        "indexed {n_files} notes, {n_tables} tables into {}",
        db_path.display()
    );
    Ok(0)
}

/// `forge notes search`.
pub fn run_search(args: &SearchArgs) -> Result<u8> {
    let db_path = resolve_db_path(&args.host, args.db.as_deref());
    if !db_path.exists() {
        bail!("notes database not found at {}", db_path.display());
    }
    let conn =
        Connection::open(&db_path).with_context(|| format!("opening {}", db_path.display()))?;
    let query = args.query.join(" ");
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
}

pub fn run(action: NotesCommand) -> Result<u8> {
    match action {
        NotesCommand::Index(args) => run_index(&args),
        NotesCommand::Search(args) => run_search(&args),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn fts5_is_available() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE VIRTUAL TABLE t USING fts5(body)")
            .expect("rusqlite must be built with an FTS5-enabled sqlite3");
    }

    #[test]
    fn extract_tables_parses_header_sep_rows() {
        let text = "# Sec\n\n| a | b |\n|---|---|\n| 1 | 2 |\n";
        let tables = extract_tables(text);
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].section, "Sec");
        assert_eq!(tables[0].headers, vec!["a", "b"]);
        assert_eq!(tables[0].rows, vec![vec!["1".to_string(), "2".to_string()]]);
    }

    #[test]
    fn rebuild_indexes_repo_and_vault_together() {
        let tmp = tempfile_dir();
        let host = tmp.join("repo");
        let vault = tmp.join("vault");
        write(
            &host.join("research/notes/alpha.md"),
            "# Alpha\n\ntfwd marker\n",
        );
        write(&host.join("tasks/beta.md"), "# Beta\n\nbeta body\n");
        write(&vault.join("research/gamma.md"), "# Gamma\n\nvault body\n");

        let mut conn = Connection::open_in_memory().unwrap();
        let (n_files, _n_tables) = rebuild(&mut conn, &host, Some(&vault)).unwrap();
        assert_eq!(n_files, 3);
        let mut stmt = conn
            .prepare("SELECT DISTINCT source FROM notes_fts ORDER BY source")
            .unwrap();
        let sources: Vec<String> = stmt
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        assert_eq!(sources, vec!["notes", "tasks", "vault_research"]);

        let hits = search_notes(&conn, "tfwd", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].path.ends_with("alpha.md"));
    }

    #[test]
    fn default_db_is_the_notes_database_not_the_run_database() {
        // Parity pin with the Python reference's `DEFAULT_NOTES_DB`. Both sides
        // hardcoded `runs.db` until 2026-09-16, which sent the prose index to the
        // run database on any host that had split the two.
        let host = Path::new("/srv/host");
        assert_eq!(
            resolve_db_path(host, None),
            PathBuf::from("/srv/host/research/notes.db")
        );
    }

    #[test]
    fn an_explicit_db_flag_wins_over_the_default() {
        let host = Path::new("/srv/host");
        let chosen = PathBuf::from("/tmp/other.db");
        assert_eq!(resolve_db_path(host, Some(&chosen)), chosen);
    }

    fn tempfile_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "forge-notes-index-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}
