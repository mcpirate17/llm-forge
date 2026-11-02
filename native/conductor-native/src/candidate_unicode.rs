//! Locale-independent candidate path casefold and Python 3.12-style repr.
//! Both use Unicode 15.0.0; Rust UTF-8 strings cannot contain lone surrogates.
//! This is not Unicode normalization or a claim about newer Python UCD versions.
#[path = "candidate_unicode_table.rs"]
mod table;
use anyhow::{ensure, Result};
use std::fmt::Write;
use unicode_general_category::{get_general_category, GeneralCategory};

pub const UNICODE_VERSION: (u64, u64, u64) = (15, 0, 0);
pub const MAX_PATH_BYTES: usize = 4096;

/// Unicode 15 default full case fold, using C+F mappings and excluding Turkic T.
pub fn casefold(path: &str) -> Result<String> {
    ensure!(
        path.len() <= MAX_PATH_BYTES,
        "candidate path exceeds 4096 bytes"
    );
    if path.is_ascii() {
        return Ok(path.to_ascii_lowercase());
    }
    let mut out = String::with_capacity(path.len());
    for ch in path.chars() {
        if ch.is_ascii() {
            out.push(ch.to_ascii_lowercase());
            continue;
        }
        match table::CASE_FOLD.binary_search_by_key(&(ch as u32), |entry| entry.0) {
            Ok(index) => out.push_str(table::CASE_FOLD[index].1),
            Err(_) => out.push(ch),
        }
    }
    Ok(out)
}
/// CPython 3.12 str repr for valid UTF-8, including quote choice and escapes.
pub fn python_repr(path: &str) -> Result<String> {
    ensure!(
        path.len() <= MAX_PATH_BYTES,
        "candidate path exceeds 4096 bytes"
    );
    Ok(python_repr_bounded(path))
}
/// Shared CPython repr writer for inputs bounded by the caller.
pub(crate) fn python_repr_bounded(path: &str) -> String {
    let quote = if path.contains('\'') && !path.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut output = String::with_capacity(path.len() + 2);
    output.push(quote);
    for ch in path.chars() {
        append_repr(&mut output, ch, quote);
    }
    output.push(quote);
    output
}
fn append_repr(out: &mut String, ch: char, quote: char) {
    match ch {
        '\\' => out.push_str("\\\\"),
        '\n' => out.push_str("\\n"),
        '\r' => out.push_str("\\r"),
        '\t' => out.push_str("\\t"),
        ch if ch == quote => {
            out.push('\\');
            out.push(ch);
        }
        ch if ch != ' ' && !printable(ch) => {
            let n = ch as u32;
            if n <= 0xff {
                write!(out, "\\x{n:02x}").unwrap();
            } else if n <= 0xffff {
                write!(out, "\\u{n:04x}").unwrap();
            } else {
                write!(out, "\\U{n:08x}").unwrap();
            }
        }
        ch => out.push(ch),
    }
}
fn printable(ch: char) -> bool {
    !matches!(
        get_general_category(ch),
        GeneralCategory::Control
            | GeneralCategory::Format
            | GeneralCategory::PrivateUse
            | GeneralCategory::Surrogate
            | GeneralCategory::Unassigned
            | GeneralCategory::SpaceSeparator
            | GeneralCategory::LineSeparator
            | GeneralCategory::ParagraphSeparator
    )
}
#[cfg(test)]
#[path = "candidate_unicode_tests.rs"]
mod tests;
