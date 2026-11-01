//! Markdown extraction and legacy JSON encoding.
use anyhow::Result;
use regex::Regex;
use std::{path::Path, sync::OnceLock};

fn sep_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\s*\|?[\s:|-]*-[\s:|-]*\|?\s*$").unwrap())
}

fn heading_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\s{0,3}(#{1,6})\s+(.*?)\s*#*\s*$").unwrap())
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

pub(super) fn to_python_json<T: serde::Serialize>(value: &T) -> Result<String> {
    let mut buf = Vec::new();
    let mut ser = serde_json::Serializer::with_formatter(&mut buf, PySeparatorFormatter);
    value.serialize(&mut ser)?;
    let utf8 = String::from_utf8(buf)?;
    let mut ascii = String::with_capacity(utf8.len());
    for ch in utf8.chars() {
        if ch.is_ascii() && ch != '\u{7f}' {
            ascii.push(ch);
        } else {
            for unit in ch.encode_utf16(&mut [0; 2]) {
                use std::fmt::Write;
                write!(ascii, "\\u{unit:04x}")?;
            }
        }
    }
    Ok(ascii)
}

/// One markdown table extracted from a note, `serde_json`-encoded the same
/// shape the Python `note_tables.headers_json`/`rows_json` columns hold.
pub(super) struct Table {
    pub(super) section: String,
    pub(super) table_idx: i64,
    pub(super) headers: Vec<String>,
    pub(super) rows: Vec<Vec<String>>,
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
pub(super) fn extract_tables(text: &str) -> Vec<Table> {
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

pub(super) fn title_of(text: &str, path: &Path) -> String {
    for line in text.lines() {
        if let Some(caps) = heading_re().captures(line) {
            return caps.get(2).unwrap().as_str().trim().to_string();
        }
    }
    path.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default()
}
