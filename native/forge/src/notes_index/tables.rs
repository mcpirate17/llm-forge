//! Bounded structured-table listing over an existing notes database.
use super::{open_search, resolve_db_path};
use anyhow::{ensure, Context, Result};
use clap::Args;
use rusqlite::Connection;
use serde::Serialize;
use std::fmt::Write;
use std::path::PathBuf;

const MAX_OUTPUT: usize = 1024 * 1024;

#[derive(Debug, Serialize)]
pub struct TableHit {
    pub note: String,
    pub table_idx: i64,
    pub section_heading: String,
    pub n_cols: i64,
    pub n_rows: i64,
    pub headers: Vec<String>,
}

#[derive(Args)]
pub struct TablesArgs {
    #[arg(long)]
    pub host: PathBuf,
    /// Basename LIKE filter; an empty filter lists every matching table.
    pub filter: Vec<String>,
    #[arg(long)]
    pub db: Option<PathBuf>,
    #[arg(long, default_value_t = 20)]
    pub limit: u32,
    #[arg(long)]
    pub json: bool,
}

pub fn table_notes(conn: &Connection, filter: &str, limit: u32) -> Result<Vec<TableHit>> {
    ensure!(
        filter.len() <= 4096 && !filter.contains('\0'),
        "notes table filter must be at most 4096 bytes without NUL"
    );
    ensure!((1..=1000).contains(&limit), "limit must be in 1..1000");
    let mut statement = conn.prepare("SELECT note,table_idx,section_heading,n_cols,n_rows,headers_json FROM note_tables WHERE note LIKE ?1 ORDER BY note,table_idx,id LIMIT ?2")?;
    let mut rows = statement.query(rusqlite::params![format!("%{filter}%"), limit])?;
    let mut hits = Vec::new();
    let mut bytes = 0usize;
    while let Some(row) = rows.next()? {
        let hit = table_hit(row)?;
        bytes = bytes
            .checked_add(serde_json::to_vec(&hit)?.len() + 1)
            .context("notes table output overflow")?;
        ensure!(
            bytes <= MAX_OUTPUT - 2,
            "notes table output exceeds 1 MiB; narrow the filter or limit"
        );
        hits.push(hit);
    }
    Ok(hits)
}

fn table_hit(row: &rusqlite::Row<'_>) -> Result<TableHit> {
    let headers_json: String = row.get(5)?;
    ensure!(
        headers_json.len() <= MAX_OUTPUT,
        "notes table headers exceed output limit"
    );
    let headers: Vec<String> =
        serde_json::from_str(&headers_json).context("invalid notes table headers")?;
    let hit = TableHit {
        note: row.get(0)?,
        table_idx: row.get(1)?,
        section_heading: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
        n_cols: row.get(3)?,
        n_rows: row.get(4)?,
        headers,
    };
    ensure!(
        hit.table_idx >= 0
            && hit.n_rows >= 0
            && hit.n_cols >= 0
            && hit.n_cols as usize == hit.headers.len(),
        "invalid notes table dimensions"
    );
    Ok(hit)
}

// Match Python's repr quote choice and control escaping for the legacy CLI line.
fn section_repr(text: &str) -> String {
    let quote = if text.contains('\'') && !text.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut result = String::from(quote);
    for ch in text.chars() {
        match ch {
            '\\' => result.push_str("\\\\"),
            '\n' => result.push_str("\\n"),
            '\r' => result.push_str("\\r"),
            '\t' => result.push_str("\\t"),
            ch if ch == quote => {
                result.push('\\');
                result.push(ch);
            }
            ch if ch.is_control() => {
                if ch as u32 <= 0xff {
                    write!(result, "\\x{:02x}", ch as u32).expect("write string");
                } else {
                    write!(result, "\\u{:04x}", ch as u32).expect("write string");
                }
            }
            ch => result.push(ch),
        }
    }
    result.push(quote);
    result
}

pub(super) fn render(hits: &[TableHit], json: bool) -> Result<String> {
    let output = if json {
        serde_json::to_string(hits)?
    } else {
        let mut output = String::new();
        for hit in hits {
            writeln!(
                output,
                "{} [#{}] ({}x{}) {}\n    cols: {}",
                hit.note,
                hit.table_idx,
                hit.n_rows,
                hit.n_cols,
                section_repr(&hit.section_heading),
                hit.headers.join(", ")
            )?;
            ensure!(
                output.len() <= MAX_OUTPUT,
                "notes table output exceeds 1 MiB; narrow the filter or limit"
            );
        }
        output
    };
    ensure!(
        output.len() <= MAX_OUTPUT,
        "notes table output exceeds 1 MiB; narrow the filter or limit"
    );
    Ok(output)
}

pub(super) fn run(args: &TablesArgs) -> Result<u8> {
    let path = resolve_db_path(&args.host, args.db.as_deref())?;
    let conn = open_search(&path)?
        .with_context(|| format!("notes database not found at {}", path.display()))?;
    let hits = table_notes(&conn, &args.filter.join(" "), args.limit)?;
    let output = render(&hits, args.json)?;
    if args.json {
        println!("{output}");
    } else {
        print!("{output}");
    }
    Ok(0)
}
